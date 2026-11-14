package codeg

import (
	"net"
	"net/http"
	"net/http/httptest"
	"strings"
	"testing"
)

func TestBlockedFetchIP(t *testing.T) {
	blocked := []string{
		"127.0.0.1", "10.1.2.3", "172.16.0.1", "192.168.1.1",
		"169.254.169.254", "100.64.0.1", "192.0.0.10", "198.18.0.1",
		"240.0.0.1", "0.0.0.0", "::1", "fe80::1", "fc00::1",
		"::ffff:127.0.0.1", "2001:db8::1",
	}
	for _, raw := range blocked {
		if !blockedFetchIP(net.ParseIP(raw)) {
			t.Fatalf("expected %s to be blocked", raw)
		}
	}
	allowed := []string{"8.8.8.8", "1.1.1.1", "93.184.216.34", "2606:4700:4700::1111"}
	for _, raw := range allowed {
		if blockedFetchIP(net.ParseIP(raw)) {
			t.Fatalf("expected %s to be allowed", raw)
		}
	}
	if !blockedFetchIP(nil) {
		t.Fatal("nil IP must be blocked")
	}
}

func TestSafeFetchClientRefusesLoopback(t *testing.T) {
	reached := false
	srv := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		reached = true
		_, _ = w.Write([]byte("should not be reached"))
	}))
	defer srv.Close()

	res, err := safeFetchClient().Get(srv.URL)
	if err == nil {
		res.Body.Close()
		t.Fatal("safeFetchClient reached a loopback server")
	}
	if reached {
		t.Fatal("handler ran despite the dial guard")
	}
	if !strings.Contains(err.Error(), "不允许") {
		t.Fatalf("unexpected error: %v", err)
	}
}
