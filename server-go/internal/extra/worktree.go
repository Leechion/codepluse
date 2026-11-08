// Package extra hosts the peripheral services: git worktree isolation,
// skill packs, webhook notification and session handoff — the Go ports of
// core/worktree.js, skills.js, notifier.js and handoff.js.
package extra

import (
	"fmt"
	"os"
	"os/exec"
	"path/filepath"
	"regexp"
	"strings"
)

const MAX_DIFF = 200000

// Worktrees isolates missions in git worktrees (codeg's To-dos model): each
// mission runs in its own branch; a human reviews the diff and merges or
// discards. Non-git directories fall back to the shared workdir instead of
// failing.
type Worktrees struct {
	Root string
	Dir  string
}

// NewWorktrees defaults dir to <root>/.worktrees.
func NewWorktrees(root, dir string) *Worktrees {
	if dir == "" {
		dir = filepath.Join(root, ".worktrees")
	}
	return &Worktrees{Root: root, Dir: dir}
}

var worktreeIDRe = regexp.MustCompile(`^[A-Za-z0-9][A-Za-z0-9._-]{0,63}$`)

func (w *Worktrees) path(id string) string   { return filepath.Join(w.Dir, id) }
func (w *Worktrees) branch(id string) string { return "hh/" + id }

func git(cwd string, allowFail bool, args ...string) (string, error) {
	cmd := exec.Command("git", args...)
	cmd.Dir = cwd
	var out, errOut strings.Builder
	cmd.Stdout = &out
	cmd.Stderr = &errOut
	err := cmd.Run()
	if err != nil && !allowFail {
		msg := errOut.String()
		if msg == "" {
			msg = err.Error()
		}
		if len(msg) > 400 {
			msg = msg[len(msg)-400:]
		}
		return out.String(), fmt.Errorf("git %s 失败：%s", args[0], msg)
	}
	return out.String(), nil
}

// Available reports whether baseWorkdir is a git work tree.
func (w *Worktrees) Available(base string) bool {
	out, err := git(base, true, "rev-parse", "--is-inside-work-tree")
	return err == nil && strings.TrimSpace(out) == "true"
}

// For returns (creating if needed) the isolated worktree for a mission. A
// failed attempt takes away only what is provably its own — the branch it just
// tried to create (codeg's worktree.rs rule).
func (w *Worktrees) For(id, base string) (string, error) {
	if !w.Available(base) {
		return "", fmt.Errorf("目标目录不是 git 仓库，无法隔离")
	}
	if !worktreeIDRe.MatchString(id) {
		return "", fmt.Errorf("工作树名称无效：%s", id)
	}
	target := w.path(id)
	if _, err := os.Stat(target); err == nil {
		return target, nil
	}
	_ = os.MkdirAll(w.Dir, 0o755)
	listed, err := git(base, false, "worktree", "list", "--porcelain")
	if err != nil {
		return "", err
	}
	listed_ := false
	for _, line := range strings.Split(listed, "\n") {
		if line == "worktree "+target {
			listed_ = true
		}
	}
	if !listed_ {
		if _, err := git(base, false, "worktree", "add", target, "-b", w.branch(id)); err != nil {
			_, _ = git(base, true, "branch", "-D", w.branch(id))
			return "", err
		}
	}
	return target, nil
}

// Diff renders the review diff: committed + dirty + untracked file contents.
func (w *Worktrees) Diff(id, base string) (map[string]any, error) {
	target := w.path(id)
	if _, err := os.Stat(target); err != nil {
		return nil, fmt.Errorf("该任务没有隔离工作树")
	}
	branch := w.branch(id)
	baseCommit, _ := git(base, true, "merge-base", "HEAD", branch)
	baseCommit = strings.TrimSpace(baseCommit)
	committed := ""
	if baseCommit != "" {
		committed, _ = git(base, true, "diff", baseCommit+".."+branch)
	}
	dirty, _ := git(target, true, "diff", "HEAD")
	untrackedOut, _ := git(target, true, "ls-files", "--others", "--exclude-standard")
	var untracked []string
	for _, line := range strings.Split(untrackedOut, "\n") {
		if strings.TrimSpace(line) != "" {
			untracked = append(untracked, line)
		}
	}
	// Untracked output files never appear in `git diff`, but a reviewer needs them.
	newFiles := strings.Builder{}
	for _, rel := range untracked {
		if len(untracked) > 0 && len(newFiles.String()) > 0 && strings.Count(newFiles.String(), "diff --git") >= 20 {
			break
		}
		bodyBytes, err := os.ReadFile(filepath.Join(target, rel))
		if err != nil {
			continue
		}
		body := string(bodyBytes)
		if len(body) > 20000 {
			body = body[:20000]
		}
		var plus []string
		for _, l := range strings.Split(body, "\n") {
			plus = append(plus, "+"+l)
		}
		newFiles.WriteString("\ndiff --git a/" + rel + " b/" + rel + " (new file)\n--- /dev/null\n+++ b/" + rel + "\n" + strings.Join(plus, "\n") + "\n")
	}
	parts := []string{}
	for _, s := range []string{committed, dirty, newFiles.String()} {
		if strings.TrimSpace(s) != "" {
			parts = append(parts, s)
		}
	}
	text := strings.Join(parts, "\n")
	truncated := false
	if len(text) > MAX_DIFF {
		text = text[:MAX_DIFF]
		truncated = true
	}
	if len(untracked) > 200 {
		untracked = untracked[:200]
	}
	return map[string]any{"branch": branch, "hasDiff": strings.TrimSpace(text) != "", "truncated": truncated,
		"untracked": untracked, "text": text}, nil
}

// Merge auto-commits agent work and merges the mission branch --no-ff.
func (w *Worktrees) Merge(id, base, message string) (map[string]any, error) {
	target := w.path(id)
	if _, err := os.Stat(target); err != nil {
		return nil, fmt.Errorf("该任务没有隔离工作树")
	}
	if message == "" {
		message = "Merge " + w.branch(id)
	}
	// The .worktrees directory lives inside the repo; it is not user dirt.
	statusOut, err := git(base, false, "status", "--porcelain", "--", ".", ":!.worktrees")
	if err != nil {
		return nil, err
	}
	dirty := []string{}
	for _, line := range strings.Split(statusOut, "\n") {
		if strings.TrimSpace(line) != "" && !strings.Contains(line, ".worktrees") {
			dirty = append(dirty, line)
		}
	}
	if len(dirty) > 0 {
		return nil, fmt.Errorf("主工作区有未提交改动，请先提交或暂存后再合并")
	}
	// ACP sessions rarely commit; auto-commit whatever the agent left behind so
	// uncommitted output is not silently dropped by the merge.
	if _, err := git(target, false, "add", "-A"); err != nil {
		return nil, err
	}
	_, _ = git(target, true, "commit", "-m", "Agent work (auto-committed before review merge)")
	if _, err := git(base, false, "merge", "--no-ff", w.branch(id), "-m", short(message, 200)); err != nil {
		return nil, err
	}
	if err := w.cleanup(id, base); err != nil {
		return nil, err
	}
	return map[string]any{"merged": true}, nil
}

// Discard removes the worktree and its branch.
func (w *Worktrees) Discard(id, base string) (map[string]any, error) {
	if _, err := os.Stat(w.path(id)); err != nil {
		return nil, fmt.Errorf("该任务没有隔离工作树")
	}
	if err := w.cleanup(id, base); err != nil {
		return nil, err
	}
	return map[string]any{"discarded": true}, nil
}

func (w *Worktrees) cleanup(id, base string) error {
	if _, err := git(base, false, "worktree", "remove", "--force", w.path(id)); err != nil {
		return err
	}
	_, err := git(base, false, "branch", "-D", w.branch(id))
	return err
}

func short(s string, n int) string {
	if len(s) > n {
		return s[:n]
	}
	return s
}
