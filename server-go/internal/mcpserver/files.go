package mcpserver

import (
	"bufio"
	"encoding/json"
	"fmt"
	"io"
	"os"
	"path/filepath"
	"strings"
)

// Files is the read-only workspace MCP stdio server. Both lexical paths and
// symlink targets must stay inside the explicitly supplied workspace. No
// command or write tools exist.
type Files struct {
	root string
}

// NewFiles resolves the workspace root.
func NewFiles(root string) (*Files, error) {
	real, err := filepath.EvalSymlinks(root)
	if err != nil {
		return nil, err
	}
	return &Files{root: real}, nil
}

func (f *Files) resolve(input string) (string, error) {
	if input == "" {
		input = "."
	}
	candidate := filepath.Join(f.root, input)
	real, err := filepath.EvalSymlinks(candidate)
	if err != nil {
		return "", err
	}
	rel, err := filepath.Rel(f.root, real)
	if err != nil || rel == ".." || strings.HasPrefix(rel, ".."+string(filepath.Separator)) || filepath.IsAbs(rel) {
		return "", fmt.Errorf("Path is outside the workspace")
	}
	return real, nil
}

// Call executes one tool invocation.
func (f *Files) Call(name string, args map[string]any) (map[string]any, error) {
	pathArg, _ := args["path"].(string)
	file, err := f.resolve(pathArg)
	if err != nil {
		return nil, err
	}
	if name == "list_directory" {
		entries, err := os.ReadDir(file)
		if err != nil {
			return nil, err
		}
		if len(entries) > 500 {
			entries = entries[:500]
		}
		out := make([]map[string]any, 0, len(entries))
		for _, e := range entries {
			out = append(out, map[string]any{"name": e.Name(), "directory": e.IsDir()})
		}
		return map[string]any{"entries": out}, nil
	}
	if name != "read_file" {
		return nil, fmt.Errorf("Unknown tool")
	}
	st, err := os.Stat(file)
	if err != nil {
		return nil, err
	}
	if !st.Mode().IsRegular() {
		return nil, fmt.Errorf("Not a regular file")
	}
	start := 1
	if v, ok := args["startLine"].(float64); ok && v >= 1 {
		start = int(v)
	}
	count := 200
	if v, ok := args["lineCount"].(float64); ok {
		count = int(v)
		if count < 1 {
			count = 1
		}
		if count > 400 {
			count = 400
		}
	}
	// Never read an unbounded file into the model context.
	fd, err := os.Open(file)
	if err != nil {
		return nil, err
	}
	defer fd.Close()
	buf := make([]byte, 256*1024)
	n, _ := fd.Read(buf)
	lines := splitFileLines(string(buf[:n]))
	var text []string
	end := start - 1 + count
	for i := start - 1; i < end && i < len(lines); i++ {
		text = append(text, fmt.Sprintf("%d: %s", i+1, lines[i]))
	}
	return map[string]any{"path": relPath(f.root, file), "truncated": int64(n) < st.Size(),
		"text": strings.Join(text, "\n")}, nil
}

func relPath(root, file string) string {
	rel, err := filepath.Rel(root, file)
	if err != nil {
		return file
	}
	return rel
}

func splitFileLines(s string) []string {
	raw := strings.Split(s, "\n")
	out := make([]string, len(raw))
	for i, l := range raw {
		out[i] = strings.TrimSuffix(l, "\r")
	}
	return out
}

var fileTools = []map[string]any{
	{"name": "read_file", "description": "Read workspace file with line numbers. Use this instead of shell commands.",
		"inputSchema": map[string]any{"type": "object", "properties": map[string]any{
			"path": map[string]any{"type": "string"}, "startLine": map[string]any{"type": "integer"}, "lineCount": map[string]any{"type": "integer"}},
			"required": []string{"path"}},
		"annotations": map[string]any{"readOnlyHint": true}},
	{"name": "list_directory", "description": "List workspace directory entries.",
		"inputSchema": map[string]any{"type": "object", "properties": map[string]any{"path": map[string]any{"type": "string"}}},
		"annotations": map[string]any{"readOnlyHint": true}},
}

// RunFiles serves the stdio MCP protocol for the workspace-files server.
func RunFiles(root string, stdin io.Reader, stdout io.Writer) error {
	f, err := NewFiles(root)
	if err != nil {
		return err
	}
	scanner := bufio.NewScanner(stdin)
	scanner.Buffer(make([]byte, 0, 1024*1024), 16*1024*1024)
	for scanner.Scan() {
		var req struct {
			ID     *json.Number `json:"id"`
			Method string       `json:"method"`
			Params struct {
				Name      string         `json:"name"`
				Arguments map[string]any `json:"arguments"`
			} `json:"params"`
		}
		if json.Unmarshal(scanner.Bytes(), &req) != nil || req.ID == nil {
			continue
		}
		switch req.Method {
		case "initialize":
			writeJSON(stdout, map[string]any{"jsonrpc": "2.0", "id": req.ID,
				"result": map[string]any{"protocolVersion": "2024-11-05", "capabilities": map[string]any{"tools": map[string]any{}},
					"serverInfo": map[string]any{"name": "harness-workspace-files", "version": "1.0.0"}}})
		case "ping":
			writeJSON(stdout, map[string]any{"jsonrpc": "2.0", "id": req.ID, "result": map[string]any{}})
		case "tools/list":
			writeJSON(stdout, map[string]any{"jsonrpc": "2.0", "id": req.ID, "result": map[string]any{"tools": fileTools}})
		case "tools/call":
			payload, err := f.Call(req.Params.Name, req.Params.Arguments)
			if err != nil {
				writeJSON(stdout, map[string]any{"jsonrpc": "2.0", "id": req.ID,
					"result": map[string]any{"isError": true, "content": []map[string]any{{"type": "text", "text": err.Error()}}}})
			} else {
				text, _ := json.Marshal(payload)
				writeJSON(stdout, map[string]any{"jsonrpc": "2.0", "id": req.ID,
					"result": map[string]any{"content": []map[string]any{{"type": "text", "text": string(text)}}}})
			}
		default:
			writeJSON(stdout, map[string]any{"jsonrpc": "2.0", "id": req.ID,
				"error": map[string]any{"code": -32601, "message": "Method not found"}})
		}
	}
	return scanner.Err()
}

// MainFiles is the entry point for the `mcp-files` subcommand.
func MainFiles(args []string) int {
	if len(args) == 0 {
		fmt.Fprintln(os.Stderr, "mcp-files 需要 workspace 根目录参数")
		return 1
	}
	if err := RunFiles(args[0], os.Stdin, os.Stdout); err != nil {
		fmt.Fprintln(os.Stderr, err)
		return 1
	}
	return 0
}
