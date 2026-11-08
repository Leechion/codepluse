package codeg

import (
	"encoding/json"
	"fmt"
	"io"
	"net/http"
	"net/url"
	"strings"
	"time"
)

const filepathSep = '/'

// githubAPI is a var so tests can point the client at a mock server.
var githubAPI = "https://api.github.com"

// githubClient performs one authenticated GitHub REST v3 request and decodes
// the JSON body into a Row (or []Row via the raw flag).
func (s *Server) githubRequest(accountID, method, path string, body any, out any) error {
	account, err := s.forge.accountFor(accountID)
	if err != nil {
		return err
	}
	var reader io.Reader
	if body != nil {
		b, err := json.Marshal(body)
		if err != nil {
			return err
		}
		reader = strings.NewReader(string(b))
	}
	request, err := http.NewRequest(method, githubAPI+path, reader)
	if err != nil {
		return err
	}
	request.Header.Set("Authorization", "Bearer "+account.Token)
	request.Header.Set("Accept", "application/vnd.github+json")
	request.Header.Set("X-GitHub-Api-Version", "2022-11-28")
	if body != nil {
		request.Header.Set("Content-Type", "application/json")
	}
	client := &http.Client{Timeout: 30 * time.Second}
	response, err := client.Do(request)
	if err != nil {
		return fmt.Errorf("无法连接 GitHub：%v", err)
	}
	defer response.Body.Close()
	data, _ := io.ReadAll(io.LimitReader(response.Body, 8<<20))
	if response.StatusCode >= 300 {
		message := strings.TrimSpace(string(data))
		if len(message) > 300 {
			message = message[:300]
		}
		return fmt.Errorf("GitHub %s %s 返回 %d：%s", method, path, response.StatusCode, message)
	}
	if out != nil && len(data) > 0 {
		return json.Unmarshal(data, out)
	}
	return nil
}

// forgeValidateToken checks a GitHub personal access token against GET /user.
func (s *Server) forgeValidateToken(token string) (any, error) {
	if strings.TrimSpace(token) == "" {
		return nil, fmt.Errorf("请填写 GitHub token")
	}
	request, _ := http.NewRequest("GET", githubAPI+"/user", nil)
	request.Header.Set("Authorization", "Bearer "+token)
	request.Header.Set("Accept", "application/vnd.github+json")
	client := &http.Client{Timeout: 15 * time.Second}
	response, err := client.Do(request)
	if err != nil {
		return nil, fmt.Errorf("无法连接 GitHub：%v", err)
	}
	defer response.Body.Close()
	body, _ := io.ReadAll(io.LimitReader(response.Body, 1<<20))
	if response.StatusCode == 401 {
		return Row{"valid": false, "message": "token 无效或已过期"}, nil
	}
	if response.StatusCode != 200 {
		return Row{"valid": false, "message": fmt.Sprintf("GitHub 返回 %d", response.StatusCode)}, nil
	}
	var user struct {
		Login     string `json:"login"`
		Name      string `json:"name"`
		AvatarURL string `json:"avatar_url"`
	}
	_ = json.Unmarshal(body, &user)
	return Row{"valid": true, "login": user.Login, "name": user.Name, "avatar_url": user.AvatarURL}, nil
}

func (s *Server) forgeIdentity(accountID string) (string, string, error) {
	var user Row
	if err := s.githubRequest(accountID, "GET", "/user", nil, &user); err != nil {
		return "", "", err
	}
	return text(user["login"]), text(user["avatar_url"]), nil
}

// forgeRow converts a GitHub issue/PR payload into the wire row. The pulls
// endpoint has no `pull_request` marker — head+base identify it instead.
func forgeRow(item Row) Row {
	isPR := item["pull_request"] != nil || item["head"] != nil
	labels := []Row{}
	if raw, ok := item["labels"].([]any); ok {
		for _, l := range raw {
			if label, ok := l.(Row); ok {
				color := text(label["color"])
				if color != "" {
					color = "#" + color
				}
				labels = append(labels, Row{"name": text(label["name"]), "color": color})
			}
		}
	}
	author, avatar := "", ""
	if user, ok := item["user"].(Row); ok {
		author = text(user["login"])
		avatar = text(user["avatar_url"])
	}
	state := text(item["state"])
	if isPR && item["merged_at"] != nil {
		state = "merged"
	}
	draft := false
	if pr, ok := item["pull_request"].(Row); ok {
		draft, _ = pr["draft"].(bool)
	}
	body := text(item["body"])
	if len(body) > 2000 {
		body = body[:2000]
	}
	return Row{"number": item["number"], "title": item["title"], "body": body, "state": state,
		"draft": draft, "labels": labels, "author": author, "author_avatar": avatar,
		"updated_at": item["updated_at"], "html_url": item["html_url"], "is_pr": isPR,
		"comments": item["comments"]}
}

// forgeListIssues serves one page of the issues or PRs tab via GitHub search.
func (s *Server) forgeListIssues(repo string, query Row) (any, error) {
	tab := text(query["tab"])
	if tab == "" {
		tab = "issues"
	}
	state := text(query["state"])
	if state == "" {
		state = "open"
	}
	page := number(query["page"])
	if page < 1 {
		page = 1
	}
	perPage := number(query["perPage"])
	if perPage < 1 || perPage > 100 {
		perPage = 30
	}
	terms := []string{"repo:" + repo, "is:" + tab, "state:" + state}
	if query["assignedMe"] == true {
		terms = append(terms, "assignee:@me")
	}
	if labels, ok := query["labels"].([]any); ok {
		for _, l := range labels {
			terms = append(terms, `label:"`+text(l)+`"`)
		}
	}
	if search := text(query["search"]); search != "" {
		terms = append(terms, search)
	}
	sort := text(query["sort"])
	endpoint := fmt.Sprintf("/search/issues?q=%s&page=%d&per_page=%d",
		url.QueryEscape(strings.Join(terms, " ")), page, perPage)
	switch sort {
	case "updated":
		endpoint += "&sort=updated&order=desc"
	case "created":
		endpoint += "&sort=created&order=desc"
	case "comments":
		endpoint += "&sort=comments&order=desc"
	}
	var result Row
	if err := s.githubRequest(text(query["accountId"]), "GET", endpoint, nil, &result); err != nil {
		return nil, err
	}
	items, _ := result["items"].([]any)
	rows := []Row{}
	for _, raw := range items {
		if item, ok := raw.(Row); ok {
			rows = append(rows, forgeRow(item))
		}
	}
	total, _ := result["total_count"].(float64)
	reachable := int(total)
	hasNext := page*perPage < reachable
	if reachable > 1000 {
		reachable = 1000
		hasNext = page*perPage < reachable
	}
	return Row{"rows": rows, "page": page, "per_page": perPage, "total_count": int(total),
		"reachable_count": reachable, "has_next": hasNext, "incomplete": false}, nil
}

func (s *Server) forgeTabCount(repo string, query Row) (any, error) {
	tab := text(query["tab"])
	if tab == "" {
		tab = "issues"
	}
	state := text(query["state"])
	if state == "" {
		state = "open"
	}
	terms := []string{"repo:" + repo, "is:" + tab, "state:" + state}
	if query["assignedMe"] == true {
		terms = append(terms, "assignee:@me")
	}
	endpoint := "/search/issues?q=" + url.QueryEscape(strings.Join(terms, " ")) + "&per_page=1"
	var result Row
	if err := s.githubRequest(text(query["accountId"]), "GET", endpoint, nil, &result); err != nil {
		return nil, err
	}
	total, _ := result["total_count"].(float64)
	return Row{"count": int(total)}, nil
}

func (s *Server) forgeIssueDetail(repo string, query Row) (any, error) {
	var item Row
	if err := s.githubRequest(text(query["accountId"]), "GET", fmt.Sprintf("/repos/%s/issues/%d", repo, number(query["number"])), nil, &item); err != nil {
		return nil, err
	}
	row := forgeRow(item)
	row["body"] = item["body"]
	return row, nil
}

func (s *Server) forgeListLabels(repo, accountID string) (any, error) {
	var items []Row
	if err := s.githubRequest(accountID, "GET", fmt.Sprintf("/repos/%s/labels?per_page=100", repo), nil, &items); err != nil {
		return nil, err
	}
	labels := []Row{}
	for _, label := range items {
		color := text(label["color"])
		if color != "" {
			color = "#" + color
		}
		labels = append(labels, Row{"name": text(label["name"]), "color": color})
	}
	return Row{"labels": labels}, nil
}

func (s *Server) forgeListComments(repo string, filters Row) (any, error) {
	kind := text(filters["kind"])
	if kind == "" {
		kind = "issue"
	}
	number_ := number(filters["number"])
	page := number(filters["page"])
	if page < 1 {
		page = 1
	}
	perPage := number(filters["perPage"])
	if perPage < 1 || perPage > 100 {
		perPage = 30
	}
	// GitHub serves PR discussions from the issue-comments endpoint.
	var items []Row
	endpoint := fmt.Sprintf("/repos/%s/issues/%d/comments?page=%d&per_page=%d", repo, number_, page, perPage)
	if err := s.githubRequest(text(filters["accountId"]), "GET", endpoint, nil, &items); err != nil {
		return nil, err
	}
	comments := []Row{}
	for _, item := range items {
		author, avatar := "", ""
		if user, ok := item["user"].(Row); ok {
			author = text(user["login"])
			avatar = text(user["avatar_url"])
		}
		comments = append(comments, Row{"id": fmt.Sprint(number(item["id"])), "author": author,
			"author_avatar": avatar, "body": item["body"], "created_at": item["created_at"],
			"html_url": item["html_url"]})
	}
	hasNext := len(comments) == perPage
	return Row{"comments": comments, "page": page, "per_page": perPage, "has_next": hasNext}, nil
}

func (s *Server) forgeCreateComment(repo string, draft Row) (any, error) {
	kind := text(draft["kind"])
	if kind == "" {
		kind = "issue"
	}
	number_ := number(draft["number"])
	var item Row
	if err := s.githubRequest(text(draft["accountId"]), "POST", fmt.Sprintf("/repos/%s/issues/%d/comments", repo, number_),
		Row{"body": text(draft["body"])}, &item); err != nil {
		return nil, err
	}
	author, avatar := "", ""
	if user, ok := item["user"].(Row); ok {
		author = text(user["login"])
		avatar = text(user["avatar_url"])
	}
	return Row{"id": fmt.Sprint(number(item["id"])), "author": author, "author_avatar": avatar,
		"body": item["body"], "created_at": item["created_at"], "html_url": item["html_url"]}, nil
}

func (s *Server) forgeSetItemState(repo string, request Row) (any, error) {
	kind := text(request["kind"])
	number_ := number(request["number"])
	action := text(request["action"])
	state := "closed"
	if action == "reopen" {
		state = "open"
	}
	endpoint := fmt.Sprintf("/repos/%s/issues/%d", repo, number_)
	if kind == "pr" {
		endpoint = fmt.Sprintf("/repos/%s/pulls/%d", repo, number_)
	}
	var item Row
	if err := s.githubRequest(text(request["accountId"]), "PATCH", endpoint, Row{"state": state}, &item); err != nil {
		return nil, err
	}
	row := forgeRow(item)
	row["body"] = item["body"]
	return row, nil
}

func (s *Server) forgeCreateIssue(repo string, draft Row) (any, error) {
	body := Row{"title": text(draft["title"])}
	if text(draft["body"]) != "" {
		body["body"] = text(draft["body"])
	}
	if labels, ok := draft["labels"].([]any); ok && len(labels) > 0 {
		body["labels"] = labels
	}
	var item Row
	if err := s.githubRequest(text(draft["accountId"]), "POST", fmt.Sprintf("/repos/%s/issues", repo), body, &item); err != nil {
		return nil, err
	}
	return forgeRow(item), nil
}

func (s *Server) forgeChangeDetail(repo string, query Row) (any, error) {
	var pr Row
	if err := s.githubRequest(text(query["accountId"]), "GET", fmt.Sprintf("/repos/%s/pulls/%d", repo, number(query["number"])), nil, &pr); err != nil {
		return nil, err
	}
	head, _ := pr["head"].(Row)
	base, _ := pr["base"].(Row)
	headRepo := ""
	if hr, ok := head["repo"].(Row); ok {
		headRepo = text(hr["full_name"])
	}
	checks := []Row{}
	if sha := text(head["sha"]); sha != "" {
		var combined Row
		if err := s.githubRequest(text(query["accountId"]), "GET", fmt.Sprintf("/repos/%s/commits/%s/check-runs?per_page=100", repo, sha), nil, &combined); err == nil {
			if items, ok := combined["check_runs"].([]any); ok {
				for _, raw := range items {
					if run, ok := raw.(Row); ok {
						state := text(run["conclusion"])
						if state == "" {
							state = text(run["status"])
						}
						checks = append(checks, Row{"name": text(run["name"]), "state": state, "url": text(run["html_url"])})
					}
				}
			}
		}
	}
	return Row{"number": pr["number"], "base_ref": text(base["ref"]), "head_ref": text(head["ref"]),
		"head_repo": headRepo, "head_sha": head["sha"], "mergeable": pr["mergeable"],
		"mergeable_state": pr["mergeable_state"], "draft": pr["draft"], "checks": checks,
		"state": pr["state"], "merged": pr["merged"]}, nil
}

func (s *Server) forgeChangeFiles(repo string, query Row) (any, error) {
	number_ := number(query["number"])
	page := number(query["page"])
	if page < 1 {
		page = 1
	}
	perPage := number(query["perPage"])
	if perPage < 1 || perPage > 100 {
		perPage = 30
	}
	var items []Row
	if err := s.githubRequest(text(query["accountId"]), "GET",
		fmt.Sprintf("/repos/%s/pulls/%d/files?page=%d&per_page=%d", repo, number_, page, perPage), nil, &items); err != nil {
		return nil, err
	}
	files := []Row{}
	for _, item := range items {
		files = append(files, Row{"file": item["filename"], "additions": item["additions"],
			"deletions": item["deletions"], "changes": item["changes"], "status": item["status"]})
	}
	return Row{"files": files, "page": page, "per_page": perPage, "has_next": len(files) == perPage}, nil
}

func (s *Server) forgeMergeOptions(repo, accountID string) (any, error) {
	var repoRow Row
	if err := s.githubRequest(accountID, "GET", "/repos/"+repo, nil, &repoRow); err != nil {
		return nil, err
	}
	methods := []string{}
	defaultMethod := "merge"
	for _, entry := range []struct {
		flag   string
		method string
	}{{"allow_merge_commit", "merge"}, {"allow_squash_merge", "squash"}, {"allow_rebase_merge", "rebase"}} {
		if repoRow[entry.flag] == true {
			methods = append(methods, entry.method)
		}
	}
	if len(methods) > 0 {
		defaultMethod = methods[0]
	}
	return Row{"methods": methods, "default_method": defaultMethod, "merge_strategy": "commit"}, nil
}

func (s *Server) forgeMergeChange(repo string, request Row) (any, error) {
	number_ := number(request["number"])
	body := Row{"merge_method": text(request["method"])}
	if sha := text(request["headSha"]); sha != "" {
		body["sha"] = sha
	}
	endpoint := fmt.Sprintf("/repos/%s/pulls/%d/merge", repo, number_)
	var result Row
	if err := s.githubRequest(text(request["accountId"]), "PUT", endpoint, body, &result); err != nil {
		return nil, err
	}
	if result["merged"] != true {
		return nil, nil
	}
	// Read the row back; a failure here is not a failure of the merge.
	var item Row
	if err := s.githubRequest(text(request["accountId"]), "GET", fmt.Sprintf("/repos/%s/pulls/%d", repo, number_), nil, &item); err != nil {
		return nil, nil
	}
	return forgeRow(item), nil
}
