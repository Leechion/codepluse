package main

import (
	"net/http"
	"net/http/httptest"
	"testing"
)

func TestLocalGuardRejectsCrossSiteAndNonJSON(t *testing.T) {
	const port = 8840
	next := http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		w.WriteHeader(http.StatusOK)
	})
	guard := newLocalGuard(port).wrap(next)

	cases := []struct {
		name      string
		method    string
		path      string
		host      string
		origin    string
		content   string
		fetchSite string
		want      int
	}{
		{name: "same origin json", method: "POST", path: "/api/acp_prompt",
			host: "127.0.0.1:8840", origin: "http://127.0.0.1:8840", content: "application/json", want: 200},
		{name: "localhost origin, charset", method: "POST", path: "/api/acp_prompt",
			host: "127.0.0.1:8840", origin: "http://localhost:8840", content: "application/json; charset=utf-8", want: 200},
		{name: "native caller sends no origin", method: "POST", path: "/api/codeg_mcp",
			host: "127.0.0.1:8840", content: "application/json", want: 200},
		{name: "multipart upload", method: "POST", path: "/api/upload_attachment",
			host: "127.0.0.1:8840", origin: "http://127.0.0.1:8840",
			content: "multipart/form-data; boundary=x", want: 200},
		{name: "get health", method: "GET", path: "/api/health",
			host: "127.0.0.1:8840", want: 200},
		{name: "static asset", method: "GET", path: "/workspace",
			host: "localhost:8840", want: 200},

		{name: "evil origin", method: "POST", path: "/api/terminal_spawn",
			host: "127.0.0.1:8840", origin: "http://evil.example", content: "application/json", want: 403},
		{name: "null origin", method: "POST", path: "/api/terminal_spawn",
			host: "127.0.0.1:8840", origin: "null", content: "application/json", want: 403},
		{name: "dns rebinding host", method: "POST", path: "/api/terminal_spawn",
			host: "evil.example:8840", origin: "http://evil.example:8840", content: "application/json", want: 403},
		{name: "cross-site fetch metadata", method: "POST", path: "/api/terminal_spawn",
			host: "127.0.0.1:8840", content: "application/json", fetchSite: "cross-site", want: 403},
		{name: "text/plain csrf", method: "POST", path: "/api/terminal_spawn",
			host: "127.0.0.1:8840", origin: "http://127.0.0.1:8840", content: "text/plain", want: 415},
		{name: "missing content type", method: "POST", path: "/api/terminal_spawn",
			host: "127.0.0.1:8840", origin: "http://127.0.0.1:8840", want: 415},
		{name: "json upload rejected", method: "POST", path: "/api/upload_attachment",
			host: "127.0.0.1:8840", origin: "http://127.0.0.1:8840", content: "application/json", want: 415},
	}

	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			req := httptest.NewRequest(tc.method, tc.path, nil)
			req.Host = tc.host
			if tc.origin != "" {
				req.Header.Set("Origin", tc.origin)
			}
			if tc.content != "" {
				req.Header.Set("Content-Type", tc.content)
			}
			if tc.fetchSite != "" {
				req.Header.Set("Sec-Fetch-Site", tc.fetchSite)
			}
			rec := httptest.NewRecorder()
			guard.ServeHTTP(rec, req)
			if rec.Code != tc.want {
				t.Fatalf("status = %d, want %d (body %s)", rec.Code, tc.want, rec.Body.String())
			}
		})
	}
}

func TestLocalGuardIgnoresNonLoopbackHost(t *testing.T) {
	// A non-loopback HOST is not a supported deployment; it must not silently
	// widen the host allowlist.
	guard := newLocalGuard(8840)
	for _, host := range []string{"0.0.0.0:8840", "192.168.1.10:8840", "codeg.local:8840"} {
		req := httptest.NewRequest("GET", "/api/health", nil)
		req.Host = host
		rec := httptest.NewRecorder()
		guard.wrap(http.HandlerFunc(func(http.ResponseWriter, *http.Request) {})).
			ServeHTTP(rec, req)
		if rec.Code != http.StatusForbidden {
			t.Fatalf("host %s: status = %d, want 403", host, rec.Code)
		}
	}
}
