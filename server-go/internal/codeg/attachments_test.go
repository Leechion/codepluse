package codeg

import (
	"bytes"
	"encoding/base64"
	"encoding/json"
	"mime/multipart"
	"net/http/httptest"
	"net/url"
	"os"
	"path/filepath"
	"testing"
	"time"

	"harnesshub/internal/acp"
	"harnesshub/internal/hub"
)

func TestImagePromptHydratesUploadedImageAndPersistsHistory(t *testing.T) {
	s := testServer(t)
	launch := s.Runtime.LaunchFn
	s.Runtime.LaunchFn = func(agent *hub.Agent, permission, cwd string) (*acp.Launch, error) {
		v, err := launch(agent, permission, cwd)
		if err == nil {
			v.Args = append(v.Args, "--images")
		}
		return v, err
	}
	cid := command(t, s, "acp_connect", Row{"agentType": "codex"}).(string)
	caps := s.snapshot(cid)["prompt_capabilities"].(map[string]bool)
	if !caps["image"] {
		t.Fatal("image capability not reported")
	}
	dir := filepath.Join(s.Root, ".data", "uploads", "test")
	if err := os.MkdirAll(dir, 0700); err != nil {
		t.Fatal(err)
	}
	path := filepath.Join(dir, "test image.png")
	image := []byte("image bytes")
	if err := os.WriteFile(path, image, 0600); err != nil {
		t.Fatal(err)
	}
	uriPath := filepath.ToSlash(path)
	if len(uriPath) > 1 && uriPath[1] == ':' {
		uriPath = "/" + uriPath
	}
	uri := (&url.URL{Scheme: "file", Path: uriPath}).String()
	other := base64.StdEncoding.EncodeToString([]byte("second image"))
	if err := s.prompt(Row{"connectionId": cid, "blocks": []any{Row{"type": "text", "text": "describe"}, Row{"type": "image", "mime_type": "image/png", "data": "", "uri": uri}, Row{"type": "text", "text": "then"}, Row{"type": "image", "mime_type": "image/jpeg", "data": other}}}); err != nil {
		t.Fatal(err)
	}
	deadline := time.Now().Add(5 * time.Second)
	for {
		s.mu.Lock()
		busy, conversation := s.connections[cid].Busy, s.connections[cid].ConversationID
		s.mu.Unlock()
		if !busy {
			s.store.mu.Lock()
			turns := s.store.turnsLocked(conversation)
			s.store.mu.Unlock()
			encoded, _ := json.Marshal(turns)
			if len(turns) != 2 || !bytes.Contains(encoded, []byte("image:image/png:"+base64.StdEncoding.EncodeToString(image)+"|text:then|image:image/jpeg:"+other)) || !bytes.Contains(encoded, []byte(`"mime_type":"image/png"`)) {
				t.Fatalf("missing image in prompt or history: %s", encoded)
			}
			restored := reloadStore(t, s.store.file)
			persisted, _ := json.Marshal(restored.turnsLocked(conversation))
			if !bytes.Contains(persisted, []byte(other)) {
				t.Fatalf("second image lost after reload: %s", persisted)
			}
			break
		}
		if time.Now().After(deadline) {
			t.Fatal("prompt did not finish")
		}
		time.Sleep(10 * time.Millisecond)
	}
}

func TestImagePromptRejectsUnsupportedAgentAndOutsideUploadDirectory(t *testing.T) {
	s := testServer(t)
	cid := command(t, s, "acp_connect", Row{"agentType": "codex"}).(string)
	if caps := s.snapshot(cid)["prompt_capabilities"].(map[string]bool); caps["image"] {
		t.Fatal("undeclared image capability was enabled")
	}
	err := s.prompt(Row{"connectionId": cid, "blocks": []any{Row{"type": "image", "mime_type": "image/png", "data": "YWJj"}}})
	if err == nil {
		t.Fatal("unsupported image was accepted")
	}
	s.mu.Lock()
	busy := s.connections[cid].Busy
	s.mu.Unlock()
	if busy {
		t.Fatal("rejected image left connection busy")
	}
	out := filepath.ToSlash(filepath.Join(s.Root, "private.png"))
	if len(out) > 1 && out[1] == ':' {
		out = "/" + out
	}
	uri := (&url.URL{Scheme: "file", Path: out}).String()
	if _, err := s.imageData(map[string]any{"uri": uri}); err == nil {
		t.Fatal("accepted file outside uploads")
	}
}

func TestResourceLinkIsSentAndRestored(t *testing.T) {
	s := testServer(t)
	cid := command(t, s, "acp_connect", Row{"agentType": "codex"}).(string)
	blocks := []any{Row{"type": "text", "text": "Read this"}, Row{"type": "resource_link", "name": "spec", "uri": "https://example.test/spec", "mime_type": "text/plain"}}
	if err := s.prompt(Row{"connectionId": cid, "blocks": blocks}); err != nil {
		t.Fatal(err)
	}
	deadline := time.Now().Add(3 * time.Second)
	for {
		s.mu.Lock()
		busy, conv := s.connections[cid].Busy, s.connections[cid].ConversationID
		s.mu.Unlock()
		if !busy {
			restored := reloadStore(t, s.store.file)
			turns := restored.turnsLocked(conv)
			if len(turns) != 2 {
				t.Fatalf("unexpected turns: %v", turns)
			}
			user, _ := json.Marshal(turns[0])
			assistant, _ := json.Marshal(turns[1])
			if !bytes.Contains(user, []byte("[spec](https://example.test/spec)")) || !bytes.Contains(assistant, []byte("resource:spec:https://example.test/spec:text/plain")) {
				t.Fatalf("resource did not roundtrip: %s %s", user, assistant)
			}
			userBlocks := turns[0]["blocks"].([]any)
			source := userBlocks[1].(map[string]any)["source_prompt_block"].(map[string]any)
			if source["type"] != "resource_link" || source["mime_type"] != "text/plain" {
				t.Fatalf("source link lost: %v", source)
			}
			return
		}
		if time.Now().After(deadline) {
			t.Fatal("turn did not finish")
		}
		time.Sleep(10 * time.Millisecond)
	}
}

func TestEmbeddedResourcesReachAgentAndRestore(t *testing.T) {
	s := testServer(t)
	launch := s.Runtime.LaunchFn
	s.Runtime.LaunchFn = func(a *hub.Agent, permission, cwd string) (*acp.Launch, error) {
		v, err := launch(a, permission, cwd)
		if err == nil {
			v.Args = append(v.Args, "--embedded")
		}
		return v, err
	}
	cid := command(t, s, "acp_connect", Row{"agentType": "codex"}).(string)
	caps := s.snapshot(cid)["prompt_capabilities"].(map[string]bool)
	if !caps["embedded_context"] || caps["image"] {
		t.Fatalf("wrong capabilities: %v", caps)
	}
	image := base64.StdEncoding.EncodeToString([]byte("embedded image"))
	blocks := []any{Row{"type": "text", "text": "check"}, Row{"type": "resource", "uri": "memory://note", "mime_type": "text/plain", "text": "note body"}, Row{"type": "resource", "uri": "memory://picture.png", "mime_type": "image/png", "blob": image}}
	if err := s.prompt(Row{"connectionId": cid, "blocks": blocks}); err != nil {
		t.Fatal(err)
	}
	deadline := time.Now().Add(3 * time.Second)
	for {
		s.mu.Lock()
		busy, conv := s.connections[cid].Busy, s.connections[cid].ConversationID
		s.mu.Unlock()
		if !busy {
			restored := reloadStore(t, s.store.file)
			turns := restored.turnsLocked(conv)
			if len(turns) != 2 {
				t.Fatalf("unexpected turns: %v", turns)
			}
			user, _ := json.Marshal(turns[0])
			assistant, _ := json.Marshal(turns[1])
			if !bytes.Contains(user, []byte(`"mime_type":"image/png"`)) || !bytes.Contains(user, []byte(image)) || !bytes.Contains(assistant, []byte("embedded:memory://note:text/plain:note body")) || !bytes.Contains(assistant, []byte("embedded:memory://picture.png:image/png:"+image)) {
				t.Fatalf("missing embedded data: %s %s", user, assistant)
			}
			userBlocks := turns[0]["blocks"].([]any)
			original := userBlocks[1].(map[string]any)["source_prompt_block"].(map[string]any)
			imageBlock := userBlocks[2].(map[string]any)
			imageOriginal := imageBlock["source_prompt_block"].(map[string]any)
			if original["type"] != "resource" || original["text"] != "note body" || imageOriginal["type"] != "resource" || imageBlock["data"] != image {
				t.Fatalf("embedded source lost: %v %v", original, imageBlock)
			}
			return
		}
		if time.Now().After(deadline) {
			t.Fatal("turn did not finish")
		}
		time.Sleep(10 * time.Millisecond)
	}
}

func TestUploadAttachmentStoresBytesOnAgentHost(t *testing.T) {
	s := testServer(t)
	var body bytes.Buffer
	m := multipart.NewWriter(&body)
	file, err := m.CreateFormFile("file", "notes.txt")
	if err != nil {
		t.Fatal(err)
	}
	_, _ = file.Write([]byte("attachment content"))
	_ = m.Close()
	r := httptest.NewRequest("POST", "/api/upload_attachment", &body)
	r.Header.Set("Content-Type", m.FormDataContentType())
	w := httptest.NewRecorder()
	s.ServeHTTP(w, r)
	if w.Code != 200 {
		t.Fatalf("upload status %d: %s", w.Code, w.Body.String())
	}
	var result struct {
		Path, Name string
		Size       int64
	}
	if err := json.Unmarshal(w.Body.Bytes(), &result); err != nil {
		t.Fatal(err)
	}
	data, err := os.ReadFile(result.Path)
	if err != nil || string(data) != "attachment content" || result.Name != "notes.txt" || result.Size != int64(len(data)) {
		t.Fatalf("wrong upload result: %+v, %q, %v", result, data, err)
	}
	if rel, err := filepath.Rel(filepath.Join(s.Root, ".data", "uploads"), result.Path); err != nil || filepath.IsAbs(rel) || rel == ".." {
		t.Fatalf("upload outside data directory: %q, %v", result.Path, err)
	}
	// The composer serializes file badges into text. Verify that the exact
	// uploaded path reaches an ACP agent and remains in the conversation.
	cid := command(t, s, "acp_connect", Row{"agentType": "codex"}).(string)
	prompt := "Please read [notes.txt](file:///" + filepath.ToSlash(result.Path) + ")"
	if err := s.prompt(Row{"connectionId": cid, "blocks": []any{Row{"type": "text", "text": prompt}}}); err != nil {
		t.Fatal(err)
	}
	deadline := time.Now().Add(5 * time.Second)
	for {
		s.mu.Lock()
		busy, conversation := s.connections[cid].Busy, s.connections[cid].ConversationID
		s.mu.Unlock()
		if !busy {
			s.store.mu.Lock()
			turns := s.store.turnsLocked(conversation)
			s.store.mu.Unlock()
			if len(turns) < 2 {
				t.Fatalf("agent did not produce an answer: %v", turns)
			}
			encoded, _ := json.Marshal(turns[len(turns)-1])
			if !bytes.Contains(encoded, []byte(filepath.ToSlash(result.Path))) {
				t.Fatalf("uploaded path did not reach the agent: %s", encoded)
			}
			break
		}
		if time.Now().After(deadline) {
			t.Fatal("agent did not finish")
		}
		time.Sleep(10 * time.Millisecond)
	}
}
