package codeg

import (
	"fmt"
	"os"
	"strings"
)

func (s *Server) submitFeedback(a Row) (any, error) {
	cid, body := text(a["connectionId"]), strings.TrimSpace(text(a["text"]))
	if body == "" || len(body) > 12000 {
		return nil, fmt.Errorf("反馈需要 1–12000 字节")
	}
	note := Row{"id": id(), "text": body, "status": "pending", "created_at": timestamp()}
	s.mu.Lock()
	c := s.connections[cid]
	if c == nil || !c.Busy {
		s.mu.Unlock()
		// The composer recognizes this marker and retains the draft by sending
		// it through the normal queue when the current turn just ended.
		return nil, fmt.Errorf("no active turn")
	}
	runtimeID, conversationID := c.RuntimeID, c.ConversationID
	s.mu.Unlock()
	if s.Runtime.SupportsNativeSteering(runtimeID) {
		wire := []map[string]any{}
		stored := []Row{}
		if blocks, ok := a["blocks"].([]any); ok && len(blocks) > 0 {
			for _, raw := range blocks {
				block, _ := raw.(map[string]any)
				switch text(block["type"]) {
				case "text":
					wire = append(wire, map[string]any{"type": "text", "text": text(block["text"])})
					stored = append(stored, Row{"type": "text", "text": text(block["text"])})
				case "image":
					mime := text(block["mime_type"])
					if !s.Runtime.PromptCapabilities(runtimeID)["image"] || !strings.HasPrefix(mime, "image/") {
						return nil, fmt.Errorf("当前执行端不支持此图片")
					}
					data, err := s.imageData(block)
					if err != nil {
						return nil, err
					}
					wire = append(wire, map[string]any{"type": "image", "mimeType": mime, "data": data})
					stored = append(stored, Row{"type": "image", "mime_type": mime, "data": data, "uri": block["uri"]})
				case "resource_link":
					link, display, err := resourceLink(block)
					if err != nil {
						return nil, err
					}
					wire = append(wire, link)
					stored = append(stored, display)
				case "resource":
					if !s.Runtime.PromptCapabilities(runtimeID)["embedded_context"] {
						return nil, fmt.Errorf("当前执行端不支持嵌入资源")
					}
					resource, display, err := s.embeddedResource(block)
					if err != nil {
						return nil, err
					}
					wire = append(wire, resource)
					stored = append(stored, display)
				default:
					return nil, fmt.Errorf("当前引导不支持此附件类型")
				}
			}
		} else {
			wire = append(wire, map[string]any{"type": "text", "text": body})
			stored = append(stored, Row{"type": "text", "text": body})
		}
		// Serialize with this connection's completion without holding the
		// shared store lock across a request to the agent.
		c.completionMu.Lock()
		defer c.completionMu.Unlock()
		err := s.Runtime.Steer(runtimeID, wire)
		if err != nil {
			return nil, err
		}
		s.store.mu.Lock()
		row := s.store.conversationLocked(conversationID)
		if row != nil {
			if _, err := s.store.appendTurnLocked(conversationID, "user", body, stored); err != nil {
				fmt.Fprintln(os.Stderr, "[codeg-store] 保存用户反馈失败：", err)
			}
			_ = s.store.saveLocked()
		}
		s.store.mu.Unlock()
		at := timestamp()
		note["status"], note["delivered_at"] = "delivered", at
		s.mu.Lock()
		if s.connections[cid] == c {
			c.Feedback = append(c.Feedback, note)
		}
		s.mu.Unlock()
		s.event(cid, Row{"type": "feedback_submitted", "item": note})
		return clone(note), nil
	}
	if blocks, ok := a["blocks"].([]any); ok && len(blocks) > 0 {
		return nil, fmt.Errorf("no active turn")
	}
	s.mu.Lock()
	if s.connections[cid] != c || !c.Busy {
		s.mu.Unlock()
		return nil, fmt.Errorf("no active turn")
	}
	c.Feedback = append(c.Feedback, note)
	s.mu.Unlock()
	s.event(cid, Row{"type": "feedback_submitted", "item": note})
	return clone(note), nil
}
func (s *Server) consumeFeedback(cid string) []Row {
	s.mu.Lock()
	c := s.connections[cid]
	messages := []Row{}
	ids := []string{}
	at := timestamp()
	if c != nil {
		for _, note := range c.Feedback {
			if note["status"] == "pending" {
				messages = append(messages, clone(note))
				ids = append(ids, text(note["id"]))
				note["status"] = "delivered"
				note["delivered_at"] = at
			}
		}
	}
	s.mu.Unlock()
	if len(ids) > 0 {
		s.event(cid, Row{"type": "feedback_consumed", "ids": ids, "delivered_at": at})
	}
	return messages
}
