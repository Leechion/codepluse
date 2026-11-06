package codeg

import (
	"bytes"
	"context"
	"encoding/base64"
	"fmt"
	"net/url"
	"os"
	"os/exec"
	"path/filepath"
	"strconv"
	"strings"
	"time"
)

func gitRun(path string, args ...string) (string, error) {
	ctx, cancel := context.WithTimeout(context.Background(), 45*time.Second)
	defer cancel()
	argv := append([]string{"--no-optional-locks", "--literal-pathspecs", "-c", "core.quotePath=false", "-C", path}, args...)
	cmd := exec.CommandContext(ctx, "git", argv...)
	hideCommand(cmd)
	cmd.Env = append(os.Environ(), "GIT_TERMINAL_PROMPT=0")
	var stderr bytes.Buffer
	cmd.Stderr = &stderr
	b, err := cmd.Output()
	if err != nil {
		return "", fmt.Errorf("git %s: %s", args[0], strings.TrimSpace(stderr.String()))
	}
	return string(b), nil
}
func gitRef(ref string) error {
	if strings.TrimSpace(ref) == "" || strings.HasPrefix(ref, "-") || strings.ContainsAny(ref, "\r\n\x00") {
		return fmt.Errorf("Git 引用无效")
	}
	return nil
}
func gitLines(value string) []string {
	out := []string{}
	for _, line := range strings.Split(strings.TrimSpace(value), "\n") {
		if v := strings.TrimSpace(line); v != "" {
			out = append(out, v)
		}
	}
	return out
}
func gitChanges(path, commit string) ([]Row, error) {
	out, err := gitRun(path, "show", "--format=", "--numstat", "--no-renames", "--root", commit, "--")
	if err != nil {
		return nil, err
	}
	status, err := gitRun(path, "show", "--format=", "--name-status", "--no-renames", "--root", commit, "--")
	if err != nil {
		return nil, err
	}
	statuses := map[string]string{}
	for _, line := range strings.Split(status, "\n") {
		parts := strings.SplitN(line, "\t", 2)
		if len(parts) == 2 {
			statuses[parts[1]] = parts[0]
		}
	}
	rows := []Row{}
	for _, line := range strings.Split(out, "\n") {
		p := strings.SplitN(line, "\t", 3)
		if len(p) != 3 {
			continue
		}
		add, _ := strconv.Atoi(p[0])
		del, _ := strconv.Atoi(p[1])
		rows = append(rows, Row{"path": p[2], "status": statuses[p[2]], "additions": add, "deletions": del})
	}
	return rows, nil
}
func (s *Server) gitCommand(command string, a Row) (any, error) {
	path, err := s.Resolve(text(a["path"]))
	if err != nil {
		return nil, err
	}
	run := func(args ...string) (string, error) { return gitRun(path, args...) }
	ref := func(key string) (string, error) { v := text(a[key]); return v, gitRef(v) }
	file := text(a["file"])
	if file != "" {
		resolved, e := s.resolveFile(path, file)
		if e != nil {
			return nil, e
		}
		file, e = filepath.Rel(path, resolved)
		if e != nil {
			return nil, e
		}
		file = filepath.ToSlash(file)
	}
	result := func(args ...string) (any, error) { _, e := run(args...); return nil, e }
	branchName := text(a["branchName"])
	switch command {
	case "git_init":
		return result("init")
	case "get_git_branch":
		b, e := run("symbolic-ref", "--short", "HEAD")
		if e != nil {
			return nil, nil
		}
		return strings.TrimSpace(b), nil
	case "git_status":
		mode := "--untracked-files=normal"
		if a["showAllUntracked"] == true {
			mode = "--untracked-files=all"
		}
		out, e := run("status", "--porcelain=v1", "-z", mode)
		if e != nil {
			return nil, e
		}
		parts := strings.Split(out, "\x00")
		rows := []Row{}
		for i := 0; i < len(parts); i++ {
			entry := parts[i]
			if len(entry) < 4 {
				continue
			}
			status, name := strings.TrimSpace(entry[:2]), entry[3:]
			if strings.ContainsAny(entry[:2], "RC") && i+1 < len(parts) {
				i++
			}
			rows = append(rows, Row{"status": status, "file": name})
		}
		return rows, nil
	case "git_diff", "git_diff_with_branch", "git_show_diff":
		args := []string{"diff", "--no-ext-diff", "HEAD"}
		if command == "git_diff_with_branch" {
			branch, e := ref("branch")
			if e != nil {
				return nil, e
			}
			args = []string{"diff", "--no-ext-diff", branch}
		}
		if command == "git_show_diff" {
			commit, e := ref("commit")
			if e != nil {
				return nil, e
			}
			args = []string{"show", "--format=", "--no-ext-diff", commit}
		}
		args = append(args, "--")
		if file != "" {
			args = append(args, file)
		}
		out, e := run(args...)
		if e != nil && command == "git_diff" {
			args = []string{"diff", "--cached", "--no-ext-diff", "--"}
			if file != "" {
				args = append(args, file)
			}
			return run(args...)
		}
		return out, e
	case "git_is_tracked":
		_, e := run("ls-files", "--error-unmatch", "--", file)
		return e == nil, nil
	case "git_show_file", "git_show_file_base64":
		revision := text(a["refName"])
		if revision == "" {
			revision = "HEAD"
		}
		if e := gitRef(revision); e != nil {
			return nil, e
		}
		_, refErr := run("rev-parse", "--verify", "--end-of-options", revision)
		data, e := run("show", revision+":"+file)
		if command == "git_show_file" {
			if e != nil {
				return nil, e
			}
			if strings.ContainsRune(data, 0) {
				return nil, fmt.Errorf("该文件为二进制，请使用图片预览")
			}
			return data, nil
		}
		exists := e == nil
		cap := number(a["maxBytes"])
		if cap <= 0 || cap > 8<<20 {
			cap = 8 << 20
		}
		encoded := ""
		if exists && len(data) <= cap {
			encoded = base64.StdEncoding.EncodeToString([]byte(data))
		}
		return Row{"exists": exists, "ref_missing": refErr != nil, "data": encoded, "byte_size": len(data), "too_large": len(data) > cap}, nil
	case "git_add_files", "git_commit":
		files := []string{}
		for _, raw := range array(a["files"]) {
			p, e := s.resolveFile(path, text(raw))
			if e != nil {
				return nil, e
			}
			rel, e := filepath.Rel(path, p)
			if e != nil {
				return nil, e
			}
			files = append(files, filepath.ToSlash(rel))
		}
		if len(files) == 0 {
			return nil, fmt.Errorf("请选择要提交或暂存的文件")
		}
		if command == "git_commit" && strings.TrimSpace(text(a["message"])) == "" {
			return nil, fmt.Errorf("提交说明不能为空")
		}
		stagedDeletions, _ := run("diff", "--cached", "--name-only", "--diff-filter=D", "-z")
		deleted := map[string]bool{}
		for _, p := range strings.Split(stagedDeletions, "\x00") {
			deleted[p] = true
		}
		toAdd := []string{}
		for _, p := range files {
			if !deleted[p] {
				toAdd = append(toAdd, p)
			}
		}
		if len(toAdd) > 0 {
			if _, e := run(append([]string{"add", "--"}, toAdd...)...); e != nil {
				return nil, e
			}
		}
		if command == "git_add_files" {
			return nil, nil
		}
		if _, e := run(append([]string{"commit", "--only", "-m", text(a["message"]), "--"}, files...)...); e != nil {
			return nil, e
		}
		s.emit("folder://git-commit-succeeded", Row{"folder_id": a["folderId"], "committed_files": len(files)})
		return Row{"committed_files": len(files)}, nil
	case "git_rollback_file":
		if file == "" {
			return nil, fmt.Errorf("文件不能为空")
		}
		return result("restore", "--source=HEAD", "--staged", "--worktree", "--", file)
	case "git_list_branches", "git_list_all_branches":
		local, e := run("for-each-ref", "--format=%(refname:short)", "refs/heads/")
		if e != nil {
			return nil, e
		}
		if command == "git_list_branches" {
			return gitLines(local), nil
		}
		remote, e := run("for-each-ref", "--format=%(refname:short)", "refs/remotes/")
		if e != nil {
			return nil, e
		}
		worktrees, _ := run("worktree", "list", "--porcelain")
		busy := []string{}
		var main any
		blocks := strings.Split(strings.TrimSpace(worktrees), "\n\n")
		for index, block := range blocks {
			p, b := "", ""
			for _, line := range strings.Split(block, "\n") {
				if strings.HasPrefix(line, "worktree ") {
					p = strings.TrimPrefix(line, "worktree ")
				}
				if strings.HasPrefix(line, "branch refs/heads/") {
					b = strings.TrimPrefix(line, "branch refs/heads/")
				}
			}
			if !strings.EqualFold(filepath.Clean(p), filepath.Clean(path)) && b != "" {
				busy = append(busy, b)
				if index == 0 {
					main = b
				}
			}
		}
		return Row{"local": gitLines(local), "remote": gitLines(remote), "worktree_branches": busy, "main_worktree_branch": main}, nil
	case "git_new_branch", "git_checkout", "git_delete_branch", "git_merge", "git_rebase":
		if e := gitRef(branchName); e != nil {
			return nil, e
		}
		if command == "git_new_branch" {
			args := []string{"checkout", "-b", branchName}
			if start := text(a["startPoint"]); start != "" {
				if e := gitRef(start); e != nil {
					return nil, e
				}
				args = append(args, start)
			}
			return result(args...)
		}
		if command == "git_checkout" {
			return result("checkout", branchName, "--")
		}
		if command == "git_delete_branch" {
			flag := "-d"
			if a["force"] == true {
				flag = "-D"
			}
			return run("branch", flag, "--", branchName)
		}
		op := "merge"
		if command == "git_rebase" {
			op = "rebase"
		}
		before, _ := run("rev-parse", "HEAD")
		_, e := run(op, branchName)
		conflict, _ := s.gitCommand("git_list_conflicts", Row{"path": path})
		if e != nil && conflict.(Row)["has_conflicts"] != true {
			return nil, e
		}
		if op == "merge" {
			count, _ := run("rev-list", "--count", strings.TrimSpace(before)+"..HEAD")
			n, _ := strconv.Atoi(strings.TrimSpace(count))
			return Row{"merged_commits": n, "conflict": conflict}, nil
		}
		return Row{"message": "rebase completed", "conflict": conflict}, nil
	case "git_current_user":
		value, e := run("config", "user.name")
		if e != nil {
			return nil, nil
		}
		return strings.TrimSpace(value), nil
	case "git_commit_files":
		commit, e := ref("commit")
		if e != nil {
			return nil, e
		}
		return gitChanges(path, commit)
	case "git_commit_branches":
		commit, e := ref("commit")
		if e != nil {
			return nil, e
		}
		value, e := run("branch", "-a", "--contains", commit, "--format=%(refname:short)")
		return gitLines(value), e
	case "git_search_authors":
		value, e := run("shortlog", "-sne", "--all")
		if e != nil {
			return nil, e
		}
		rows := []string{}
		limit := number(a["limit"])
		if limit <= 0 {
			limit = 30
		}
		for _, line := range strings.Split(value, "\n") {
			parts := strings.SplitN(strings.TrimSpace(line), "\t", 2)
			if len(parts) == 2 && strings.Contains(strings.ToLower(parts[1]), strings.ToLower(text(a["query"]))) {
				rows = append(rows, parts[1])
				if len(rows) >= limit {
					break
				}
			}
		}
		return rows, nil
	case "git_log":
		limit := number(a["limit"])
		if limit <= 0 {
			limit = 50
		}
		limit = min(limit, 500)
		args := []string{"log", "--date=iso-strict", "--format=%h%x00%H%x00%an%x00%aI%x00%s%x00", "-n", strconv.Itoa(limit), "--skip=" + strconv.Itoa(max(0, number(a["skip"])))}
		if a["allBranches"] == true {
			args = append(args, "--all")
		} else if branch := text(a["branch"]); branch != "" {
			if e := gitRef(branch); e != nil {
				return nil, e
			}
			args = append(args, branch)
		}
		if author := text(a["author"]); author != "" {
			args = append(args, "--author="+author, "--fixed-strings")
		}
		args = append(args, "--")
		value, e := run(args...)
		if e != nil {
			if _, headErr := run("rev-parse", "--verify", "HEAD"); headErr != nil {
				return Row{"entries": []Row{}, "has_upstream": false}, nil
			}
			return nil, e
		}
		_, upstreamErr := run("rev-parse", "--verify", "@{upstream}")
		unpushed := map[string]bool{}
		if upstreamErr == nil {
			diff, _ := run("rev-list", "@{upstream}..HEAD")
			for _, id := range gitLines(diff) {
				unpushed[id] = true
			}
		}
		entries := []Row{}
		p := strings.Split(value, "\x00")
		for i := 0; i+4 < len(p); i += 5 {
			hash := strings.TrimSpace(p[i])
			if hash == "" {
				continue
			}
			files := []Row{}
			if a["withFiles"] == true {
				files, _ = gitChanges(path, p[i+1])
			}
			var pushed any
			if upstreamErr == nil {
				pushed = !unpushed[p[i+1]]
			}
			entries = append(entries, Row{"hash": hash, "full_hash": p[i+1], "author": p[i+2], "date": p[i+3], "message": p[i+4], "files": files, "pushed": pushed})
		}
		return Row{"entries": entries, "has_upstream": upstreamErr == nil}, nil
	case "git_list_remotes":
		names, e := run("remote")
		if e != nil {
			return nil, e
		}
		rows := []Row{}
		for _, name := range gitLines(names) {
			url, e := run("remote", "get-url", name)
			if e != nil {
				return nil, e
			}
			rows = append(rows, Row{"name": name, "url": strings.TrimSpace(url)})
		}
		return rows, nil
	case "git_add_remote", "git_remove_remote", "git_set_remote_url":
		name, e := ref("name")
		if e != nil {
			return nil, e
		}
		switch command {
		case "git_remove_remote":
			return result("remote", "remove", name)
		case "git_add_remote":
			return result("remote", "add", name, text(a["url"]))
		default:
			return result("remote", "set-url", name, text(a["url"]))
		}
	case "git_fetch", "git_fetch_remote", "git_pull", "git_push":
		if credentials, ok := a["credentials"].(map[string]any); ok && len(credentials) > 0 {
			return nil, fmt.Errorf("当前移植使用系统 Git 凭据配置，尚未接入自定义凭据")
		}
		if command == "git_fetch" {
			return run("fetch", "--all")
		}
		remote := text(a["remote"])
		if remote != "" {
			if e := gitRef(remote); e != nil {
				return nil, e
			}
		}
		if command == "git_fetch_remote" {
			return run("fetch", remote)
		}
		if command == "git_pull" {
			before, _ := run("rev-parse", "HEAD")
			_, e := run("pull", "--no-rebase")
			conflict, _ := s.gitCommand("git_list_conflicts", Row{"path": path})
			if e != nil && conflict.(Row)["has_conflicts"] != true {
				return nil, e
			}
			files, _ := run("diff", "--name-only", strings.TrimSpace(before), "HEAD", "--")
			return Row{"updated_files": len(gitLines(files)), "conflict": conflict}, nil
		}
		branch := text(a["branch"])
		if branch == "" {
			value, e := run("symbolic-ref", "--short", "HEAD")
			if e != nil {
				return nil, e
			}
			branch = strings.TrimSpace(value)
		}
		if e := gitRef(branch); e != nil {
			return nil, e
		}
		if remote == "" {
			value, _ := run("config", "branch."+branch+".remote")
			remote = strings.TrimSpace(value)
			if remote == "" {
				remote = "origin"
			}
		}
		count, _ := run("rev-list", "--count", remote+"/"+branch+".."+branch)
		n, _ := strconv.Atoi(strings.TrimSpace(count))
		_, e := run("push", "--set-upstream", remote, branch)
		return Row{"pushed_commits": n, "upstream_set": e == nil}, e
	case "git_push_info":
		branch := text(a["branch"])
		if branch == "" {
			value, e := run("symbolic-ref", "--short", "HEAD")
			if e != nil {
				return nil, e
			}
			branch = strings.TrimSpace(value)
		}
		if e := gitRef(branch); e != nil {
			return nil, e
		}
		remotes, e := s.gitCommand("git_list_remotes", Row{"path": path})
		if e != nil {
			return nil, e
		}
		remote, _ := run("config", "branch."+branch+".remote")
		return Row{"branch": branch, "remotes": remotes, "tracking_remote": nullable(remote)}, nil
	case "git_stash_push":
		args := []string{"stash", "push", "--include-untracked"}
		if a["keepIndex"] == true {
			args = append(args, "--keep-index")
		}
		if msg := text(a["message"]); msg != "" {
			args = append(args, "-m", msg)
		}
		return run(args...)
	case "git_stash_list":
		value, e := run("stash", "list", "--format=%gd%x00%gs%x00%ci")
		if e != nil {
			return nil, e
		}
		rows := []Row{}
		for i, line := range gitLines(value) {
			parts := strings.Split(line, "\x00")
			if len(parts) < 3 {
				continue
			}
			branch := ""
			if p := strings.SplitN(parts[1], ":", 2); len(p) == 2 {
				branch = strings.TrimPrefix(p[0], "On ")
			}
			rows = append(rows, Row{"index": i, "ref_name": parts[0], "message": parts[1], "branch": branch, "date": parts[2]})
		}
		return rows, nil
	case "git_stash_pop", "git_stash_apply", "git_stash_drop", "git_stash_show", "git_stash_clear":
		op := strings.TrimPrefix(command, "git_stash_")
		args := []string{"stash", op}
		if op == "show" {
			args = append(args, "-p")
		}
		if stash := text(a["stashRef"]); stash != "" {
			if e := gitRef(stash); e != nil {
				return nil, e
			}
			args = append(args, stash)
		}
		return run(args...)
	case "git_list_conflicts":
		files, e := run("diff", "--name-only", "--diff-filter=U", "-z")
		if e != nil {
			return nil, e
		}
		list := []string{}
		for _, f := range strings.Split(files, "\x00") {
			if f != "" {
				list = append(list, f)
			}
		}
		operation := ""
		for _, op := range []struct{ file, name string }{{"MERGE_HEAD", "merge"}, {"rebase-merge", "rebase"}, {"rebase-apply", "rebase"}, {"CHERRY_PICK_HEAD", "cherry-pick"}} {
			resolved, _ := run("rev-parse", "--git-path", op.file)
			p := strings.TrimSpace(resolved)
			if !filepath.IsAbs(p) {
				p = filepath.Join(path, p)
			}
			if _, e := os.Stat(p); e == nil {
				operation = op.name
				break
			}
		}
		return Row{"has_conflicts": len(list) > 0, "conflicted_files": list, "operation": operation}, nil
	case "git_has_merge_head":
		value, e := run("rev-parse", "--verify", "MERGE_HEAD")
		return e == nil && strings.TrimSpace(value) != "", nil
	case "git_conflict_file_versions":
		versions := Row{}
		for name, stage := range map[string]string{"base": ":1:", "ours": ":2:", "theirs": ":3:"} {
			value, _ := run("show", stage+file)
			versions[name] = value
		}
		p, e := s.resolveFile(path, file)
		if e != nil {
			return nil, e
		}
		data, e := os.ReadFile(p)
		if e != nil {
			return nil, e
		}
		versions["merged"] = string(data)
		return versions, nil
	case "git_resolve_conflict":
		p, e := s.resolveFile(path, file)
		if e != nil {
			return nil, e
		}
		if e = os.WriteFile(p, []byte(text(a["content"])), 0644); e != nil {
			return nil, e
		}
		return result("add", "--", file)
	case "git_abort_operation", "git_continue_operation":
		op := text(a["operation"])
		if op != "merge" && op != "rebase" && op != "cherry-pick" {
			return nil, fmt.Errorf("Git 操作类型无效")
		}
		flag := "--abort"
		if command == "git_continue_operation" {
			flag = "--continue"
		}
		return result(op, flag)
	case "git_reset":
		commit, e := ref("commit")
		if e != nil {
			return nil, e
		}
		mode := text(a["mode"])
		if mode != "soft" && mode != "mixed" && mode != "hard" && mode != "keep" {
			return nil, fmt.Errorf("Reset 模式无效")
		}
		return result("reset", "--"+mode, commit, "--")
	case "git_worktree_add":
		if e := gitRef(branchName); e != nil {
			return nil, e
		}
		target, e := s.Resolve(text(a["worktreePath"]))
		if e != nil {
			return nil, e
		}
		return result("worktree", "add", "--", target, branchName)
	}
	return nil, fmt.Errorf("Go 移植尚未实现此 Git 接口：%s", command)
}
func array(v any) []any { list, _ := v.([]any); return list }

func gitWindow(command string, a Row) Row {
	page := strings.TrimSuffix(strings.TrimPrefix(command, "open_"), "_window")
	query := url.Values{}
	query.Set("folderId", fmt.Sprint(number(a["folderId"])))
	for _, key := range []string{"operation", "upstreamCommit", "branch"} {
		if v := text(a[key]); v != "" {
			query.Set(key, v)
		}
	}
	return Row{"path": "/" + page + "?" + query.Encode()}
}
