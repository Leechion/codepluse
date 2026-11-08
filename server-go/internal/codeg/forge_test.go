package codeg

import (
	"strings"
	"fmt"
	"net/http"
	"net/http/httptest"
	"testing"
)

// mockGitHub serves a tiny fake of the GitHub REST endpoints the forge uses.
func mockGitHub(t *testing.T) *httptest.Server {
	t.Helper()
	mux := http.NewServeMux()
	check := func(token string) bool { return token == "tok-good" }
	mux.HandleFunc("/user", func(w http.ResponseWriter, r *http.Request) {
		if r.Header.Get("Authorization") != "Bearer tok-good" {
			w.WriteHeader(401)
			_, _ = w.Write([]byte(`{"message":"Bad credentials"}`))
			return
		}
		_, _ = w.Write([]byte(`{"login":"octo","name":"Octo Cat","avatar_url":"https://example.com/a.png"}`))
	})
	mux.HandleFunc("/search/issues", func(w http.ResponseWriter, r *http.Request) {
		if !check(strings.TrimPrefix(r.Header.Get("Authorization"), "Bearer ")) {
			w.WriteHeader(401)
			return
		}
		fmt.Fprintf(w, `{"total_count":1200,"items":[{"number":7,"title":"修复登录","state":"open","html_url":"https://github.com/o/r/issues/7","user":{"login":"alice","avatar_url":"https://example.com/x.png"},"labels":[{"name":"bug","color":"ff0000"}],"comments":2,"updated_at":"2026-01-01T00:00:00Z","body":"登录失败"}]}`)
	})
	mux.HandleFunc("/repos/o/r/labels", func(w http.ResponseWriter, r *http.Request) {
		_, _ = w.Write([]byte(`[{"name":"bug","color":"ff0000"},{"name":"ui","color":"00ff00"}]`))
	})
	mux.HandleFunc("/repos/o/r/issues/7/comments", func(w http.ResponseWriter, r *http.Request) {
		if r.Method == "POST" {
			_, _ = w.Write([]byte(`{"id":99,"user":{"login":"octo","avatar_url":"https://example.com/a.png"},"body":"马上处理","created_at":"2026-01-02T00:00:00Z","html_url":"https://github.com/o/r/issues/7#c"}`))
			return
		}
		_, _ = w.Write([]byte(`[{"id":5,"user":{"login":"bob","avatar_url":"https://example.com/b.png"},"body":"我也遇到了","created_at":"2026-01-01T01:00:00Z","html_url":"https://github.com/o/r/issues/7#c1"}]`))
	})
	mux.HandleFunc("/repos/o/r/issues/7", func(w http.ResponseWriter, r *http.Request) {
		if r.Method == "PATCH" {
			_, _ = w.Write([]byte(`{"number":7,"title":"修复登录","state":"closed","html_url":"https://github.com/o/r/issues/7","user":{"login":"alice"},"labels":[],"comments":2,"updated_at":"2026-01-03T00:00:00Z","body":""}`))
			return
		}
		_, _ = w.Write([]byte(`{"number":7,"title":"修复登录","state":"open","html_url":"https://github.com/o/r/issues/7","user":{"login":"alice","avatar_url":"https://example.com/x.png"},"labels":[],"comments":2,"updated_at":"2026-01-01T00:00:00Z","body":"登录失败"}`))
	})
	mux.HandleFunc("/repos/o/r/pulls/9/merge", func(w http.ResponseWriter, r *http.Request) {
		_, _ = w.Write([]byte(`{"merged":true,"sha":"abc123"}`))
	})
	mux.HandleFunc("/repos/o/r/pulls/9", func(w http.ResponseWriter, r *http.Request) {
		_, _ = w.Write([]byte(`{"number":9,"title":"PR","state":"closed","merged_at":"2026-01-04T00:00:00Z","html_url":"https://github.com/o/r/pull/9","user":{"login":"carol"},"labels":[],"head":{"ref":"feat","sha":"abc123","repo":{"full_name":"o/r"}},"base":{"ref":"main","sha":"def456","repo":{"full_name":"o/r"}},"mergeable":true,"mergeable_state":"clean","draft":false,"body":""}`))
	})
	server := httptest.NewServer(mux)
	t.Cleanup(server.Close)
	return server
}

func forgeTestServer(t *testing.T) *Server {
	server := mockGitHub(t)
	previous := githubAPI
	githubAPI = server.URL
	t.Cleanup(func() { githubAPI = previous })
	s := testServer(t)
	command(t, s, "open_folder", Row{"path": s.Root})
	folders := command(t, s, "list_all_folder_details", Row{}).([]any)
	folderID := int(folders[0].(map[string]any)["id"].(float64))
	// The folder's origin remote points at the mock host mapping.
	s.forge.mu.Lock()
	s.forge.accounts = []*ForgeAccount{{ID: 1, Kind: "github", Name: "主账户", Login: "octo", Token: "tok-good"}}
	s.forge.mu.Unlock()
	// Point the folder remote at o/r via a real git remote in the temp repo.
	initGitRepo(t, s.Root)
	_, _ = execGit(s.Root, "remote", "add", "origin", "https://github.com/o/r.git")
	_ = folderID
	return s
}

func TestForgeListIssuesAndTabs(t *testing.T) {
	s := forgeTestServer(t)
	list := command(t, s, "forge_list_issues", Row{"folderId": 1, "query": Row{"tab": "issues", "state": "open", "page": 1, "perPage": 30}}).(map[string]any)
	rows := list["rows"].([]any)
	if len(rows) != 1 {
		t.Fatalf("应有 1 行：%v", rows)
	}
	row := rows[0].(map[string]any)
	if row["title"] != "修复登录" || row["is_pr"] != false {
		t.Fatalf("行数据异常：%v", row)
	}
	if list["total_count"].(float64) != 1200 {
		t.Fatalf("总数应为 1200：%v", list["total_count"])
	}
	if list["reachable_count"].(float64) != 1000 {
		t.Fatalf("可达数应截断为 1000：%v", list["reachable_count"])
	}
	count := command(t, s, "forge_tab_count", Row{"folderId": 1, "query": Row{"tab": "issues", "state": "open"}}).(map[string]any)
	if count["count"].(float64) != 1200 {
		t.Fatalf("tab 计数异常：%v", count)
	}
}

func TestForgeCommentsAndState(t *testing.T) {
	s := forgeTestServer(t)
	comments := command(t, s, "forge_list_comments", Row{"folderId": 1, "filters": Row{"kind": "issue", "number": 7, "page": 1, "perPage": 30}}).(map[string]any)
	if len(comments["comments"].([]any)) != 1 {
		t.Fatalf("评论数异常：%v", comments)
	}
	created := command(t, s, "forge_create_comment", Row{"folderId": 1, "draft": Row{"kind": "issue", "number": 7, "body": "马上处理"}}).(map[string]any)
	if created["author"] != "octo" {
		t.Fatalf("评论作者异常：%v", created)
	}
	state := command(t, s, "forge_set_item_state", Row{"folderId": 1, "request": Row{"kind": "issue", "number": 7, "action": "close"}}).(map[string]any)
	if state["state"] != "closed" {
		t.Fatalf("状态切换异常：%v", state)
	}
}

func TestForgeMergeChange(t *testing.T) {
	s := forgeTestServer(t)
	row := command(t, s, "forge_merge_change", Row{"folderId": 1, "request": Row{"number": 9, "method": "squash", "headSha": "abc123"}}).(map[string]any)
	if row["state"] != "merged" {
		t.Fatalf("合并后状态应为 merged：%v", row)
	}
}

func TestForgeRemoteAndIdentity(t *testing.T) {
	s := forgeTestServer(t)
	remote := command(t, s, "folder_forge_remote", Row{"folderId": 1}).(map[string]any)
	if remote["server_host"] != "github.com" || remote["owner_repo"] != "o/r" {
		t.Fatalf("远程解析异常：%v", remote)
	}
	identity := command(t, s, "forge_identity", Row{"folderId": 1}).(map[string]any)
	if identity["username"] != "octo" {
		t.Fatalf("身份异常：%v", identity)
	}
}
