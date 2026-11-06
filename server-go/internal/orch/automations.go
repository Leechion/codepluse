// Automations: a saved chat prompt that runs on a cron schedule — the Go port
// of core/automations.js. An automation is nothing more than "send this
// message to that chat as if the user typed it", so orchestration, permissions,
// audit and SSE all come for free through the ChatController. Cron is a
// self-contained 5-field parser (minute hour day-of-month month day-of-week).
package orch

import (
	"encoding/json"
	"fmt"
	"os"
	"path/filepath"
	"strings"
	"sync"
	"time"
)

var fieldRanges = [][2]int{{0, 59}, {0, 23}, {1, 31}, {1, 12}, {0, 6}}

const maxScanMinutes = 366 * 24 * 60

func parseField(spec string, index int) (map[int]bool, error) {
	min, max := fieldRanges[index][0], fieldRanges[index][1]
	values := map[int]bool{}
	for _, part := range strings.Split(spec, ",") {
		if part == "" {
			return nil, fmt.Errorf("cron 字段无效：%s", spec)
		}
		rangeRaw, stepRaw := part, ""
		if i := strings.Index(part, "/"); i >= 0 {
			rangeRaw, stepRaw = part[:i], part[i+1:]
		}
		step := 1
		if stepRaw != "" {
			if _, err := fmt.Sscanf(stepRaw, "%d", &step); err != nil || step < 1 {
				return nil, fmt.Errorf("cron 步长无效：%s", part)
			}
		}
		lo, hi := 0, 0
		switch {
		case rangeRaw == "*":
			lo, hi = min, max
		case strings.Contains(rangeRaw, "-"):
			if _, err := fmt.Sscanf(rangeRaw, "%d-%d", &lo, &hi); err != nil {
				return nil, fmt.Errorf("cron 取值无效：%s（字段范围 %d-%d）", part, min, max)
			}
		default:
			if _, err := fmt.Sscanf(rangeRaw, "%d", &lo); err != nil {
				return nil, fmt.Errorf("cron 取值无效：%s（字段范围 %d-%d）", part, min, max)
			}
			hi = lo
		}
		if lo < min || hi > max || lo > hi {
			return nil, fmt.Errorf("cron 取值无效：%s（字段范围 %d-%d）", part, min, max)
		}
		for v := lo; v <= hi; v += step {
			values[v] = true
		}
	}
	return values, nil
}

// ParseCron parses a 5-field cron expression into field value sets.
func ParseCron(cron string) ([]map[int]bool, error) {
	fields := strings.Fields(cron)
	if len(fields) != 5 {
		return nil, fmt.Errorf("cron 需要 5 个字段：分 时 日 月 周")
	}
	parsed := make([]map[int]bool, 5)
	for i, f := range fields {
		v, err := parseField(f, i)
		if err != nil {
			return nil, err
		}
		parsed[i] = v
	}
	if parsed[4][7] {
		parsed[4][0] = true // 7 means Sunday, like the crontab convention
	}
	return parsed, nil
}

// CronMatches reports whether the cron hits the given local time.
func CronMatches(cron []map[int]bool, t time.Time) bool {
	return cron[0][t.Minute()] && cron[1][t.Hour()] &&
		cron[2][t.Day()] && cron[3][int(t.Month())] && cron[4][int(t.Weekday())]
}

// NextRun scans forward one minute at a time (bounded by a year).
func NextRun(cron []map[int]bool, from time.Time) (time.Time, error) {
	cursor := from.Truncate(time.Minute)
	for i := 0; i < maxScanMinutes; i++ {
		cursor = cursor.Add(time.Minute)
		if CronMatches(cron, cursor) {
			return cursor, nil
		}
	}
	return time.Time{}, fmt.Errorf("cron 在一年内没有任何匹配时间")
}

// AutomationRecord is one saved schedule.
type AutomationRecord struct {
	ID            string `json:"id"`
	ChatID        string `json:"chatId"`
	Prompt        string `json:"prompt"`
	Cron          string `json:"cron"`
	Enabled       bool   `json:"enabled"`
	CreatedAt     int64  `json:"createdAt"`
	LastRunAt     int64  `json:"lastRunAt"`
	LastStatus    string `json:"lastStatus"`
	NextRunAt     int64  `json:"nextRunAt"`
	LastMinuteKey int64  `json:"-"`
}

// Automations owns the schedule file and the tick loop.
type Automations struct {
	Chats *ChatController
	File  string
	Log   func(level, source, text string)

	mu      sync.Mutex
	records []*AutomationRecord
}

// NewAutomations loads the persisted records.
func NewAutomations(chats *ChatController, file string, log func(level, source, text string)) *Automations {
	a := &Automations{Chats: chats, File: file, Log: log}
	a.Load()
	return a
}

// Load reads the records file (BOM-tolerant, like the Node version).
func (a *Automations) Load() {
	if a.File == "" || a.File == ":memory:" {
		return
	}
	data, err := os.ReadFile(a.File)
	if err != nil {
		return
	}
	text := strings.TrimPrefix(string(data), "\uFEFF")
	var saved []*AutomationRecord
	if json.Unmarshal([]byte(text), &saved) != nil {
		if a.Log != nil {
			a.Log("error", "定时任务", "读取配置失败：格式无效")
		}
		return
	}
	a.mu.Lock()
	a.records = saved
	a.mu.Unlock()
}

// Save atomically rewrites the records file.
func (a *Automations) Save() {
	if a.File == "" || a.File == ":memory:" {
		return
	}
	a.mu.Lock()
	data, _ := json.Marshal(a.records)
	a.mu.Unlock()
	_ = os.MkdirAll(filepath.Dir(a.File), 0o755)
	if os.WriteFile(a.File+".tmp", data, 0o644) == nil {
		_ = os.Rename(a.File+".tmp", a.File)
	}
}

// Records returns a snapshot of the schedule list.
func (a *Automations) Records() []*AutomationRecord {
	a.mu.Lock()
	defer a.mu.Unlock()
	return append([]*AutomationRecord{}, a.records...)
}

// ValidateCron parses and returns the next run time.
func (a *Automations) ValidateCron(cron string) (int64, error) {
	parsed, err := ParseCron(cron)
	if err != nil {
		return 0, err
	}
	next, err := NextRun(parsed, time.Now())
	if err != nil {
		return 0, err
	}
	return next.UnixMilli(), nil
}

// Create adds a schedule.
func (a *Automations) Create(spec map[string]any) (*AutomationRecord, error) {
	prompt, _ := spec["prompt"].(string)
	trimmed := strings.TrimSpace(prompt)
	if trimmed == "" || len(prompt) > 12000 {
		return nil, fmt.Errorf("请填写 1–12000 字符的定时提示")
	}
	chatID, _ := spec["chatId"].(string)
	if _, err := a.Chats.Get(chatID); err != nil {
		return nil, err
	}
	cronRaw, _ := spec["cron"].(string)
	cron := strings.TrimSpace(cronRaw)
	nextAt, err := a.ValidateCron(cron)
	if err != nil {
		return nil, err
	}
	rec := &AutomationRecord{ID: "auto_" + newOrchID(), ChatID: chatID, Prompt: trimmed,
		Cron: cron, Enabled: true, CreatedAt: nowMs(), NextRunAt: nextAt}
	a.mu.Lock()
	a.records = append(a.records, rec)
	a.mu.Unlock()
	a.Save()
	return rec, nil
}

// Update patches one record.
func (a *Automations) Update(id string, patch map[string]any) (*AutomationRecord, error) {
	a.mu.Lock()
	var rec *AutomationRecord
	for _, r := range a.records {
		if r.ID == id {
			rec = r
		}
	}
	a.mu.Unlock()
	if rec == nil {
		return nil, fmt.Errorf("定时任务不存在")
	}
	if raw, ok := patch["prompt"].(string); ok && raw != "" {
		trimmed := strings.TrimSpace(raw)
		if trimmed == "" || len(raw) > 12000 {
			return nil, fmt.Errorf("请填写 1–12000 字符的定时提示")
		}
		rec.Prompt = trimmed
	}
	if raw, ok := patch["cron"].(string); ok && raw != "" {
		nextAt, err := a.ValidateCron(strings.TrimSpace(raw))
		if err != nil {
			return nil, err
		}
		rec.Cron = strings.TrimSpace(raw)
		rec.NextRunAt = nextAt
	}
	if raw, ok := patch["enabled"].(bool); ok {
		rec.Enabled = raw
	}
	a.Save()
	return rec, nil
}

// Remove deletes one record.
func (a *Automations) Remove(id string) error {
	a.mu.Lock()
	before := len(a.records)
	filtered := a.records[:0]
	for _, r := range a.records {
		if r.ID != id {
			filtered = append(filtered, r)
		}
	}
	a.records = filtered
	a.mu.Unlock()
	if len(a.records) == before {
		return fmt.Errorf("定时任务不存在")
	}
	a.Save()
	return nil
}

// Tick fires due schedules; safe to call every ~30s from the server loop.
func (a *Automations) Tick() {
	now := time.Now()
	minuteKey := now.Unix() / 60
	a.mu.Lock()
	records := append([]*AutomationRecord{}, a.records...)
	a.mu.Unlock()
	for _, rec := range records {
		if !rec.Enabled {
			continue
		}
		func() {
			defer a.Save()
			parsed, err := ParseCron(rec.Cron)
			if err != nil {
				rec.LastStatus = "error: " + err.Error()
				return
			}
			if rec.NextRunAt == 0 {
				next, err := NextRun(parsed, now)
				if err != nil {
					rec.LastStatus = "error: " + err.Error()
					return
				}
				rec.NextRunAt = next.UnixMilli()
			}
			if now.UnixMilli() < rec.NextRunAt {
				return
			}
			next, err := NextRun(parsed, now)
			if err != nil {
				rec.LastStatus = "error: " + err.Error()
				return
			}
			rec.NextRunAt = next.UnixMilli()
			if rec.LastMinuteKey == minuteKey {
				return
			}
			rec.LastMinuteKey = minuteKey
			chat, err := a.Chats.Get(rec.ChatID)
			if err != nil {
				rec.LastStatus = "missing-chat"
				return
			}
			if a.Chats.active(chat) != nil {
				rec.LastStatus = "busy"
				return
			}
			rec.LastRunAt = now.UnixMilli()
			rec.LastStatus = "sent"
			if _, err := a.Chats.Send(rec.ChatID, rec.Prompt, nil, nil); err != nil {
				rec.LastStatus = "error: " + err.Error()
				return
			}
			if a.Log != nil {
				a.Log("info", "定时任务", "已按计划触发："+rec.ID)
			}
		}()
	}
}
