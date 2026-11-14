// Package codeg ports the Codeg 0.42 web contracts to the Go runtime.
// Frontend source: spacering-net/codeg, Apache-2.0 (codeg-ui/LICENSE).
package codeg

import (
	"crypto/rand"
	"encoding/hex"
	"encoding/json"
	"fmt"
	"math"
	"os"
	"path/filepath"
	"sort"
	"strconv"
	"strings"
	"sync"
	"sync/atomic"
	"time"
)

type Row = map[string]any
type Store struct {
	mu        sync.Mutex
	file      string
	turnMu    sync.Mutex
	turns     *turnDB // opened lazily by turnStore()
	turnsPath string
	closed    bool
	// savedMtime/savedSize record the document this process last read or wrote,
	// so reloadIfChangedLocked can tell whether another writer replaced it.
	savedMtime int64
	savedSize  int64
	Next      int `json:"next"`
	// Turns is populated only while loading a pre-SQLite workspace document.
	// Runtime reads and writes go through the turn store; the field stays nil
	// and is omitted from the JSON document.
	Turns          map[string][]Row `json:"turns,omitempty"`
	Folders        []Row            `json:"folders"`
	Conversations  []Row            `json:"conversations"`
	Settings       map[string]any   `json:"settings"`
	Tabs           []Row            `json:"tabs"`
	TabsVersion    int              `json:"tabsVersion"`
	CanvasBoards   []Row            `json:"canvasBoards"`
	CanvasNodes    []Row            `json:"canvasNodes"`
	CanvasRevision int              `json:"canvasRevision"`
	FolderGroups   []Row            `json:"folderGroups"`
	// P1 sidebar entities: conversation tags, assistants (tombstones kept), and
	// quick messages. Each is a flat list persisted in codeg-workspace.json.
	ConversationTags []Row `json:"conversationTags"`
	Assistants       []Row `json:"assistants"`
	QuickMessages    []Row `json:"quickMessages"`
	// Agents holds the per-agent settings overlay written by the harness
	// settings page (env / native config / enabled / provider binding), keyed by
	// wire agent type. It is deliberately kept in codeg's own store rather than
	// written back to the vendor files (~/.claude, real ~/.codex) so saving a
	// config never mutates the user's global CLI setup; the launch path merges
	// this overlay on top of the ambient environment instead.
	Agents       map[string]Row            `json:"agents"`
	CustomAgents map[string]customAgentDef `json:"customAgents,omitempty"`
}

func timestamp() string { return time.Now().UTC().Format(time.RFC3339Nano) }
func id() string {
	b := make([]byte, 16)
	if _, err := rand.Read(b); err != nil {
		// Previously the error was ignored and the all-zero buffer produced a
		// 32-character id of "0"s, so every subsequent id collided.
		return fmt.Sprintf("%d-%d", time.Now().UnixNano(), atomic.AddUint64(&idFallback, 1))
	}
	return hex.EncodeToString(b)
}

var idFallback uint64
func text(v any) string { s, _ := v.(string); return s }

// optStr 可空字符串字段：缺省/null → nil，否则指向去空白后的值。
func optStr(v any) *string {
	s, ok := v.(string)
	if !ok {
		return nil
	}
	t := strings.TrimSpace(s)
	return &t
}
func number(v any) int {
	switch n := v.(type) {
	case int:
		return n
	case int64:
		return int(n)
	case uint64:
		return int(n)
	case float64:
		// A JSON number can be NaN-free but far outside int range (1e300) or
		// fractional; converting blindly yields MinInt on amd64, which then
		// reaches slice bounds and panics. Clamp like the truncation helpers.
		if math.IsNaN(n) || math.IsInf(n, 0) {
			return 0
		}
		if n >= float64(math.MaxInt) {
			return math.MaxInt
		}
		if n <= float64(math.MinInt) {
			return math.MinInt
		}
		return int(n)
	}
	return 0
}
func clone[T any](value T) T {
	b, err := json.Marshal(value)
	if err != nil {
		// Returning the zero value would silently replace real data with an
		// empty object (e.g. a Row containing NaN); hand back the input.
		return value
	}
	var out T
	if err := json.Unmarshal(b, &out); err != nil {
		return value
	}
	return out
}
func newStore(file string) (*Store, error) {
	s := &Store{file: file, turnsPath: turnDBFile(file), Next: 1, Folders: []Row{}, Conversations: []Row{}, Settings: map[string]any{}, Tabs: []Row{}, FolderGroups: []Row{}, ConversationTags: []Row{}, Assistants: []Row{}, QuickMessages: []Row{}, Agents: map[string]Row{}}
	if file == ":memory:" {
		return s, nil
	}
	b, err := os.ReadFile(file)
	if err == nil {
		if err = json.Unmarshal(b, s); err != nil {
			return nil, err
		}
		s.rememberSavedLocked()
	} else if !os.IsNotExist(err) {
		return nil, err
	}
	for _, folder := range s.Folders {
		if folder["kind"] == "project" {
			folder["kind"] = "regular"
		}
	}
	for _, row := range s.Conversations {
		if row["kind"] == "conversation" {
			row["kind"] = "regular"
			if f := s.folderLocked(number(row["folder_id"])); f != nil && f["kind"] == "chat" {
				row["kind"] = "chat"
			}
			if number(row["parent_id"]) > 0 {
				row["kind"] = "delegate"
			}
		}
	}
	if err := s.importLegacyTurnsLocked(); err != nil {
		return nil, err
	}
	return s, nil
}

// importLegacyTurnsLocked moves the pre-SQLite "turns" map into SQLite and
// rewrites the JSON document without it. Idempotent: a conversation that
// already has rows in the database is skipped.
func (s *Store) importLegacyTurnsLocked() error {
	if len(s.Turns) == 0 {
		return nil
	}
	db, err := s.turnStore()
	if err != nil {
		return err
	}
	backup := fmt.Sprintf("%s.bak-%s", s.file, time.Now().Format("20060102-150405"))
	if data, err := os.ReadFile(s.file); err == nil {
		if err := os.WriteFile(backup, data, 0o600); err != nil {
			return fmt.Errorf("备份旧工作区数据失败：%w", err)
		}
	}
	for key, turns := range s.Turns {
		conv, err := strconv.Atoi(key)
		if err != nil {
			continue
		}
		if n, err := db.count(conv); err == nil && n > 0 {
			continue
		}
		if err := db.importTurns(conv, turns); err != nil {
			return fmt.Errorf("导入会话 %d 的历史失败：%w", conv, err)
		}
	}
	s.Turns = nil
	return s.saveLocked()
}
func (s *Store) saveLocked() error {
	if s.file == ":memory:" {
		return nil
	}
	if err := os.MkdirAll(filepath.Dir(s.file), 0755); err != nil {
		return err
	}
	b, err := json.Marshal(s)
	if err != nil {
		return err
	}
	if err = os.WriteFile(s.file+".tmp", b, 0600); err != nil {
		return err
	}
	if err = os.Rename(s.file+".tmp", s.file); err != nil {
		return err
	}
	s.rememberSavedLocked()
	return nil
}

// rememberSavedLocked snapshots the on-disk identity of the document we just
// read or wrote. Caller holds s.mu (or is still constructing the store).
func (s *Store) rememberSavedLocked() {
	if s.file == ":memory:" {
		return
	}
	info, err := os.Stat(s.file)
	if err != nil {
		return
	}
	s.savedMtime, s.savedSize = info.ModTime().UnixNano(), info.Size()
}

// reloadIfChangedLocked picks up edits made by another process.
//
// The desktop build runs two backends during the migration: this Go sidecar and
// the Rust front door, which answers an increasing share of the API surface.
// Both load the workspace document into memory and save it back whole, so a
// writer that never re-reads would silently discard the other side's changes.
// Keeping exactly one writer per request solves that: every request re-reads
// the document first, and a save can only ever follow a fresh read.
//
// Cheap when nothing changed (one stat), and a no-op if the other side wrote a
// document we cannot parse — the in-memory state always wins over a broken
// file, never the other way round.
func (s *Store) reloadIfChangedLocked() {
	if s.file == ":memory:" {
		return
	}
	info, err := os.Stat(s.file)
	if err != nil {
		return
	}
	if mt, size := info.ModTime().UnixNano(), info.Size(); mt == s.savedMtime && size == s.savedSize {
		return
	}
	raw, err := os.ReadFile(s.file)
	if err != nil {
		return
	}
	if err := json.Unmarshal(raw, s); err != nil {
		return
	}
	s.rememberSavedLocked()
}
func (s *Store) allocate() int { n := s.Next; s.Next++; return n }
func (s *Store) folderLocked(n int) Row {
	for _, r := range s.Folders {
		if number(r["id"]) == n {
			return r
		}
	}
	return nil
}
func (s *Store) groupLocked(n int) Row {
	for _, g := range s.FolderGroups {
		if number(g["id"]) == n {
			return g
		}
	}
	return nil
}

// groupsLocked returns every folder group deep-cloned in sort_order. Always a
// non-nil slice: the frontend filters the result, and a JSON null would throw.
func (s *Store) groupsLocked() []Row {
	out := []Row{}
	for _, g := range s.FolderGroups {
		out = append(out, clone(g))
	}
	sort.SliceStable(out, func(i, j int) bool {
		return number(out[i]["sort_order"]) < number(out[j]["sort_order"])
	})
	return out
}
func groupColor(v any) string {
	if s := text(v); s != "" {
		return s
	}
	return "default"
}
func (s *Store) conversationLocked(n int) Row {
	for _, r := range s.Conversations {
		if number(r["id"]) == n {
			return r
		}
	}
	return nil
}
func (s *Store) openFolderLocked(path, kind string) Row {
	for _, r := range s.Folders {
		if text(r["path"]) == path {
			r["last_opened_at"] = timestamp()
			r["open"] = true
			return r
		}
	}
	r := Row{"id": s.allocate(), "name": filepath.Base(path), "path": path, "git_branch": nil, "default_agent_type": nil, "last_opened_at": timestamp(), "sort_order": len(s.Folders), "color": "default", "parent_id": nil, "kind": kind, "alias": nil, "group_id": nil, "open": true}
	s.Folders = append(s.Folders, r)
	return r
}
func (s *Store) createConversationLocked(folder int, agent, title string, parent int) Row {
	r := Row{"id": s.allocate(), "folder_id": folder, "title": title, "title_locked": false, "agent_type": agent, "status": "pending", "kind": "conversation", "model": nil, "git_branch": nil, "external_id": nil, "message_count": 0, "child_count": 0, "created_at": timestamp(), "updated_at": timestamp(), "pinned_at": nil, "parent_id": nil, "tag_ids": []int{}}
	r["kind"] = "regular"
	if f := s.folderLocked(folder); f != nil && f["kind"] == "chat" {
		r["kind"] = "chat"
	}
	if parent > 0 {
		r["kind"] = "delegate"
		r["parent_id"] = parent
		if p := s.conversationLocked(parent); p != nil {
			p["child_count"] = number(p["child_count"]) + 1
		}
	}
	s.Conversations = append(s.Conversations, r)
	return r
}
