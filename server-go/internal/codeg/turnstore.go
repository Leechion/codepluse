package codeg

// Turn storage: the append-only message history that used to live inside
// codeg-workspace.json as a single map, which made every save rewrite the
// entire conversation archive. Turns now live in SQLite; the JSON document
// keeps only the small structural state (folders, conversation metadata, tabs,
// canvas, settings, agents).
//
// Paging needs the fingerprint/counts of the prefix before a window, and
// recomputing them by reading the prefix would reintroduce an O(history) read
// per page. Each row therefore stores the cumulative FNV-1a hash, inclusive
// role counts and max timestamp up to and including itself; a page start only
// needs the row immediately before it.

import (
	"database/sql"
	"encoding/json"
	"fmt"
	"net/url"
	"os"
	"path/filepath"
	"strconv"
	"strings"
	"time"

	_ "modernc.org/sqlite"
)

const turnSchema = `
CREATE TABLE IF NOT EXISTS turns (
	conversation_id INTEGER NOT NULL,
	seq             INTEGER NOT NULL,
	data            TEXT    NOT NULL,
	role            TEXT    NOT NULL,
	timestamp       TEXT    NOT NULL,
	hash_after      TEXT    NOT NULL,
	assistant_upto  INTEGER NOT NULL,
	user_upto       INTEGER NOT NULL,
	max_ts          TEXT    NOT NULL,
	PRIMARY KEY (conversation_id, seq)
);
CREATE INDEX IF NOT EXISTS idx_turns_conv_seq ON turns(conversation_id, seq);
`

const insertTurnSQL = `
INSERT INTO turns (conversation_id, seq, data, role, timestamp, hash_after, assistant_upto, user_upto, max_ts)
VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)
`

// maxTurnSeq is an exclusive upper bound for "all turns"; it fits int32 so the
// range helpers stay portable.
const maxTurnSeq = 1<<31 - 1

type turnDB struct {
	db *sql.DB
}

// turnDBFile derives the database path beside the workspace JSON. An in-memory
// store keeps an in-memory database so tests remain isolated.
func turnDBFile(workspaceFile string) string {
	if workspaceFile == ":memory:" {
		return ":memory:"
	}
	return filepath.Join(filepath.Dir(workspaceFile), "codeg.db")
}

func openTurnDB(path string) (*turnDB, error) {
	dsn := path
	if path != ":memory:" {
		if err := os.MkdirAll(filepath.Dir(path), 0755); err != nil {
			return nil, err
		}
		slash := filepath.ToSlash(path)
		if !strings.HasPrefix(slash, "/") {
			slash = "/" + slash
		}
		dsn = (&url.URL{Scheme: "file", Path: slash}).String()
	}
	db, err := sql.Open("sqlite", dsn)
	if err != nil {
		return nil, err
	}
	// The store serialises access with its own mutex; one connection also keeps
	// an in-memory database from being split across pooled connections.
	db.SetMaxOpenConns(1)
	db.SetMaxIdleConns(1)
	for _, pragma := range []string{
		"PRAGMA journal_mode=WAL",
		"PRAGMA synchronous=NORMAL",
		"PRAGMA busy_timeout=5000",
	} {
		if _, err := db.Exec(pragma); err != nil {
			_ = db.Close()
			return nil, fmt.Errorf("初始化会话数据库失败：%w", err)
		}
	}
	for _, stmt := range strings.Split(turnSchema, ";") {
		if strings.TrimSpace(stmt) == "" {
			continue
		}
		if _, err := db.Exec(stmt); err != nil {
			_ = db.Close()
			return nil, fmt.Errorf("创建 turns 表失败：%w", err)
		}
	}
	return &turnDB{db: db}, nil
}

func (t *turnDB) close() error { return t.db.Close() }

// turnState is the cumulative prefix state carried by one row.
type turnState struct {
	hash            string
	assistant, user int
	maxTs           string
	role            string
}

// count returns the number of turns and doubles as the next seq. Seqs are
// always contiguous (insert appends, truncate removes the tail, replace
// rewrites from zero), so MAX(seq)+1 avoids a COUNT(*) scan per append.
func (t *turnDB) count(conv int) (int, error) {
	var n int
	err := t.db.QueryRow(`SELECT COALESCE(MAX(seq) + 1, 0) FROM turns WHERE conversation_id = ?`, conv).Scan(&n)
	return n, err
}

func (t *turnDB) latest(conv int) (turnState, bool, error) {
	var st turnState
	row := t.db.QueryRow(`SELECT role, hash_after, assistant_upto, user_upto, max_ts
		FROM turns WHERE conversation_id = ? ORDER BY seq DESC LIMIT 1`, conv)
	err := row.Scan(&st.role, &st.hash, &st.assistant, &st.user, &st.maxTs)
	if err == sql.ErrNoRows {
		return turnState{}, false, nil
	}
	if err != nil {
		return turnState{}, false, err
	}
	return st, true, nil
}

func (t *turnDB) at(conv, seq int) (turnState, bool, error) {
	var st turnState
	row := t.db.QueryRow(`SELECT role, hash_after, assistant_upto, user_upto, max_ts
		FROM turns WHERE conversation_id = ? AND seq = ?`, conv, seq)
	err := row.Scan(&st.role, &st.hash, &st.assistant, &st.user, &st.maxTs)
	if err == sql.ErrNoRows {
		return turnState{}, false, nil
	}
	if err != nil {
		return turnState{}, false, err
	}
	return st, true, nil
}

// nextTurnState extends prev (empty when ok is false) with one turn and
// returns the row's cumulative state plus its JSON payload.
func nextTurnState(prev turnState, ok bool, turn Row) (turnState, []byte, error) {
	role := text(turn["role"])
	// Legacy turns may have no timestamp. Keep it empty instead of inventing
	// "now": the fingerprint treats a missing timestamp as epoch 0, and
	// rewriting history would change the prefix hash the client verifies.
	ts := text(turn["timestamp"])
	h := uint64(fnvSeed)
	assistant, user := 0, 0
	maxTs := ""
	if ok {
		parsed, err := strconv.ParseUint(prev.hash, 16, 64)
		if err != nil {
			return turnState{}, nil, fmt.Errorf("损坏的历史指纹：%v", err)
		}
		h = parsed
		assistant, user = prev.assistant, prev.user
		maxTs = prev.maxTs
	}
	h = fnvTurn(h, role, ts)
	if role == "assistant" {
		assistant++
	}
	if role == "user" {
		user++
	}
	maxTs = laterTimestamp(maxTs, ts)
	data, err := json.Marshal(turn)
	if err != nil {
		return turnState{}, nil, err
	}
	return turnState{hash: fmt.Sprintf("%016x", h), assistant: assistant, user: user, maxTs: maxTs, role: role}, data, nil
}

// append adds turn at the end of a conversation.
func (t *turnDB) append(conv int, turn Row) error {
	seq, err := t.count(conv)
	if err != nil {
		return err
	}
	prev, ok, err := t.latest(conv)
	if err != nil {
		return err
	}
	state, data, err := nextTurnState(prev, ok, turn)
	if err != nil {
		return err
	}
	_, err = t.db.Exec(insertTurnSQL, conv, seq, string(data), state.role, text(turn["timestamp"]), state.hash, state.assistant, state.user, state.maxTs)
	return err
}

// importTurns writes a legacy turn list in one transaction.
func (t *turnDB) importTurns(conv int, turns []Row) error {
	tx, err := t.db.Begin()
	if err != nil {
		return err
	}
	defer func() { _ = tx.Rollback() }()
	var state turnState
	ok := false
	for i, turn := range turns {
		var data []byte
		state, data, err = nextTurnState(state, ok, turn)
		if err != nil {
			return err
		}
		ok = true
		if _, err = tx.Exec(insertTurnSQL, conv, i, string(data), state.role, text(turn["timestamp"]), state.hash, state.assistant, state.user, state.maxTs); err != nil {
			return err
		}
	}
	return tx.Commit()
}

// replace overrides a conversation's whole turn list in one transaction.
func (t *turnDB) replace(conv int, turns []Row) error {
	tx, err := t.db.Begin()
	if err != nil {
		return err
	}
	defer func() { _ = tx.Rollback() }()
	if _, err = tx.Exec(`DELETE FROM turns WHERE conversation_id = ?`, conv); err != nil {
		return err
	}
	var state turnState
	ok := false
	for i, turn := range turns {
		var data []byte
		state, data, err = nextTurnState(state, ok, turn)
		if err != nil {
			return err
		}
		ok = true
		if _, err = tx.Exec(insertTurnSQL, conv, i, string(data), state.role, text(turn["timestamp"]), state.hash, state.assistant, state.user, state.maxTs); err != nil {
			return err
		}
	}
	return tx.Commit()
}

func (t *turnDB) deleteConv(conv int) error {
	_, err := t.db.Exec(`DELETE FROM turns WHERE conversation_id = ?`, conv)
	return err
}

func (t *turnDB) truncate(conv, keep int) error {
	_, err := t.db.Exec(`DELETE FROM turns WHERE conversation_id = ? AND seq >= ?`, conv, keep)
	return err
}

// scanRows decodes row payloads in seq order.
func scanTurnRows(rows *sql.Rows) ([]Row, error) {
	defer rows.Close()
	out := []Row{}
	for rows.Next() {
		var data string
		if err := rows.Scan(&data); err != nil {
			return nil, err
		}
		var turn Row
		if err := json.Unmarshal([]byte(data), &turn); err != nil {
			return nil, err
		}
		out = append(out, turn)
	}
	return out, rows.Err()
}

func (t *turnDB) rangeRows(conv, start, end int) ([]Row, error) {
	rows, err := t.db.Query(`SELECT data FROM turns WHERE conversation_id = ? AND seq >= ? AND seq < ? ORDER BY seq`, conv, start, end)
	if err != nil {
		return nil, err
	}
	return scanTurnRows(rows)
}

func (t *turnDB) allTurns() (map[int][]Row, error) {
	rows, err := t.db.Query(`SELECT conversation_id, data FROM turns ORDER BY conversation_id, seq`)
	if err != nil {
		return nil, err
	}
	defer rows.Close()
	out := map[int][]Row{}
	for rows.Next() {
		var conv int
		var data string
		if err := rows.Scan(&conv, &data); err != nil {
			return nil, err
		}
		var turn Row
		if err := json.Unmarshal([]byte(data), &turn); err != nil {
			return nil, err
		}
		out[conv] = append(out[conv], turn)
	}
	return out, rows.Err()
}

func (t *turnDB) assistantCount() (int, error) {
	var n int
	err := t.db.QueryRow(`SELECT COUNT(*) FROM turns WHERE role = 'assistant'`).Scan(&n)
	return n, err
}

// --- Store wrappers (callers hold Store.mu) ---

// turnStore opens the database on first use. Lazy opening keeps a store that
// never sends a message from creating an empty database (and .data directory)
// on disk, and keeps read-only test fixtures free of extra files.
func (s *Store) turnStore() (*turnDB, error) {
	s.turnMu.Lock()
	defer s.turnMu.Unlock()
	if s.turns != nil {
		return s.turns, nil
	}
	if s.closed {
		return nil, fmt.Errorf("会话存储已关闭")
	}
	if s.turnsPath == "" {
		return nil, fmt.Errorf("会话存储路径未配置")
	}
	db, err := openTurnDB(s.turnsPath)
	if err != nil {
		return nil, err
	}
	s.turns = db
	return db, nil
}

func (s *Store) insertTurnLocked(conversation int, turn Row) error {
	db, err := s.turnStore()
	if err != nil {
		return err
	}
	return db.append(conversation, turn)
}

// appendTurnLocked adds a turn with the given display blocks and maintains the
// conversation's message_count/updated_at.
func (s *Store) appendTurnLocked(conversation int, role, body string, blocks []Row) (Row, error) {
	if blocks == nil {
		blocks = []Row{{"type": "text", "text": body}}
	}
	turn := Row{"id": id(), "role": role, "blocks": blocks, "timestamp": timestamp()}
	if err := s.insertTurnLocked(conversation, turn); err != nil {
		return nil, err
	}
	if r := s.conversationLocked(conversation); r != nil {
		r["message_count"] = s.turnCountLocked(conversation)
		r["updated_at"] = timestamp()
	}
	return turn, nil
}

func (s *Store) turnsLocked(conversation int) []Row {
	db, err := s.turnStore()
	if err != nil {
		fmt.Fprintln(os.Stderr, "[codeg-store] 打开历史库失败：", err)
		return []Row{}
	}
	turns, err := db.rangeRows(conversation, 0, maxTurnSeq)
	if err != nil {
		fmt.Fprintln(os.Stderr, "[codeg-store] 读取历史失败：", err)
		return []Row{}
	}
	return turns
}

func (s *Store) turnCountLocked(conversation int) int {
	db, err := s.turnStore()
	if err != nil {
		fmt.Fprintln(os.Stderr, "[codeg-store] 打开历史库失败：", err)
		return 0
	}
	n, err := db.count(conversation)
	if err != nil {
		fmt.Fprintln(os.Stderr, "[codeg-store] 统计历史失败：", err)
		return 0
	}
	return n
}

func (s *Store) turnRangeLocked(conversation, start, end int) []Row {
	db, err := s.turnStore()
	if err != nil {
		fmt.Fprintln(os.Stderr, "[codeg-store] 打开历史库失败：", err)
		return []Row{}
	}
	turns, err := db.rangeRows(conversation, start, end)
	if err != nil {
		fmt.Fprintln(os.Stderr, "[codeg-store] 分页读取历史失败：", err)
		return []Row{}
	}
	return turns
}

func (s *Store) prefixAtLocked(conversation, seq int) (turnState, bool) {
	db, err := s.turnStore()
	if err != nil {
		return turnState{}, false
	}
	st, ok, err := db.at(conversation, seq)
	if err != nil {
		fmt.Fprintln(os.Stderr, "[codeg-store] 读取历史前缀失败：", err)
		return turnState{}, false
	}
	return st, ok
}

func (s *Store) deleteTurnsLocked(conversation int) error {
	db, err := s.turnStore()
	if err != nil {
		return err
	}
	return db.deleteConv(conversation)
}

func (s *Store) truncateTurnsLocked(conversation, keep int) error {
	db, err := s.turnStore()
	if err != nil {
		return err
	}
	return db.truncate(conversation, keep)
}

func (s *Store) replaceTurnsLocked(conversation int, turns []Row) error {
	db, err := s.turnStore()
	if err != nil {
		return err
	}
	return db.replace(conversation, turns)
}

func (s *Store) allTurnsLocked() map[int][]Row {
	db, err := s.turnStore()
	if err != nil {
		fmt.Fprintln(os.Stderr, "[codeg-store] 打开历史库失败：", err)
		return map[int][]Row{}
	}
	all, err := db.allTurns()
	if err != nil {
		fmt.Fprintln(os.Stderr, "[codeg-store] 读取全部历史失败：", err)
		return map[int][]Row{}
	}
	return all
}

func (s *Store) assistantTurnCountLocked() int {
	db, err := s.turnStore()
	if err != nil {
		return 0
	}
	n, err := db.assistantCount()
	if err != nil {
		return 0
	}
	return n
}

func (s *Store) Close() error {
	s.turnMu.Lock()
	defer s.turnMu.Unlock()
	s.closed = true
	if s.turns == nil {
		return nil
	}
	err := s.turns.close()
	s.turns = nil
	return err
}

// fnvTurn folds one turn's role tag and timestamp into an FNV-1a 64 hash. It
// must stay byte-compatible with prefixFingerprint and the client's
// extendPrefixFingerprint.
func fnvTurn(hash uint64, role, ts string) uint64 {
	var tag byte
	switch role {
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
	if parsed, err := time.Parse(time.RFC3339Nano, ts); err == nil {
		millis = parsed.UnixMilli()
	}
	v := uint64(millis)
	for i := 0; i < 8; i++ {
		hash ^= v & 0xff
		hash *= fnvPrime
		v >>= 8
	}
	return hash
}

func laterTimestamp(a, b string) string {
	if a == "" {
		return b
	}
	if b == "" {
		return a
	}
	ta, errA := time.Parse(time.RFC3339Nano, a)
	tb, errB := time.Parse(time.RFC3339Nano, b)
	if errA != nil {
		return b
	}
	if errB != nil {
		return a
	}
	if tb.After(ta) {
		return b
	}
	return a
}
