package extra

import (
	"bytes"
	"encoding/json"
	"fmt"
	"net/http"
	"time"

	"harnesshub/internal/hub"
)

// Notifier is the terminal-mission webhook notifier, following codeg's
// notification stance: only main-thread completions and errors are pushed,
// never per-worker noise. One silent retry; notification failures never
// affect the orchestration flow.
type Notifier struct {
	URL string
	Log func(level, source, text string)
}

// NewNotifier subscribes to mission events and starts the push goroutine.
func NewNotifier(h *hub.Hub, url string, log func(level, source, text string)) *Notifier {
	n := &Notifier{URL: url, Log: log}
	if url == "" || h == nil {
		return n
	}
	go func() {
		_, events := h.Subscribe()
		for evt := range events {
			raw, ok := evt.Data.(json.RawMessage)
			if !ok {
				continue
			}
			var m struct {
				Status string `json:"status"`
			}
			if json.Unmarshal(raw, &m) != nil {
				continue
			}
			if m.Status != "completed" && m.Status != "partial" && m.Status != "failed" {
				continue
			}
			n.Push(n.Payload(raw))
		}
	}()
	return n
}

// Payload builds the webhook body from a mission snapshot.
func (n *Notifier) Payload(raw json.RawMessage) map[string]any {
	var m struct {
		ID         string   `json:"id"`
		ChatID     string   `json:"chatId"`
		Status     string   `json:"status"`
		Objective  string   `json:"objective"`
		Reply      string   `json:"reply"`
		Error      string   `json:"error"`
		WorkerIDs  []string `json:"workerIds"`
		FinishedAt int64    `json:"finishedAt"`
	}
	_ = json.Unmarshal(raw, &m)
	reply := m.Reply
	if reply == "" {
		reply = m.Error
	}
	return map[string]any{"type": "mission.finished", "missionId": m.ID, "chatId": m.ChatID, "status": m.Status,
		"objective": short(m.Objective, 2000), "reply": short(reply, 8000),
		"error": m.Error, "workerIds": m.WorkerIDs,
		"finishedAt": map[bool]int64{true: m.FinishedAt, false: time.Now().UnixMilli()}[m.FinishedAt > 0]}
}

// Push posts the payload once, with one silent retry.
func (n *Notifier) Push(body map[string]any) bool {
	return n.push(body, 0)
}

func (n *Notifier) push(body map[string]any, attempt int) bool {
	if n.URL == "" {
		return false
	}
	data, err := json.Marshal(body)
	if err != nil {
		return false
	}
	client := &http.Client{Timeout: 10 * time.Second}
	resp, err := client.Post(n.URL, "application/json; charset=utf-8", bytes.NewReader(data))
	if err != nil {
		if attempt == 0 {
			return n.push(body, 1)
		}
		return false
	}
	defer resp.Body.Close()
	if resp.StatusCode >= 200 && resp.StatusCode < 300 {
		return true
	}
	if attempt == 0 {
		return n.push(body, 1)
	}
	if n.Log != nil {
		n.Log("warn", "通知", fmt.Sprintf("Webhook 返回 %d", resp.StatusCode))
	}
	return false
}
