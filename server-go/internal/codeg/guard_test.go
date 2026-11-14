package codeg

import (
	"net/http/httptest"
	"strings"
	"testing"
)

func guardServer() *Server {
	return &Server{Base: "http://127.0.0.1:8840"}
}

func TestGuardRejectsCrossSiteBrowserRequests(t *testing.T) {
	s := guardServer()
	cases := []struct {
		name    string
		host    string
		origin  string
		fetch   string
		wantErr bool
	}{
		{"same origin", "127.0.0.1:8840", "http://127.0.0.1:8840", "same-origin", false},
		{"localhost alias", "localhost:8840", "http://localhost:8840", "", false},
		{"foreign origin", "127.0.0.1:8840", "http://evil.example", "cross-site", true},
		{"rebinding host", "evil.example:8840", "http://evil.example:8840", "", true},
		{"foreign port", "127.0.0.1:9999", "http://127.0.0.1:9999", "", true},
		{"cross-site fetch metadata", "127.0.0.1:8840", "", "cross-site", true},
		{"native client", "127.0.0.1:8840", "", "", false},
	}
	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			r := httptest.NewRequest("POST", "http://"+tc.host+"/api/terminal_spawn", strings.NewReader("{}"))
			r.Host = tc.host
			if tc.origin != "" {
				r.Header.Set("Origin", tc.origin)
			}
			if tc.fetch != "" {
				r.Header.Set("Sec-Fetch-Site", tc.fetch)
			}
			err := s.checkLocalRequest(r)
			if tc.wantErr && err == nil {
				t.Fatalf("%s: expected rejection, got none", tc.name)
			}
			if !tc.wantErr && err != nil {
				t.Fatalf("%s: unexpected rejection: %v", tc.name, err)
			}
		})
	}
}

func TestGuardRequiresJSONBodyForBrowserRequests(t *testing.T) {
	cases := []struct {
		contentType string
		origin      string
		wantErr     bool
	}{
		{"application/json", "http://127.0.0.1:8840", false},
		{"application/json; charset=utf-8", "http://127.0.0.1:8840", false},
		{"text/plain", "http://127.0.0.1:8840", true},
		{"", "http://127.0.0.1:8840", true},
		{"", "", false}, // native client
	}
	for _, tc := range cases {
		r := httptest.NewRequest("POST", "http://127.0.0.1:8840/api/work_task_create", strings.NewReader("{}"))
		if tc.contentType != "" {
			r.Header.Set("Content-Type", tc.contentType)
		}
		if tc.origin != "" {
			r.Header.Set("Origin", tc.origin)
		}
		err := requireJSONBody(r)
		if tc.wantErr != (err != nil) {
			t.Fatalf("Content-Type %q origin %q: got err=%v", tc.contentType, tc.origin, err)
		}
	}
}
