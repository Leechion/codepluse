// Package mcpserver implements the hub-side MCP servers spawned as
// subcommands of the main Go binary. Result is the structured-result sink —
// the counterpart of codeg's `submit_result`.
package mcpserver

import (
	"bufio"
	"encoding/json"
	"fmt"
	"io"
	"os"
)

const (
	maxRefused = 5
	maxErrors  = 8
)

type schema map[string]any

func typeOf(value any) string {
	switch value.(type) {
	case []any:
		return "array"
	case nil:
		return "null"
	case bool:
		return "boolean"
	case float64, int:
		return "number"
	case string:
		return "string"
	case map[string]any:
		return "object"
	}
	return "unknown"
}

func validateAgainst(value any, sch schema, path string, errors *[]string) {
	if len(*errors) >= maxErrors {
		return
	}
	where := path
	if where == "" {
		where = "提交参数"
	}
	if enum, ok := sch["enum"].([]any); ok {
		matched := false
		for _, e := range enum {
			if fmt.Sprint(e) == fmt.Sprint(value) {
				matched = true
			}
		}
		if !matched {
			*errors = append(*errors, where+" 必须是 "+fmt.Sprint(enum))
		}
		return
	}
	expected, _ := sch["type"].(string)
	if expected != "" && typeOf(value) != expected {
		names := map[string]string{"string": "字符串", "boolean": "布尔值", "array": "数组", "object": "对象", "number": "数字"}
		name := names[expected]
		if name == "" {
			name = expected
		}
		*errors = append(*errors, where+" 必须是"+name+"，收到 "+typeOf(value))
		return
	}
	if expected == "object" {
		obj, _ := value.(map[string]any)
		for _, key := range toStringSlice(sch["required"]) {
			if _, ok := obj[key]; !ok {
				*errors = append(*errors, where+"缺少必填字段 "+key)
			}
		}
		props, _ := sch["properties"].(map[string]any)
		for key, child := range props {
			if v, ok := obj[key]; ok {
				if cs, ok := child.(map[string]any); ok {
					sub := path
					if sub != "" {
						sub += "." + key
					} else {
						sub = key
					}
					validateAgainst(v, cs, sub, errors)
				}
			}
		}
		return
	}
	if expected == "array" {
		arr, ok := value.([]any)
		if !ok {
			return
		}
		items, _ := sch["items"].(map[string]any)
		if items == nil {
			return
		}
		n := len(arr)
		if n > 20 {
			n = 20
		}
		for i := 0; i < n; i++ {
			validateAgainst(arr[i], items, fmt.Sprintf("%s[%d]", orDefault(path, "提交参数"), i), errors)
		}
	}
}

func orDefault(v, def string) string {
	if v == "" {
		return def
	}
	return v
}

func toStringSlice(v any) []string {
	raw, ok := v.([]any)
	if !ok {
		return nil
	}
	var out []string
	for _, item := range raw {
		if s, ok := item.(string); ok {
			out = append(out, s)
		}
	}
	return out
}

// Sink validates submit_result calls against the stage schema; a refused
// submission carries what to fix, and after maxRefused refusals it lets
// submissions through so a stubborn model cannot loop forever (the hub-side
// validation keeps the final say).
type Sink struct {
	schema  schema
	refused int
}

// NewSink builds a sink for the given schema (nil schema accepts anything).
func NewSink(raw json.RawMessage) *Sink {
	s := &Sink{}
	if len(raw) > 0 {
		var sch schema
		if json.Unmarshal(raw, &sch) == nil && sch["type"] == "object" {
			s.schema = sch
		}
	}
	return s
}

// Call executes one submit_result invocation.
func (s *Sink) Call(args map[string]any) (map[string]any, error) {
	if len(args) == 0 {
		return nil, fmt.Errorf("请把约定的 JSON 字段作为工具参数对象提交")
	}
	if s.schema != nil {
		var errors []string
		validateAgainst(args, s.schema, "", &errors)
		if len(errors) > 0 {
			if len(errors) > maxErrors {
				errors = errors[:maxErrors]
			}
			if s.refused >= maxRefused {
				return map[string]any{"received": true, "note": "已多次拒绝，本次放行，由服务端最终校验"}, nil
			}
			s.refused++
			return nil, fmt.Errorf("提交未通过 schema 校验，请修正后重新调用：%s", strings_join(errors, "；"))
		}
	}
	return map[string]any{"received": true}, nil
}

func strings_join(items []string, sep string) string {
	out := ""
	for i, s := range items {
		if i > 0 {
			out += sep
		}
		out += s
	}
	return out
}

const toolDescription = "向 Harness Hub 提交本回合的结构化结果。把要求的 JSON 的每个字段直接作为工具参数传入。若提交被拒绝，按错误说明修正后重新调用；只在被明确要求提交结构化结果时调用。"

// RunResult serves the stdio MCP protocol; schemaJSON comes from argv.
func RunResult(schemaJSON string, stdin io.Reader, stdout io.Writer) error {
	sink := NewSink(json.RawMessage(schemaJSON))
	inputSchema := map[string]any{"type": "object"}
	if sink.schema != nil {
		inputSchema = sink.schema
	}
	tools := []map[string]any{{
		"name":        "submit_result",
		"description": toolDescription,
		"inputSchema": inputSchema,
	}}
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
		var result any
		ok := true
		switch req.Method {
		case "initialize":
			result = map[string]any{"protocolVersion": "2024-11-05", "capabilities": map[string]any{"tools": map[string]any{}},
				"serverInfo": map[string]any{"name": "harness-result", "version": "1.1.0"}}
		case "ping":
			result = map[string]any{}
		case "tools/list":
			result = map[string]any{"tools": tools}
		case "tools/call":
			payload, err := sink.Call(req.Params.Arguments)
			if err != nil {
				result = map[string]any{"isError": true, "content": []map[string]any{{"type": "text", "text": err.Error()}}}
			} else {
				text, _ := json.Marshal(payload)
				result = map[string]any{"content": []map[string]any{{"type": "text", "text": string(text)}}}
			}
		default:
			writeJSON(stdout, map[string]any{"jsonrpc": "2.0", "id": req.ID,
				"error": map[string]any{"code": -32601, "message": "Method not found"}})
			continue
		}
		if ok {
			writeJSON(stdout, map[string]any{"jsonrpc": "2.0", "id": req.ID, "result": result})
		}
	}
	return scanner.Err()
}

func writeJSON(w io.Writer, v any) {
	b, err := json.Marshal(v)
	if err != nil {
		return
	}
	w.Write(append(b, '\n'))
}

// MainResult is the entry point for the `mcp-result` subcommand.
func MainResult(args []string) int {
	schemaJSON := ""
	if len(args) > 0 {
		schemaJSON = args[0]
	}
	if err := RunResult(schemaJSON, os.Stdin, os.Stdout); err != nil {
		fmt.Fprintln(os.Stderr, err)
		return 1
	}
	return 0
}
