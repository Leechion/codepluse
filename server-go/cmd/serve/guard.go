package main

// localGuard is the trust boundary for the local HTTP API.
//
// codepluse binds 127.0.0.1 and serves a single-user desktop window, but the
// API itself grants process-level power (terminal_spawn, acp_prompt, file
// writes). Three cheap checks keep a web page from driving it:
//
//  1. Host allowlist. Cross-site requests are already blocked by Origin, but a
//     DNS-rebinding page has Origin == Host, so only a host whitelist stops it.
//  2. Origin allowlist (plus Fetch Metadata for requests that omit Origin).
//     Browsers always send Origin on POST; native callers (the desktop probe,
//     the codeg-mcp companion) send none and are still Host-checked.
//  3. application/json Content-Type on POST. A cross-origin fetch with
//     text/plain is a "simple request" and skips CORS preflight, so the request
//     would otherwise reach the handler even though the response is opaque.
//     The handler decoded the body regardless, which made the CSRF complete.
//
// Uploads are multipart and are allowed that media type explicitly. Non-API
// paths (static export, dev proxy) are only subject to the host check.

import (
	"net"
	"net/http"
	"strconv"
	"strings"
)

type localGuard struct {
	hosts   map[string]bool
	origins map[string]bool
}

// newLocalGuard builds the allowlists for a loopback-only deployment. Only
// loopback aliases are accepted: the desktop shell loads 127.0.0.1 and the
// WebView may use localhost, but a non-loopback HOST is not a supported
// deployment and gets no entry.
func newLocalGuard(port int) *localGuard {
	g := &localGuard{hosts: map[string]bool{}, origins: map[string]bool{}}
	for _, host := range []string{"127.0.0.1", "localhost", "::1"} {
		addr := net.JoinHostPort(host, strconv.Itoa(port))
		g.hosts[addr] = true
		g.origins["http://"+addr] = true
	}
	return g
}

func (g *localGuard) wrap(next http.Handler) http.Handler {
	return http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		if !g.hosts[strings.ToLower(r.Host)] {
			denyLocal(w, http.StatusForbidden, "forbidden_host")
			return
		}
		if origin := strings.ToLower(strings.TrimSuffix(r.Header.Get("Origin"), "/")); origin != "" {
			if !g.origins[origin] {
				denyLocal(w, http.StatusForbidden, "forbidden_origin")
				return
			}
		} else if strings.EqualFold(r.Header.Get("Sec-Fetch-Site"), "cross-site") {
			denyLocal(w, http.StatusForbidden, "cross_site_blocked")
			return
		}
		if r.Method == http.MethodPost && strings.HasPrefix(r.URL.Path, "/api/") {
			want := "application/json"
			if r.URL.Path == "/api/upload_attachment" {
				want = "multipart/form-data"
			}
			if mediaTypeOf(r.Header.Get("Content-Type")) != want {
				denyLocal(w, http.StatusUnsupportedMediaType, "unsupported_content_type")
				return
			}
		}
		next.ServeHTTP(w, r)
	})
}

// mediaTypeOf strips Content-Type parameters (charset, multipart boundary).
// The package already has a top-level `mime` map in state.go, so importing
// mime.ParseMediaType here would collide.
func mediaTypeOf(value string) string {
	if i := strings.IndexByte(value, ';'); i >= 0 {
		value = value[:i]
	}
	return strings.ToLower(strings.TrimSpace(value))
}

func denyLocal(w http.ResponseWriter, status int, code string) {
	w.Header().Set("Content-Type", "application/json; charset=utf-8")
	w.Header().Set("Cache-Control", "no-store")
	w.WriteHeader(status)
	_, _ = w.Write([]byte(`{"code":"` + code + `","message":"请求被本地安全策略拒绝"}`))
}
