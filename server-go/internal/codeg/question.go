package codeg

import (
	"context"
	"fmt"
)

// The codeg-mcp `ask_user_question` tool: the agent asks one or more multiple
// choice questions and its turn BLOCKS until the user answers. Unlike
// propose_task_split (which returns immediately), this parks the companion's
// /api/codeg_mcp HTTP call — the companion keeps that request open, so the
// answer we return here is exactly what the model receives as the tool result.
//
// Lifecycle mirrors task splits: a card is published through the connection's
// event stream (`question_request`) and the snapshot (`pending_question`), and
// cleared on `question_resolved`. A parked question is also released when the
// turn ends or the session disconnects, so a blocked tool call can never leak.

// parkedQuestion is one blocking ask_user_question waiting on the user.
type parkedQuestion struct {
	connID string
	ch     chan Row // carries the QuestionAnswer the user submits
}

// askUserQuestion raises the card and blocks until answered or torn down.
func (s *Server) askUserQuestion(ctx context.Context, caller *Caller, a Row) (any, error) {
	s.mu.Lock()
	parent := s.connections[caller.ConnectionID]
	s.mu.Unlock()
	if parent == nil {
		return nil, fmt.Errorf("父会话已断开")
	}
	questions, _ := a["questions"].([]any)
	if len(questions) == 0 {
		return nil, fmt.Errorf("questions 不能为空")
	}
	qid := id()
	park := &parkedQuestion{connID: parent.ID, ch: make(chan Row, 1)}
	s.mu.Lock()
	s.questions[qid] = park
	parent.PendingQuestion = Row{"question_id": qid, "questions": a["questions"], "created_at": timestamp()}
	s.mu.Unlock()
	s.event(parent.ID, Row{"type": "question_request", "question_id": qid, "questions": a["questions"]})
	s.publishSession(parent.ID)

	select {
	case answer := <-park.ch:
		return questionToolResult(answer), nil
	case <-ctx.Done():
		// The companion/session went away mid-question: settle declined so the
		// card clears everywhere and this goroutine returns.
		s.settleQuestion(qid, declinedAnswer())
		return nil, ctx.Err()
	}
}

// answerQuestion implements `acp_answer_question`. Idempotent: answering an
// unknown or already-settled question is a no-op success, matching the frontend.
func (s *Server) answerQuestion(a Row) {
	s.settleQuestion(text(a["questionId"]), answerArg(a["answer"]))
}

// settleQuestion drops the parked question, clears the card on every client, and
// hands `answer` to the blocked tool call. Safe to call more than once.
func (s *Server) settleQuestion(qid string, answer Row) {
	if qid == "" {
		return
	}
	s.mu.Lock()
	park := s.questions[qid]
	if park == nil {
		s.mu.Unlock()
		return
	}
	delete(s.questions, qid)
	if c := s.connections[park.connID]; c != nil && c.PendingQuestion != nil && text(c.PendingQuestion["question_id"]) == qid {
		c.PendingQuestion = nil
	}
	s.mu.Unlock()
	s.event(park.connID, Row{"type": "question_resolved", "question_id": qid})
	s.publishSession(park.connID)
	select {
	case park.ch <- answer:
	default:
	}
}

// releaseQuestionsForConnection settles every question parked on a connection —
// called when a turn ends (e.g. cancelled) or the session disconnects.
func (s *Server) releaseQuestionsForConnection(cid string) {
	s.mu.Lock()
	ids := []string{}
	for qid, park := range s.questions {
		if park.connID == cid {
			ids = append(ids, qid)
		}
	}
	s.mu.Unlock()
	for _, qid := range ids {
		s.settleQuestion(qid, declinedAnswer())
	}
}

// questionToolResult shapes the user's answer as the MCP tool result the model
// reads back.
func questionToolResult(answer Row) Row {
	if answer == nil {
		return declinedAnswer()
	}
	return Row{"declined": boolOf(answer["declined"]), "answers": answer["answers"]}
}

func declinedAnswer() Row {
	return Row{"declined": true, "answers": []any{}}
}

func answerArg(v any) Row {
	if m, ok := v.(map[string]any); ok {
		return Row(m)
	}
	return declinedAnswer()
}

func boolOf(v any) bool {
	b, _ := v.(bool)
	return b
}
