// Folder commands: the web contract ports of Codeg's per-folder shell commands
// (snake_case wire format; see codeg-ui/src/lib/types.ts FolderCommand).
// Each row is a named shell snippet the user can launch in a terminal (or, when
// run_local is set, the desktop host runs detached on its own machine). The
// store is a single atomically-written JSON file under .data like the other
// auxiliary stores (automations, forge, ...).
package codeg

import (
	"encoding/json"
	"fmt"
	"os"
	"path/filepath"
	"sort"
	"strings"
	"sync"
)

type FolderCommand struct {
	ID        int    `json:"id"`
	FolderID  int    `json:"folder_id"`
	Name      string `json:"name"`
	Command   string `json:"command"`
	SortOrder int    `json:"sort_order"`
	RunLocal  bool   `json:"run_local"`
	CreatedAt string `json:"created_at"`
	UpdatedAt string `json:"updated_at"`
}

type folderCommandStore struct {
	mu       sync.Mutex
	root     string
	nextID   int
	commands []*FolderCommand
}

func newFolderCommandStore(root string) *folderCommandStore {
	st := &folderCommandStore{root: root, nextID: 1, commands: []*FolderCommand{}}
	st.load()
	return st
}

func (st *folderCommandStore) file() string {
	return filepath.Join(st.root, ".data", "codeg-folder-commands.json")
}

func (st *folderCommandStore) load() {
	raw, err := readJSONFileData(st.file())
	if err != nil {
		return
	}
	if json.Unmarshal(raw, &st.commands) != nil {
		return
	}
	for _, c := range st.commands {
		if c.ID >= st.nextID {
			st.nextID = c.ID + 1
		}
	}
}

func (st *folderCommandStore) save() error {
	if st.commands == nil {
		st.commands = []*FolderCommand{}
	}
	return atomicWrite(st.file(), st.commands)
}

func (st *folderCommandStore) find(id int) *FolderCommand {
	for _, c := range st.commands {
		if c.ID == id {
			return c
		}
	}
	return nil
}

// forFolder returns the folder's rows in display order (sort_order, then id as
// a stable tiebreak). A fresh slice keeps callers from mutating the backing
// array while the store lock is released.
func (st *folderCommandStore) forFolder(folderID int) []*FolderCommand {
	out := []*FolderCommand{}
	for _, c := range st.commands {
		if c.FolderID == folderID {
			out = append(out, c)
		}
	}
	sort.SliceStable(out, func(i, j int) bool {
		if out[i].SortOrder != out[j].SortOrder {
			return out[i].SortOrder < out[j].SortOrder
		}
		return out[i].ID < out[j].ID
	})
	return out
}

func (st *folderCommandStore) nextSortOrder(folderID int) int {
	max := -1
	for _, c := range st.commands {
		if c.FolderID == folderID && c.SortOrder > max {
			max = c.SortOrder
		}
	}
	return max + 1
}

// resolveFolderPath returns the stored folder path, or an error when the folder
// does not exist. Called before the folder-command lock is taken so no nested
// lock ordering can arise.
func (s *Server) resolveFolderPath(folderID int) (string, error) {
	st := s.store
	st.mu.Lock()
	folder := st.folderLocked(folderID)
	path := ""
	if folder != nil {
		path = text(folder["path"])
	}
	st.mu.Unlock()
	if folder == nil {
		return "", fmt.Errorf("项目不存在")
	}
	return path, nil
}

func (s *Server) folderCommandCommand(command string, a Row) (any, error) {
	// Resolve folder context up front for the commands that need it, before
	// taking the auxiliary store lock.
	folderPath := ""
	switch command {
	case "create_folder_command", "bootstrap_folder_commands_from_package_json":
		var err error
		folderPath, err = s.resolveFolderPath(number(a["folderId"]))
		if err != nil {
			return nil, err
		}
	}

	st := s.folderCommands
	st.mu.Lock()
	defer st.mu.Unlock()

	switch command {
	case "list_folder_commands":
		return st.forFolder(number(a["folderId"])), nil

	case "create_folder_command":
		folderID := number(a["folderId"])
		name := strings.TrimSpace(text(a["name"]))
		if name == "" {
			return nil, fmt.Errorf("请填写命令名称")
		}
		commandLine := text(a["command"])
		if strings.TrimSpace(commandLine) == "" {
			return nil, fmt.Errorf("请填写命令内容")
		}
		runLocal, _ := a["runLocal"].(bool)
		item := &FolderCommand{
			ID:        st.nextID,
			FolderID:  folderID,
			Name:      name,
			Command:   commandLine,
			SortOrder: st.nextSortOrder(folderID),
			RunLocal:  runLocal,
			CreatedAt: timestamp(),
			UpdatedAt: timestamp(),
		}
		st.nextID++
		st.commands = append(st.commands, item)
		if err := st.save(); err != nil {
			return nil, err
		}
		return item, nil

	case "update_folder_command":
		item := st.find(number(a["id"]))
		if item == nil {
			return nil, fmt.Errorf("命令不存在")
		}
		if v, ok := a["name"]; ok && v != nil {
			name := strings.TrimSpace(text(v))
			if name == "" {
				return nil, fmt.Errorf("请填写命令名称")
			}
			item.Name = name
		}
		if v, ok := a["command"]; ok && v != nil {
			if strings.TrimSpace(text(v)) == "" {
				return nil, fmt.Errorf("请填写命令内容")
			}
			item.Command = text(v)
		}
		if v, ok := a["sortOrder"]; ok && v != nil {
			item.SortOrder = number(v)
		}
		if v, ok := a["runLocal"]; ok && v != nil {
			item.RunLocal, _ = v.(bool)
		}
		item.UpdatedAt = timestamp()
		if err := st.save(); err != nil {
			return nil, err
		}
		return item, nil

	case "delete_folder_command":
		id := number(a["id"])
		filtered := st.commands[:0]
		found := false
		for _, c := range st.commands {
			if c.ID == id {
				found = true
				continue
			}
			filtered = append(filtered, c)
		}
		if !found {
			return nil, fmt.Errorf("命令不存在")
		}
		st.commands = filtered
		if err := st.save(); err != nil {
			return nil, err
		}
		return nil, nil

	case "reorder_folder_commands":
		folderID := number(a["folderId"])
		order := map[int]int{}
		for i, id := range ids(a["ids"]) {
			order[id] = i
		}
		for _, c := range st.commands {
			if c.FolderID == folderID {
				if pos, ok := order[c.ID]; ok {
					c.SortOrder = pos
				}
			}
		}
		if err := st.save(); err != nil {
			return nil, err
		}
		return nil, nil

	case "bootstrap_folder_commands_from_package_json":
		folderID := number(a["folderId"])
		// Idempotent: the client only calls this on an empty folder, and if rows
		// already exist return them rather than create duplicates.
		if existing := st.forFolder(folderID); len(existing) > 0 {
			return existing, nil
		}
		created, err := st.bootstrapFromPackageJSON(folderID, folderPath)
		if err != nil {
			return nil, err
		}
		if err := st.save(); err != nil {
			return nil, err
		}
		return created, nil
	}
	return nil, fmt.Errorf("未知的文件夹命令：%s", command)
}

// bootstrapFromPackageJSON turns each package.json "scripts" entry into a
// folder command. Script names are sorted so the generated order is stable.
func (st *folderCommandStore) bootstrapFromPackageJSON(folderID int, folderPath string) ([]*FolderCommand, error) {
	pkgPath := filepath.Join(folderPath, "package.json")
	data, err := os.ReadFile(pkgPath)
	if err != nil {
		// No package.json is a normal outcome (non-Node project): nothing to
		// bootstrap, return an empty list rather than an error.
		if os.IsNotExist(err) {
			return []*FolderCommand{}, nil
		}
		return nil, err
	}
	var pkg struct {
		Scripts map[string]string `json:"scripts"`
	}
	if err := json.Unmarshal(data, &pkg); err != nil {
		return nil, fmt.Errorf("解析 package.json 失败：%w", err)
	}
	names := make([]string, 0, len(pkg.Scripts))
	for name := range pkg.Scripts {
		names = append(names, name)
	}
	sort.Strings(names)

	now := timestamp()
	created := []*FolderCommand{}
	for i, name := range names {
		item := &FolderCommand{
			ID:        st.nextID,
			FolderID:  folderID,
			Name:      name,
			Command:   pkg.Scripts[name],
			SortOrder: i,
			RunLocal:  false,
			CreatedAt: now,
			UpdatedAt: now,
		}
		st.nextID++
		st.commands = append(st.commands, item)
		created = append(created, item)
	}
	return created, nil
}
