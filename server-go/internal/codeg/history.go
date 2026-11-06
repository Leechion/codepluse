package codeg

import (
	"encoding/json"
	"fmt"
	"strings"
)

// Codeg reconstructs historical delegation bindings from persisted child rows,
// so the view-session link survives a frontend refresh and backend restart.
func (s *Store) turnsWithDelegationsLocked(conversation int) []Row {
	turns := clone(s.Turns[fmt.Sprint(conversation)])
	if turns == nil {
		return []Row{}
	}
	for _, turn := range turns {
		blocks, _ := turn["blocks"].([]any)
		for _, raw := range blocks {
			block, _ := raw.(map[string]any)
			if block["type"] == "tool_result" {
				block["output_preview"] = toolOutputText(text(block["output_preview"]))
			}
		}
	}
	for _, child := range s.Conversations {
		if number(child["parent_id"]) != conversation {
			continue
		}
		toolID := text(child["parent_tool_use_id"])
		if toolID == "" {
			continue
		}
		status := "running"
		switch child["status"] {
		case "pending_review", "completed":
			status = "completed"
		case "cancelled":
			status = "failed"
		}
		for _, turn := range turns {
			blocks, _ := turn["blocks"].([]any)
			for _, raw := range blocks {
				block, _ := raw.(map[string]any)
				if block["type"] != "tool_use" || block["tool_use_id"] != toolID {
					continue
				}
				meta, _ := block["meta"].(map[string]any)
				if meta == nil {
					meta = Row{}
					block["meta"] = meta
				}
				meta["codeg.delegation"] = Row{"status": status, "child_conversation_id": child["id"], "agent_type": child["agent_type"], "task_id": child["delegation_call_id"], "task_preview": child["title"]}
			}
		}
	}
	return turns
}

// Keep the original stream order when promoting a native turn to history.
func persistedBlocks(live Row, tools []Row, fallback string) []Row {
	blocks := []Row{}
	content, _ := live["content"].([]Row)
	for _, block := range content {
		switch block["kind"] {
		case "text", "thinking":
			blocks = append(blocks, Row{"type": block["kind"], "text": block["text"]})
		case "tool_call_ref":
			for _, tool := range tools {
				if tool["tool_call_id"] != block["tool_call_id"] {
					continue
				}
				blocks = append(blocks, Row{"type": "tool_use", "tool_use_id": tool["tool_call_id"], "tool_name": tool["title"], "input_preview": tool["raw_input"], "status": tool["status"], "meta": tool["meta"]})
				output := tool["raw_output"]
				if output == nil {
					output = tool["content"]
				}
				if output != nil {
					blocks = append(blocks, Row{"type": "tool_result", "tool_use_id": tool["tool_call_id"], "output_preview": toolOutputText(text(output)), "is_error": tool["status"] == "failed"})
				}
			}
		}
	}
	if len(blocks) == 0 {
		blocks = append(blocks, Row{"type": "text", "text": fallback})
	}
	return blocks
}

// ACP adapters can wrap MCP text in content arrays. History stores the text
// shown by the tool, matching the upstream parser, rather than escaped frames.
func toolOutputText(raw string) string {
	var value any
	if json.Unmarshal([]byte(raw), &value) != nil {
		return raw
	}
	var extract func(any) []string
	extract = func(value any) []string {
		switch v := value.(type) {
		case []any:
			out := []string{}
			for _, item := range v {
				out = append(out, extract(item)...)
			}
			return out
		case map[string]any:
			if str, ok := v["text"].(string); ok {
				return []string{str}
			}
			if nested := v["content"]; nested != nil {
				return extract(nested)
			}
		}
		return nil
	}
	if texts := extract(value); len(texts) > 0 {
		return strings.Join(texts, "\n")
	}
	return raw
}

func toolStateWire(tools []Row) []Row {
	out := []Row{}
	for _, tool := range tools {
		var input any
		_ = json.Unmarshal([]byte(text(tool["raw_input"])), &input)
		var output any
		if raw := text(tool["raw_output"]); raw != "" {
			output = Row{"kind": "text", "content": raw}
		}
		out = append(out, Row{"id": tool["tool_call_id"], "kind": tool["kind"], "label": tool["title"], "status": tool["status"], "input": input, "output": output, "content": tool["content"], "locations": tool["locations"], "meta": tool["meta"]})
	}
	return out
}

func (s *Store) regularFoldersLocked() []Row {
	out := []Row{}
	for _, f := range s.Folders {
		if f["kind"] != "chat" {
			out = append(out, clone(f))
		}
	}
	return out
}

func (s *Store) statsLocked() Row {
	messages := 0
	counts := map[string]int{}
	for _, row := range s.Conversations {
		messages += number(row["message_count"])
		counts[text(row["agent_type"])]++
	}
	byAgent := []Row{}
	for agent, count := range counts {
		byAgent = append(byAgent, Row{"agent_type": agent, "conversation_count": count})
	}
	return Row{"total_conversations": len(s.Conversations), "total_messages": messages, "by_agent": byAgent}
}
