package codeg

// Local request guard: the HTTP API can reach terminals, git and the file
// system, so a browser page on any origin must never be able to drive it.
// Binding to 127.0.0.1 does not help: a malicious page can still POST to a
// loopback port (CSRF with a "simple" text/plain body needs no preflight),
// and DNS rebinding makes the loopback service same-origin with an attacker
// domain.
//
// The checks are applied only when the request carries browser signals
// (Origin / Sec-Fetch-Site). Native clients — curl, the MCP companion, the
// desktop shell, the Go tests — send neither and keep working unchanged.
//
//   - Sec-Fetch-Site must be same-origin or none.
//   - Origin, when present, must be a loopback origin on this exact port and
//     must agree with the Host header, so evil.com:8840 (rebound to 127.0.0.1)
//     is rejected on both counts.
//   - Command bodies must be application/json, which forces a CORS preflight
//     that this server never approves, so a cross-site fetch cannot deliver a
//     body at all.
//
// A local process can still call the API (it could also just run the command
// itself), so this is a browser boundary, not a multi-user authentication
// layer.

import (
	"fmt"
	"mime"
	"net/http"
	"net/url"
	"os"
	"strings"
)

func isLoopbackName(host string) bool {
	switch strings.ToLower(host) {
	case "127.0.0.1", "localhost", "::1":
		return true
	}
	return false
}

// listenPort is the port from Server.Base ("http://127.0.0.1:8840").
func (s *Server) listenPort() string {
	u, err := url.Parse(s.Base)
	if err != nil {
		return ""
	}
	return u.Port()
}

// originAllowed reports whether an Origin authority belongs to this server.
// When the operator deliberately binds a non-loopback interface (HOST=0.0.0.0
// or a LAN address) that name is accepted too, because the deployment is then
// explicitly opting out of the local-only guarantee.
func (s *Server) originAllowed(originHost string, requestHost string) bool {
	host, port, err := splitAuthority(originHost)
	if err != nil {
		return false
	}
	if want := s.listenPort(); want != "" && port != want {
		return false
	}
	if !isLoopbackName(host) {
		configured := strings.ToLower(strings.TrimSpace(os.Getenv("HOST")))
		if configured == "" || configured != strings.ToLower(host) {
			return false
		}
	}
	// A rebinding domain reaches the loopback socket with its own Host header;
	// requiring Host == Origin rejects it even though the origin itself is a
	// loopback name.
	return strings.EqualFold(originHost, requestHost)
}

func splitAuthority(authority string) (host, port string, err error) {
	u, err := url.Parse("//" + authority)
	if err != nil {
		return "", "", err
	}
	if u.Hostname() == "" || u.Port() == "" {
		return "", "", fmt.Errorf("authority %q has no host or port", authority)
	}
	return u.Hostname(), u.Port(), nil
}

func (s *Server) checkLocalRequest(r *http.Request) error {
	if site := r.Header.Get("Sec-Fetch-Site"); site != "" &&
		!strings.EqualFold(site, "same-origin") && !strings.EqualFold(site, "none") {
		return fmt.Errorf("cross-site request rejected")
	}
	if origin := r.Header.Get("Origin"); origin != "" {
		u, err := url.Parse(origin)
		if err != nil || u.Host == "" {
			return fmt.Errorf("invalid origin %q", origin)
		}
		if !s.originAllowed(u.Host, r.Host) {
			return fmt.Errorf("origin %q is not allowed", origin)
		}
	}
	return nil
}

// requireJSONBody rejects bodies a malicious page can send without a CORS
// preflight. Requests with no Origin are native clients and may omit
// Content-Type, matching the previous behaviour.
func requireJSONBody(r *http.Request) error {
	ct := r.Header.Get("Content-Type")
	if ct == "" {
		if r.Header.Get("Origin") != "" {
			return fmt.Errorf("Content-Type application/json is required")
		}
		return nil
	}
	mediaType, _, err := mime.ParseMediaType(ct)
	if err != nil || !strings.EqualFold(mediaType, "application/json") {
		return fmt.Errorf("unsupported Content-Type %q", ct)
	}
	return nil
}
