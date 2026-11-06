package codeg

import (
	"encoding/json"
	"fmt"
	"io/fs"
	"os"
	"path/filepath"
	"strconv"
	"strings"
	"sync"
	"time"
)

type WorkspaceWatch struct {
	mu          sync.Mutex
	Root        string
	Refs, Seq   int
	Snapshot    Row
	Fingerprint string
	Files       map[string]string
	Stop        chan struct{}
	Once        sync.Once
}

func (w *WorkspaceWatch) close() { w.Once.Do(func() { close(w.Stop) }) }
func (s *Server) workspaceCommand(command string, a Row) (any, error) {
	root, err := s.Resolve(text(a["rootPath"]))
	if err != nil {
		return nil, err
	}
	s.watchMu.Lock()
	if s.watches == nil {
		s.watches = map[string]*WorkspaceWatch{}
	}
	w := s.watches[root]
	if command == "stop_workspace_state_stream" {
		if w != nil {
			w.Refs--
			if w.Refs <= 0 {
				w.close()
				delete(s.watches, root)
			}
		}
		s.watchMu.Unlock()
		return nil, nil
	}
	created := w == nil
	if created {
		w = &WorkspaceWatch{Root: root, Stop: make(chan struct{}), Files: map[string]string{}}
		s.watches[root] = w
	}
	if command == "start_workspace_state_stream" {
		w.Refs++
	}
	s.watchMu.Unlock()
	snapshot, err := s.scanWorkspace(w, false)
	if created {
		go func() {
			ticker := time.NewTicker(1500 * time.Millisecond)
			defer ticker.Stop()
			for {
				select {
				case <-w.Stop:
					return
				case <-ticker.C:
					_, _ = s.scanWorkspace(w, true)
				}
			}
		}()
	}
	return snapshot, err
}
func (s *Server) scanWorkspace(w *WorkspaceWatch, broadcast bool) (Row, error) {
	w.mu.Lock()
	defer w.mu.Unlock()
	tree, err := s.files("get_file_tree", Row{"path": w.Root, "maxDepth": 8})
	if err != nil {
		return nil, err
	}
	files := map[string]string{}
	err = filepath.WalkDir(w.Root, func(path string, entry fs.DirEntry, e error) error {
		if e != nil {
			return e
		}
		if path != w.Root && (entry.Type()&os.ModeSymlink != 0 || entry.IsDir() && ignoredDirs[entry.Name()]) {
			if entry.IsDir() {
				return filepath.SkipDir
			}
			return nil
		}
		if !entry.IsDir() {
			info, e := entry.Info()
			if e != nil {
				return e
			}
			files[path] = fmt.Sprintf("%d:%d", info.Size(), info.ModTime().UnixNano())
		}
		return nil
	})
	if err != nil {
		return nil, err
	}
	changed := []string{}
	for path, stamp := range files {
		if w.Files[path] != stamp {
			changed = append(changed, path)
		}
	}
	for path := range w.Files {
		if _, ok := files[path]; !ok {
			changed = append(changed, path)
		}
	}
	_, repoErr := gitRun(w.Root, "rev-parse", "--git-dir")
	git := []Row{}
	if repoErr == nil {
		raw, e := s.gitCommand("git_status", Row{"path": w.Root, "showAllUntracked": true})
		if e != nil {
			return nil, e
		}
		counts := map[string][2]int{}
		stats, e := gitRun(w.Root, "diff", "--numstat", "HEAD", "--")
		if e != nil {
			stats, _ = gitRun(w.Root, "diff", "--numstat", "--cached", "--")
		}
		for _, line := range strings.Split(stats, "\n") {
			parts := strings.SplitN(line, "\t", 3)
			if len(parts) == 3 {
				add, _ := strconv.Atoi(parts[0])
				del, _ := strconv.Atoi(parts[1])
				counts[parts[2]] = [2]int{add, del}
			}
		}
		for _, r := range raw.([]Row) {
			path := text(r["file"])
			count := counts[path]
			git = append(git, Row{"path": path, "status": r["status"], "additions": count[0], "deletions": count[1]})
		}
	}
	signature, _ := json.Marshal(Row{"tree": tree, "git": git})
	fingerprint := string(signature)
	different := len(changed) > 0 || w.Fingerprint != fingerprint
	if different {
		w.Seq++
	}
	w.Files, w.Fingerprint = files, fingerprint
	snapshot := Row{"root_path": w.Root, "seq": w.Seq, "version": 1, "full": true, "tree_snapshot": tree, "git_snapshot": git, "deltas": []Row{}, "degraded": false, "is_git_repo": repoErr == nil}
	w.Snapshot = snapshot
	if different && broadcast {
		s.emit("folder://workspace-state-event", Row{"root_path": w.Root, "seq": w.Seq, "version": 1, "kind": "fs_delta", "payload": []Row{{"kind": "tree_replace", "nodes": tree}, {"kind": "git_replace", "entries": git}}, "requires_resync": false, "changed_paths": changed})
	}
	return clone(snapshot), nil
}
