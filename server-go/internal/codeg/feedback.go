package codeg

import (
	"fmt"
	"strings"
)

func (s *Server) submitFeedback(a Row) (any, error) {
	cid, body := text(a["connectionId"]), strings.TrimSpace(text(a["text"]))
	if body == "" || len(body) > 12000 {
		return nil, fmt.Errorf("反馈需要 1–12000 字节")
	}
	if blocks, ok := a["blocks"].([]any); ok && len(blocks) > 0 {
		return nil, fmt.Errorf("Go 实时反馈目前只接受文本")
	}
	note := Row{"id": id(), "text": body, "status": "pending", "created_at": timestamp()}
	s.mu.Lock()
	c := s.connections[cid]
	if c == nil || !c.Busy {
		s.mu.Unlock()
		return nil, fmt.Errorf("no_active_turn")
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
