package codeg

import (
	"bufio"
	"bytes"
	"context"
	"encoding/json"
	"fmt"
	"io"
	"net/http"
	"os"
	"sync"
)

func ToolDefinitions() []Row {
	property := func(kind, description string) Row { return Row{"type": kind, "description": description} }
	tool := func(name, description string, required []string, props Row) Row {
		return Row{"name": name, "description": description, "inputSchema": Row{"type": "object", "required": required, "properties": props}}
	}
	return []Row{
		tool("list_agents", "列出当前真正连接、可用于委派的 Harness。", []string{}, Row{}),
		tool("delegate_to_agent", "异步委派完整、自包含的子任务，立即返回 task_id。子代理没有你的对话上下文。可多次调用并行分工，再查询状态收集结果。用户点名 Harness 时使用指定目标。", []string{"agent_type", "task"}, Row{"agent_type": property("string", "list_agents 返回的 agent_type"), "task": property("string", "完整任务、相关路径、约束、预期结果"), "working_dir": property("string", "可选工作目录，默认继承父会话"), "model": property("string", "可选，目标 Harness 支持的模型 ID")}),
		tool("get_delegation_status", "收集一个或多个子任务的真实结果。省略 wait_ms 为即时快照，正数最多等待 60000ms，0 等待到完成或需要用户授权。任务耗时正常时继续等待，不要假称完成。", []string{"task_ids"}, Row{"task_ids": Row{"type": "array", "items": Row{"type": "string"}, "minItems": 1}, "wait_ms": Row{"type": "integer", "minimum": 0}}),
		tool("cancel_delegation", "仅当不再需要结果时停止子任务；已经结束的任务返回原结果。", []string{"task_id"}, Row{"task_id": property("string", "delegate_to_agent 返回的 task_id")}),
		tool("check_user_feedback", "在关键决策和阶段切换时读取新消息及本团队任务分工，及时响应用户和同僚。", []string{}, Row{}),
		tool("send_agent_message", "向同一团队的子任务发送消息；对方通过 check_user_feedback 收取。发送确认只代表入箱，不代表对方已经读取。", []string{"task_id", "message"}, Row{"task_id": property("string", "同一团队的目标任务 ID"), "message": property("string", "消息正文")}),
	}
}

// MainCompanion is a per-native-session stdio MCP process. Only stdout carries
// protocol messages; network requests are authenticated to that caller alone.
func MainCompanion(args []string) int {
	if len(args) != 2 {
		fmt.Fprintln(os.Stderr, "mcp-codeg requires endpoint and token")
		return 2
	}
	return ServeCompanion(os.Stdin, os.Stdout, args[0], args[1])
}
func ServeCompanion(in io.Reader, out io.Writer, endpoint, token string) int {
	ctx, cancel := context.WithCancel(context.Background())
	defer cancel()
	var writeMu sync.Mutex
	var calls sync.WaitGroup
	write := func(id any, value any, err error) {
		message := Row{"jsonrpc": "2.0", "id": id}
		if err != nil {
			message["error"] = Row{"code": -32603, "message": err.Error()}
		} else {
			message["result"] = value
		}
		writeMu.Lock()
		defer writeMu.Unlock()
		_ = json.NewEncoder(out).Encode(message)
	}
	scanner := bufio.NewScanner(in)
	scanner.Buffer(make([]byte, 4096), 2<<20)
	for scanner.Scan() {
		var req struct {
			ID     any    `json:"id"`
			Method string `json:"method"`
			Params Row    `json:"params"`
		}
		if json.Unmarshal(scanner.Bytes(), &req) != nil {
			continue
		}
		if req.ID == nil {
			continue
		}
		switch req.Method {
		case "initialize":
			write(req.ID, Row{"protocolVersion": "2024-11-05", "capabilities": Row{"tools": Row{}}, "serverInfo": Row{"name": "codeg-mcp-go", "version": "0.1.0"}}, nil)
		case "ping":
			write(req.ID, Row{}, nil)
		case "tools/list":
			write(req.ID, Row{"tools": ToolDefinitions()}, nil)
		case "tools/call":
			calls.Add(1)
			go func(requestID any, params Row) {
				defer calls.Done()
				body, _ := json.Marshal(params)
				httpReq, err := http.NewRequestWithContext(ctx, "POST", endpoint, bytes.NewReader(body))
				if err != nil {
					write(requestID, nil, err)
					return
				}
				httpReq.Header.Set("Content-Type", "application/json")
				httpReq.Header.Set("Authorization", "Bearer "+token)
				res, err := http.DefaultClient.Do(httpReq)
				if err != nil {
					write(requestID, nil, err)
					return
				}
				defer res.Body.Close()
				data, err := io.ReadAll(io.LimitReader(res.Body, 2<<20))
				if err != nil {
					write(requestID, nil, err)
					return
				}
				result := Row{"content": []Row{{"type": "text", "text": string(data)}}}
				if res.StatusCode != 200 {
					result["isError"] = true
				}
				write(requestID, result, nil)
			}(req.ID, req.Params)
		default:
			write(req.ID, nil, fmt.Errorf("unsupported MCP method: %s", req.Method))
		}
	}
	cancel()
	calls.Wait()
	if scanner.Err() != nil {
		return 1
	}
	return 0
}
