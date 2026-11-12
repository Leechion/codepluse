// Package codeg ports the Codeg 0.42 web contracts to the Go runtime.
// Frontend source: spacering-net/codeg, Apache-2.0 (codeg-ui/LICENSE).
package codeg

import (
	"crypto/rand"
	"encoding/hex"
	"encoding/json"
	"fmt"
	"os"
	"path/filepath"
	"sort"
	"strings"
	"sync"
	"time"
)

type Row = map[string]any
type Store struct {
	mu             sync.Mutex
	file           string
	Next           int              `json:"next"`
	Folders        []Row            `json:"folders"`
	Conversations  []Row            `json:"conversations"`
	Turns          map[string][]Row `json:"turns"`
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
	Agents map[string]Row `json:"agents"`
}

func timestamp() string { return time.Now().UTC().Format(time.RFC3339Nano) }
func id() string        { b := make([]byte, 16); _, _ = rand.Read(b); return hex.EncodeToString(b) }
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
	case float64:
		return int(n)
	}
	return 0
}
func clone[T any](value T) T {
	b, _ := json.Marshal(value)
	var out T
	_ = json.Unmarshal(b, &out)
	return out
}
func newStore(file string) (*Store, error) {
	s := &Store{file: file, Next: 1, Folders: []Row{}, Conversations: []Row{}, Turns: map[string][]Row{}, Settings: map[string]any{}, Tabs: []Row{}, FolderGroups: []Row{}, ConversationTags: []Row{}, Assistants: []Row{}, QuickMessages: []Row{}, Agents: map[string]Row{}}
	if file == ":memory:" {
		return s, nil
	}
	b, err := os.ReadFile(file)
	if err == nil {
		if err = json.Unmarshal(b, s); err != nil {
			return nil, err
		}
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
	return s, nil
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
	return os.Rename(s.file+".tmp", s.file)
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
	s.Turns[fmt.Sprint(number(r["id"]))] = []Row{}
	return r
}
func (s *Store) appendTurnLocked(conversation int, role, body string) {
	key := fmt.Sprint(conversation)
	r := s.conversationLocked(conversation)
	if r == nil {
		return
	}
	s.Turns[key] = append(s.Turns[key], Row{"id": id(), "role": role, "blocks": []Row{{"type": "text", "text": body}}, "timestamp": timestamp()})
	r["message_count"] = len(s.Turns[key])
	r["updated_at"] = timestamp()
}
