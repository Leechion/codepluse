package codeg

import (
	"encoding/base64"
	"fmt"
	"io"
	"net/http"
	"net/url"
	"os"
	"path/filepath"
	"runtime"
	"strings"
)

const maxAttachmentBytes = 20 * 1024 * 1024

// resourceLink prepares an ACP resource link and a text representation that
// the existing transcript renderer can restore as a clickable reference.
func resourceLink(block map[string]any) (map[string]any, Row, error) {
	uri := strings.TrimSpace(text(block["uri"]))
	name := strings.TrimSpace(text(block["name"]))
	if uri == "" || name == "" || strings.ContainsAny(uri, "\r\n") {
		return nil, nil, fmt.Errorf("资源引用无效")
	}
	wire := map[string]any{"type": "resource_link", "uri": uri, "name": name}
	if mime := text(block["mime_type"]); mime != "" {
		wire["mimeType"] = mime
	}
	if description := text(block["description"]); description != "" {
		wire["description"] = description
	}
	label := strings.ReplaceAll(strings.ReplaceAll(name, "[", "\\["), "]", "\\]")
	target := strings.ReplaceAll(strings.ReplaceAll(uri, "(", "%28"), ")", "%29")
	stored := Row{"type": "resource_link", "uri": uri, "name": name}
	if mime := text(block["mime_type"]); mime != "" {
		stored["mime_type"] = mime
	}
	if description := text(block["description"]); description != "" {
		stored["description"] = description
	}
	return wire, Row{"type": "text", "text": "[" + label + "](" + target + ")", "source_prompt_block": stored}, nil
}

// embeddedResource translates the composer's resource shape into ACP's nested
// resource content and keeps enough display data for a restored conversation.
func (s *Server) embeddedResource(block map[string]any) (map[string]any, Row, error) {
	uri := strings.TrimSpace(text(block["uri"]))
	if uri == "" || strings.ContainsAny(uri, "\r\n") {
		return nil, nil, fmt.Errorf("嵌入资源地址无效")
	}
	mime := text(block["mime_type"])
	resource := map[string]any{"uri": uri}
	if mime != "" {
		resource["mimeType"] = mime
	}
	var display Row
	if payload, ok := block["text"].(string); ok {
		if len(payload) > maxAttachmentBytes {
			return nil, nil, fmt.Errorf("嵌入资源过大")
		}
		resource["text"] = payload
	} else if blob, ok := block["blob"].(string); ok {
		data, err := s.imageData(map[string]any{"data": blob, "uri": uri})
		if err != nil {
			return nil, nil, err
		}
		resource["blob"] = data
		if strings.HasPrefix(mime, "image/") {
			display = Row{"type": "image", "data": data, "mime_type": mime, "uri": uri}
		}
	} else {
		return nil, nil, fmt.Errorf("嵌入资源缺少内容")
	}
	if display == nil {
		name := filepath.Base(strings.TrimRight(uri, "/"))
		_, display, _ = resourceLink(map[string]any{"uri": uri, "name": name})
		if display == nil {
			return nil, nil, fmt.Errorf("嵌入资源名称无效")
		}
	}
	stored := Row{"type": "resource", "uri": uri}
	if mime != "" {
		stored["mime_type"] = mime
	}
	if payload, ok := resource["text"]; ok {
		stored["text"] = payload
	}
	// An image's bytes are already held in its display block's data field.
	if blob, ok := resource["blob"]; ok && display["type"] != "image" {
		stored["blob"] = blob
	}
	display["source_prompt_block"] = stored
	return map[string]any{"type": "resource", "resource": resource}, display, nil
}

func (s *Server) imageData(block map[string]any) (string, error) {
	data := text(block["data"])
	if data != "" {
		decoded, err := base64.StdEncoding.DecodeString(data)
		if err != nil || len(decoded) == 0 || len(decoded) > maxAttachmentBytes {
			return "", fmt.Errorf("图片数据无效或过大")
		}
		return data, nil
	}
	u, err := url.Parse(text(block["uri"]))
	if err != nil || u.Scheme != "file" || (u.Host != "" && u.Host != "localhost") {
		return "", fmt.Errorf("图片上传地址无效")
	}
	path := u.Path
	if runtime.GOOS == "windows" && len(path) >= 3 && path[0] == '/' && path[2] == ':' {
		path = path[1:]
	}
	path = filepath.FromSlash(path)
	root, err := filepath.Abs(filepath.Join(s.Root, ".data", "uploads"))
	if err != nil {
		return "", err
	}
	path, err = filepath.Abs(path)
	if err != nil {
		return "", err
	}
	root, err = filepath.EvalSymlinks(root)
	if err != nil {
		return "", err
	}
	path, err = filepath.EvalSymlinks(path)
	if err != nil {
		return "", err
	}
	rel, err := filepath.Rel(root, path)
	if err != nil || rel == ".." || strings.HasPrefix(rel, ".."+string(filepath.Separator)) || filepath.IsAbs(rel) {
		return "", fmt.Errorf("图片不在上传目录中")
	}
	file, err := os.Open(path)
	if err != nil {
		return "", err
	}
	defer file.Close()
	bytes, err := io.ReadAll(io.LimitReader(file, maxAttachmentBytes+1))
	if err != nil || len(bytes) == 0 || len(bytes) > maxAttachmentBytes {
		return "", fmt.Errorf("图片为空或过大")
	}
	return base64.StdEncoding.EncodeToString(bytes), nil
}

// uploadAttachment stores files on the agent host and returns an absolute
// path that the composer can include as a workspace resource.
func (s *Server) uploadAttachment(w http.ResponseWriter, r *http.Request) {
	if r.Method != http.MethodPost {
		respond(w, http.StatusMethodNotAllowed, Row{"code": "invalid_input", "message": "Use POST"})
		return
	}
	r.Body = http.MaxBytesReader(w, r.Body, maxAttachmentBytes+1024*1024)
	file, header, err := r.FormFile("file")
	if err != nil {
		respond(w, http.StatusBadRequest, Row{"code": "invalid_input", "message": "无法读取附件"})
		return
	}
	defer file.Close()
	if r.MultipartForm != nil {
		defer r.MultipartForm.RemoveAll()
	}
	name := filepath.Base(strings.ReplaceAll(header.Filename, "\\", "/"))
	if name == "" || name == "." || name == ".." {
		respond(w, http.StatusBadRequest, Row{"code": "invalid_input", "message": "附件名称无效"})
		return
	}
	dir, err := filepath.Abs(filepath.Join(s.Root, ".data", "uploads", id()))
	if err != nil {
		respond(w, http.StatusInternalServerError, Row{"code": "upload_failed", "message": err.Error()})
		return
	}
	if err = os.MkdirAll(dir, 0700); err != nil {
		respond(w, http.StatusInternalServerError, Row{"code": "upload_failed", "message": err.Error()})
		return
	}
	path := filepath.Join(dir, name)
	out, err := os.OpenFile(path, os.O_WRONLY|os.O_CREATE|os.O_EXCL, 0600)
	if err != nil {
		respond(w, http.StatusInternalServerError, Row{"code": "upload_failed", "message": err.Error()})
		return
	}
	size, copyErr := io.Copy(out, io.LimitReader(file, maxAttachmentBytes+1))
	closeErr := out.Close()
	if copyErr != nil || closeErr != nil || size == 0 || size > maxAttachmentBytes {
		_ = os.Remove(path)
		_ = os.Remove(dir)
		respond(w, http.StatusBadRequest, Row{"code": "invalid_input", "message": "附件为空、过大或读取失败"})
		return
	}
	mime := header.Header.Get("Content-Type")
	respond(w, http.StatusOK, Row{"path": path, "name": name, "size": size, "mimeType": mime})
}
