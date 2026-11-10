package codeg

import (
	"crypto/sha256"
	"encoding/base64"
	"encoding/hex"
	"fmt"
	"os"
	"os/exec"
	"path/filepath"
	"sort"
	"strings"
)

var ignoredDirs = map[string]bool{"node_modules": true, ".git": true, ".next": true, ".data": true, "dist": true, "target": true}

// readableRoots 返回允许读写的根：服务根 + 所有已打开的工作空间文件夹。
// 外部文件夹被「添加为工作空间」后必须能读写，否则选了也打不开文件。
func (s *Server) readableRoots() []string {
	roots := []string{s.Root}
	if s.store != nil {
		s.store.mu.Lock()
		for _, f := range s.store.Folders {
			if p := text(f["path"]); p != "" {
				roots = append(roots, p)
			}
		}
		s.store.mu.Unlock()
	}
	return roots
}

func withinRoot(base, target string) bool {
	rel, err := filepath.Rel(base, target)
	if err != nil || filepath.IsAbs(rel) || rel == ".." {
		return false
	}
	return !strings.HasPrefix(rel, ".."+string(filepath.Separator))
}

func (s *Server) resolveFile(root, input string) (string, error) {
	if root == "" {
		root = s.Root
	}
	if !filepath.IsAbs(input) {
		input = filepath.Join(root, input)
	}
	resolved, err := s.Resolve(input)
	if err != nil {
		return "", err
	}
	for _, base := range s.readableRoots() {
		b, err := s.Resolve(base)
		if err != nil {
			continue
		}
		if withinRoot(b, resolved) {
			return resolved, nil
		}
	}
	return "", fmt.Errorf("文件超出工作区目录")
}
func (s *Server) files(command string, a Row) (any, error) {
	root := text(a["rootPath"])
	input := text(a["path"])
	if command == "get_home_directory" {
		return s.Resolve("")
	}
	var path string
	var err error
	if command == "list_directory_entries" || command == "list_directory_with_files" {
		// 目录浏览是只读的，放开到任意路径：要能把服务根之外的文件夹
		// 添加为工作空间，先得能看见并进得去。
		target := input
		if target == "" {
			target = root
		}
		if target == "" {
			target = s.Root
		}
		if !filepath.IsAbs(target) {
			target = filepath.Join(s.Root, target)
		}
		path, err = s.Resolve(target)
	} else {
		path, err = s.resolveFile(root, input)
	}
	if err != nil {
		return nil, err
	}
	switch command {
	case "list_directory_entries", "list_directory_with_files":
		entries, err := os.ReadDir(path)
		if err != nil {
			return nil, err
		}
		out := []Row{}
		for _, e := range entries {
			if e.Type()&os.ModeSymlink != 0 {
				continue
			}
			if !e.IsDir() && command == "list_directory_entries" {
				continue
			}
			p := filepath.Join(path, e.Name())
			children := false
			if e.IsDir() {
				list, _ := os.ReadDir(p)
				for _, child := range list {
					if child.IsDir() {
						children = true
						break
					}
				}
			}
			row := Row{"name": e.Name(), "path": p, "hasChildren": children}
			if command == "list_directory_with_files" {
				row["isDir"] = e.IsDir()
				if info, err := e.Info(); err == nil {
					row["size"] = info.Size()
				}
			}
			out = append(out, row)
		}
		return out, nil
	case "get_file_tree", "list_workspace_files":
		flat := []Row{}
		maxDepth := number(a["maxDepth"])
		if maxDepth <= 0 {
			maxDepth = 8
		}
		if maxDepth > 32 {
			maxDepth = 32
		}
		var tree func(string, int) ([]Row, error)
		tree = func(dir string, depth int) ([]Row, error) {
			entries, err := os.ReadDir(dir)
			if err != nil {
				return nil, err
			}
			out := []Row{}
			sort.SliceStable(entries, func(i, j int) bool { return entries[i].IsDir() && !entries[j].IsDir() })
			for _, e := range entries {
				if e.Type()&os.ModeSymlink != 0 || e.IsDir() && ignoredDirs[e.Name()] {
					continue
				}
				full := filepath.Join(dir, e.Name())
				rel, _ := filepath.Rel(path, full)
				kind := "file"
				if e.IsDir() {
					kind = "dir"
				}
				flat = append(flat, Row{"name": e.Name(), "path": filepath.ToSlash(rel), "kind": kind})
				node := Row{"kind": kind, "name": e.Name(), "path": filepath.ToSlash(rel)}
				if e.IsDir() {
					children := []Row{}
					if depth < maxDepth || command == "list_workspace_files" {
						children, err = tree(full, depth+1)
						if err != nil {
							return nil, err
						}
					}
					node["children"] = children
				}
				out = append(out, node)
			}
			return out, nil
		}
		nodes, err := tree(path, 1)
		if err != nil {
			return nil, err
		}
		if command == "list_workspace_files" {
			return flat, nil
		}
		return nodes, nil
	case "read_file_preview", "read_file_for_edit", "read_file_base64", "read_workspace_file_base64", "save_file_content", "save_file_copy":
		info, err := os.Stat(path)
		exists := err == nil
		if err != nil && !os.IsNotExist(err) {
			return nil, err
		}
		if exists && (!info.Mode().IsRegular() || info.Size() > 8<<20) {
			return nil, fmt.Errorf("仅支持小于 8 MiB 的普通文件")
		}
		data := []byte{}
		if exists {
			data, err = os.ReadFile(path)
			if err != nil {
				return nil, err
			}
		}
		hash := func(b []byte) string { h := sha256.Sum256(b); return hex.EncodeToString(h[:]) }
		if strings.HasPrefix(command, "save_") {
			expected := text(a["expectedEtag"])
			if expected != "" && expected != hash(data) {
				return nil, fmt.Errorf("file_conflict: 文件已被其他程序修改，请重新读取后再保存")
			}
			if command == "save_file_copy" && exists {
				return nil, fmt.Errorf("目标文件已存在")
			}
			data = []byte(text(a["content"]))
			if len(data) > 8<<20 {
				return nil, fmt.Errorf("文件超过 8 MiB")
			}
			if err = os.WriteFile(path, data, 0644); err != nil {
				return nil, err
			}
			info, err = os.Stat(path)
			if err != nil {
				return nil, err
			}
			exists = true
		} else if !exists {
			return nil, os.ErrNotExist
		}
		if strings.Contains(command, "base64") {
			return base64.StdEncoding.EncodeToString(data), nil
		}
		if command == "read_file_preview" {
			return Row{"path": path, "content": string(data)}, nil
		}
		ending := "none"
		if strings.Contains(string(data), "\n") {
			ending = "lf"
			if strings.Contains(string(data), "\r\n") {
				ending = "crlf"
				if strings.Contains(strings.ReplaceAll(string(data), "\r\n", ""), "\n") {
					ending = "mixed"
				}
			}
		}
		result := Row{"path": path, "etag": hash(data), "mtime_ms": info.ModTime().UnixMilli(), "readonly": info.Mode().Perm()&0200 == 0, "line_ending": ending}
		if command == "read_file_for_edit" {
			result["content"] = string(data)
		}
		return result, nil
	case "get_git_head":
		data, err := exec.Command("git", "-C", path, "rev-parse", "--abbrev-ref", "HEAD").Output()
		if err != nil {
			return nil, nil
		}
		return strings.TrimSpace(string(data)), nil
	}
	return nil, fmt.Errorf("文件操作未实现：%s", command)
}

// Kept separate from native fs callbacks: editor writes carry a user-facing
// ETag and do not turn an agent's read-only session into a writable session.
