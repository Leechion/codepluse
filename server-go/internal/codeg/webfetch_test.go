package codeg

import (
	"context"
	"net/http"
	"net/http/httptest"
	"strings"
	"testing"
)

func TestFetchURLReturnsReadablePageText(t *testing.T) {
	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		w.Header().Set("Content-Type", "text/html; charset=utf-8")
		_, _ = w.Write([]byte(`<html><head><title>标题</title><style>.a{color:red}</style></head>` +
			`<body><script>var x = 1;</script><h1>苏州今天</h1><p>多云 &amp; 晴，气温 28&#176;C</p></body></html>`))
	}))
	defer server.Close()

	s := testServer(t)
	row, err := s.fetchURL(Row{"url": server.URL + "/weather"})
	if err != nil {
		t.Fatal(err)
	}
	text := text(row["text"])
	for _, want := range []string{"苏州今天", "多云 & 晴", "28°C"} {
		if !strings.Contains(text, want) {
			t.Fatalf("正文缺少 %q：%s", want, text)
		}
	}
	for _, unwanted := range []string{"var x = 1", "color:red", "<h1>"} {
		if strings.Contains(text, unwanted) {
			t.Fatalf("正文残留标记 %q：%s", unwanted, text)
		}
	}
	if number(row["status"]) != 200 {
		t.Fatalf("状态码不对：%v", row["status"])
	}
}

func TestFetchURLKeepsContentAfterHTMLComments(t *testing.T) {
	// A single-line comment ends at its own '>'. Treating the comment as a
	// "skip until the next -->" swallows everything up to the next comment on
	// the page — which is how a whole forecast table once went missing.
	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		w.Header().Set("Content-Type", "text/html; charset=utf-8")
		_, _ = w.Write([]byte(`<html><body><!-- tong end--><ul class="t"><li><h1>13日（今天）</h1>` +
			`<p class="wea">阴转小雨</p></li></ul><!-- next--><p>尾部</p></body></html>`))
	}))
	defer server.Close()
	s := testServer(t)
	row, err := s.fetchURL(Row{"url": server.URL})
	if err != nil {
		t.Fatal(err)
	}
	body := text(row["text"])
	for _, want := range []string{"13日（今天）", "阴转小雨", "尾部"} {
		if !strings.Contains(body, want) {
			t.Fatalf("正文缺少 %q：%s", want, body)
		}
	}
}

func TestFetchURLRejectsNonHTTPAndEmptyURL(t *testing.T) {
	s := testServer(t)
	for _, url := range []string{"", "file:///c:/secret", "ftp://example.test/x", "not a url"} {
		if _, err := s.fetchURL(Row{"url": url}); err == nil {
			t.Fatalf("接受了非法 URL：%q", url)
		}
	}
}

func TestFetchURLIsReachableThroughTheCompanionTool(t *testing.T) {
	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		_, _ = w.Write([]byte("plain body"))
	}))
	defer server.Close()
	s := testServer(t)
	caller := &Caller{ConnectionID: "c1", RootID: "r1"}
	out, err := s.tool(context.Background(), caller, "fetch_url", Row{"url": server.URL})
	if err != nil {
		t.Fatal(err)
	}
	row := out.(map[string]any)
	if !strings.Contains(text(row["text"]), "plain body") {
		t.Fatalf("工具返回了别的东西：%v", row["text"])
	}
}

func TestFetchURLToolIsAdvertisedToAgents(t *testing.T) {
	names := map[string]bool{}
	for _, tool := range ToolDefinitions() {
		names[text(tool["name"])] = true
	}
	if !names["fetch_url"] {
		t.Fatal("codeg-mcp 未向智能体提供 fetch_url")
	}
}
