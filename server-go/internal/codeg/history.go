package codeg

import (
	"encoding/json"
	"fmt"
	"strings"
	"time"
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
		if text(block["parent_tool_use_id"]) != "" { continue }
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

// ── turn-window pagination (get_folder_conversation_turns) ──
//
// The frontend page (codeg-ui ConversationTurnsPage) expects one page of older
// history ending just before `beforeIndex`: full[turns_offset .. turns_offset +
// len(turns)), plus the structural prefix fingerprint of full[0..turns_offset)
// and of the page's end boundary, so the client can verify the join.

const fnvSeed uint64 = 0xcbf29ce484222325
const fnvPrime uint64 = 0x100000001b3

// prefixFingerprintLocked mirrors the client's extendPrefixFingerprint
// (src/lib/turn-window.ts) and the Rust turn_window::prefix_fingerprint:
// FNV-1a 64 over, per turn, the role tag byte (user=0, assistant=1, system=2)
// then the timestamp epoch-millis as 8 little-endian bytes. Id-free.
func prefixFingerprint(turns []Row, seed uint64) string {
	hash := seed
	for _, turn := range turns {
		var tag byte
		switch text(turn["role"]) {
		case "user":
			tag = 0
		case "assistant":
			tag = 1
		case "system":
			tag = 2
		default:
			tag = 3
		}
		hash ^= uint64(tag)
		hash *= fnvPrime
		millis := int64(0)
		if ts, err := time.Parse(time.RFC3339Nano, text(turn["timestamp"])); err == nil {
			millis = ts.UnixMilli()
		}
		v := uint64(millis)
		for i := 0; i < 8; i++ {
			hash ^= v & 0xff
			hash *= fnvPrime
			v >>= 8
		}
	}
	return fmt.Sprintf("%016x", hash)
}

// uncoveredPrefixMaxTsLocked returns the max RFC3339 timestamp in
// turns[0..end), or nil when the prefix is empty.
func uncoveredPrefixMaxTs(turns []Row) any {
	var maxTs time.Time
	found := false
	for _, turn := range turns {
		ts, err := time.Parse(time.RFC3339Nano, text(turn["timestamp"]))
		if err != nil {
			continue
		}
		if !found || ts.After(maxTs) {
			maxTs = ts
			found = true
		}
	}
	if !found {
		return nil
	}
	return maxTs.UTC().Format(time.RFC3339Nano)
}

// conversationTurnsPageLocked builds one page of older history for
// get_folder_conversation_turns. The page ends just before `beforeIndex`
// (clamped to [0, total]) and holds at most `limit` turns.
func (s *Store) conversationTurnsPageLocked(conversation int, beforeIndex, limit int) Row {
	all := s.Turns[fmt.Sprint(conversation)]
	if all == nil {
		all = []Row{}
	}
	total := len(all)
	end := beforeIndex
	if end < 0 {
		end = 0
	}
	if end > total {
		end = total
	}
	start := end - limit
	if start < 0 {
		start = 0
	}
	page := []Row{}
	for _, turn := range all[start:end] {
		page = append(page, clone(turn))
	}
	prefix := all[:start]
	assistantBefore := 0
	userBefore := 0
	for _, turn := range prefix {
		switch text(turn["role"]) {
		case "assistant":
			assistantBefore++
		case "user":
			userBefore++
		}
	}
	boundary := beforeIndex
	if boundary < 0 {
		boundary = 0
	}
	if boundary > total {
		boundary = total
	}
	return Row{
		"turns":                        page,
		"turns_offset":                 start,
		"turns_total":                  total,
		"assistant_turns_before_offset": assistantBefore,
		"user_turns_before_offset":      userBefore,
		"prefix_hash":                  prefixFingerprint(prefix, fnvSeed),
		"prefix_hash_before_index":      prefixFingerprint(all[:boundary], fnvSeed),
		"uncovered_prefix_max_ts":       uncoveredPrefixMaxTs(prefix),
	}
}
