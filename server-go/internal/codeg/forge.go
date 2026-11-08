// Forge: the Issue/PR workbench web contract (codeg-ui forge components).
// GitHub is implemented against the REST v3 API; GitLab/CNB report clearly
// that they are not supported yet. Account tokens live in memory only — a
// restart asks for re-entry — and never reach logs or events.
package codeg

import (
	"encoding/json"
	"fmt"
	"strings"
	"sync"
)

type ForgeAccount struct {
	ID        int    `json:"id"`
	Kind      string `json:"kind"` // github | gitlab | cnb
	Name      string `json:"name"`
	ServerURL string `json:"server_url,omitempty"`
	Login     string `json:"login,omitempty"`
	Token     string `json:"-"` // never serialized back to the client
}

// forgeStore keeps accounts in memory; settings (folder → pinned account and
// panel preferences) persist in .data/codeg-forge.json without tokens.
type forgeStore struct {
	mu       sync.Mutex
	root     string
	nextID   int
	accounts []*ForgeAccount
	Settings map[string]Row `json:"settings"`
	Global   Row            `json:"global"`
}

func newForgeStore(root string) *forgeStore {
	st := &forgeStore{root: root, nextID: 1, Settings: map[string]Row{}, Global: Row{}}
	if raw, err := readJSONFileData(root + string(filepathSep) + ".data" + string(filepathSep) + "codeg-forge.json"); err == nil {
		var saved struct {
			Settings map[string]Row `json:"settings"`
			Global   Row            `json:"global"`
		}
		if json.Unmarshal(raw, &saved) == nil {
			if saved.Settings != nil {
				st.Settings = saved.Settings
			}
			if saved.Global != nil {
				st.Global = saved.Global
			}
		}
	}
	return st
}

func (st *forgeStore) saveSettings() {
	_ = atomicWrite(st.root+string(filepathSep)+".data"+string(filepathSep)+"codeg-forge.json",
		Row{"settings": st.Settings, "global": st.Global})
}

func (st *forgeStore) listAccounts(kind string) []Row {
	out := []Row{}
	st.mu.Lock()
	defer st.mu.Unlock()
	for _, account := range st.accounts {
		if kind == "" || account.Kind == kind {
			out = append(out, Row{"id": fmt.Sprint(account.ID), "kind": account.Kind, "name": account.Name,
				"server_url": account.ServerURL, "login": account.Login})
		}
	}
	return out
}

// accountFor picks the token for a request: the explicit accountId when given,
// otherwise the first GitHub account on file.
func (st *forgeStore) accountFor(accountID string) (*ForgeAccount, error) {
	st.mu.Lock()
	defer st.mu.Unlock()
	if accountID != "" {
		for _, account := range st.accounts {
			if fmt.Sprint(account.ID) == accountID && account.Kind == "github" {
				return account, nil
			}
		}
		return nil, fmt.Errorf("找不到指定的 GitHub 账户")
	}
	for _, account := range st.accounts {
		if account.Kind == "github" {
			return account, nil
		}
	}
	return nil, fmt.Errorf("尚未配置 GitHub 账户，请先在设置里添加 token")
}

// ----------------------------- remote parsing -----------------------------

// folderRemote parses the folder's origin remote into forge coordinates.
func (s *Server) folderRemote(folderID int) (host, ownerRepo string, err error) {
	row := s.folderRow(folderID)
	path := text(row["path"])
	if path == "" {
		return "", "", fmt.Errorf("项目文件夹不存在")
	}
	out, err := execGit(path, "remote", "get-url", "origin")
	if err != nil {
		return "", "", fmt.Errorf("读取 origin 远程失败：%v", err)
	}
	return parseGitHubRemote(strings.TrimSpace(out))
}

func parseGitHubRemote(url string) (string, string, error) {
	u := strings.TrimSpace(url)
	u = strings.TrimSuffix(u, ".git")
	if strings.HasPrefix(u, "git@") {
		rest := strings.TrimPrefix(u, "git@")
		if i := strings.Index(rest, ":"); i > 0 {
			return rest[:i], rest[i+1:], nil
		}
	}
	for _, prefix := range []string{"https://", "http://"} {
		if strings.HasPrefix(u, prefix) {
			rest := strings.TrimPrefix(u, prefix)
			if i := strings.Index(rest, "/"); i > 0 {
				host := rest[:i]
				repo := strings.Trim(rest[i+1:], "/")
				if strings.Count(repo, "/") >= 1 {
					return host, repo, nil
				}
			}
		}
	}
	return "", "", fmt.Errorf("无法从远程地址解析仓库：%s", url)
}

// ----------------------------- commands -----------------------------

func (s *Server) forgeCommand(command string, a Row) (any, error) {
	switch command {
	case "get_github_accounts":
		return s.forge.listAccounts("github"), nil
	case "update_github_accounts":
		return nil, s.forgeUpdateAccounts(a["settings"])
	case "forge_identity":
		login, avatar, err := s.forgeIdentity(text(a["accountId"]))
		if err != nil {
			return nil, err
		}
		return Row{"username": login, "avatar_url": avatar}, nil
	case "folder_forge_remote":
		host, repo, err := s.folderRemote(number(a["folderId"]))
		if err != nil {
			return nil, err
		}
		return Row{"provider": "github", "server_host": host, "owner_repo": repo}, nil
	case "forge_settings_get":
		folderKey := fmt.Sprint(number(a["folderId"]))
		s.forge.mu.Lock()
		defer s.forge.mu.Unlock()
		return Row{"global": s.forge.Global, "folder": s.forge.Settings[folderKey]}, nil
	case "forge_settings_set":
		folderID := number(a["folderId"])
		settings, _ := a["settings"].(Row)
		s.forge.mu.Lock()
		if folderID == 0 {
			if settings == nil {
				return nil, fmt.Errorf("全局设置不能删除")
			}
			s.forge.Global = settings
		} else if settings == nil {
			delete(s.forge.Settings, fmt.Sprint(folderID))
		} else {
			s.forge.Settings[fmt.Sprint(folderID)] = settings
		}
		s.forge.saveSettings()
		s.forge.mu.Unlock()
		return s.forgeSettingsSnapshot(), nil
	case "forge_auto_status":
		return Row{"enabled": false, "reason": "自动处理尚未接通"}, nil
	case "forge_auto_scan_now":
		return Row{"scanned": 0, "created": 0}, nil
	}

	// Everything below talks to the forge; resolve the repository first.
	folderID := number(a["folderId"])
	host, repo, err := s.folderRemote(folderID)
	if err != nil {
		return nil, err
	}
	if host != "github.com" && !strings.HasSuffix(host, ".github.com") {
		return nil, fmt.Errorf("Forge 工作台当前仅支持 GitHub（该远程指向 %s）", host)
	}

	switch command {
	case "forge_list_issues":
		query, _ := a["query"].(Row)
		return s.forgeListIssues(repo, query)
	case "forge_tab_count":
		query, _ := a["query"].(Row)
		if query == nil {
			query = Row{}
		}
		return s.forgeTabCount(repo, query)
	case "forge_issue_detail":
		query, _ := a["query"].(Row)
		if query == nil {
			query = Row{}
		}
		return s.forgeIssueDetail(repo, query)
	case "forge_list_labels":
		return s.forgeListLabels(repo, text(a["accountId"]))
	case "forge_list_comments":
		filters, _ := a["filters"].(Row)
		return s.forgeListComments(repo, filters)
	case "forge_create_comment":
		draft, _ := a["draft"].(Row)
		return s.forgeCreateComment(repo, draft)
	case "forge_set_item_state":
		request, _ := a["request"].(Row)
		return s.forgeSetItemState(repo, request)
	case "forge_create_issue":
		draft, _ := a["draft"].(Row)
		return s.forgeCreateIssue(repo, draft)
	case "forge_change_detail":
		query, _ := a["query"].(Row)
		return s.forgeChangeDetail(repo, query)
	case "forge_change_files":
		query, _ := a["query"].(Row)
		return s.forgeChangeFiles(repo, query)
	case "forge_merge_options":
		return s.forgeMergeOptions(repo, text(a["accountId"]))
	case "forge_merge_change":
		request, _ := a["request"].(Row)
		return s.forgeMergeChange(repo, request)
	}
	return nil, fmt.Errorf("未知的 Forge 命令：%s", command)
}

func (s *Server) forgeSettingsSnapshot() Row {
	folder := Row{}
	for key, value := range s.forge.Settings {
		folder[key] = value
	}
	return Row{"global": s.forge.Global, "folder": folder}
}

// forgeUpdateAccounts replaces the account list after validating each token.
func (s *Server) forgeUpdateAccounts(settings any) error {
	raw, _ := settings.(Row)
	entries, _ := raw["accounts"].([]any)
	next := []*ForgeAccount{}
	seen := map[string]bool{}
	for _, entry := range entries {
		item, ok := entry.(Row)
		if !ok {
			continue
		}
		name := strings.TrimSpace(text(item["name"]))
		token := text(item["token"])
		if token == "" {
			// Keep an existing account when its token is not re-entered.
			if id := text(item["id"]); id != "" {
				if old, err := s.forge.accountFor(id); err == nil {
					next = append(next, old)
					seen[fmt.Sprint(old.ID)] = true
					continue
				}
			}
			return fmt.Errorf("账户 %s 缺少 token", name)
		}
		checked, err := s.forgeValidateToken(token)
		if err != nil {
			return err
		}
		result, _ := checked.(Row)
		valid, _ := result["valid"].(bool)
		if !valid {
			return fmt.Errorf("token 校验失败：%s", text(result["message"]))
		}
		login := text(result["login"])
		id := 0
		if text(item["id"]) != "" {
			fmt.Sscanf(text(item["id"]), "%d", &id)
		} else {
			id = s.forge.nextID
			s.forge.nextID++
		}
		next = append(next, &ForgeAccount{ID: id, Kind: "github", Name: name, Login: login, Token: token})
		seen[fmt.Sprint(id)] = true
	}
	s.forge.mu.Lock()
	s.forge.accounts = next
	s.forge.mu.Unlock()
	return nil
}
