package accountchannels

import (
	"context"
	"encoding/json"
	"net/http"
	"net/http/httptest"
	"os"
	"path/filepath"
	"strings"
	"testing"
)

func TestExistingBridgeIsAuthenticatedAndOnlyLoopbackRuntimeCanBeReused(t *testing.T) {
	key := strings.Repeat("fixture-key", 4)
	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		if r.Header.Get("Authorization") != "Bearer "+key {
			t.Error("missing local bridge key")
			w.WriteHeader(401)
			return
		}
		if r.URL.Path == "/health" {
			w.Write([]byte(`{"ok":true}`))
			return
		}
		w.Write([]byte(`[{"id":"qodercn","configured":false,"models":[],"accounts":[]}]`))
	}))
	defer server.Close()
	root := t.TempDir()
	dir := filepath.Join(root, ".data", "account-channels")
	os.MkdirAll(dir, 0700)
	write := func(url string) {
		b, _ := json.Marshal(map[string]string{"url": url, "key": key})
		os.WriteFile(filepath.Join(dir, "runtime.json"), b, 0600)
	}
	write(server.URL)
	client := running(root)
	if client == nil {
		t.Fatal("could not reuse local bridge")
	}
	body, err := client.Call(context.Background(), "GET", "/status", nil)
	if err != nil || strings.Contains(string(body), key) {
		t.Fatalf("invalid public status: %s %v", body, err)
	}
	write("https://remote.example")
	if running(root) != nil {
		t.Fatal("remote bridge runtime accepted")
	}
}
