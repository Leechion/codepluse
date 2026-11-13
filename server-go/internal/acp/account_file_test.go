package acp

import (
	"os"
	"path/filepath"
	"testing"
)

func TestAccountAgentAbsoluteFileRequestsRespectWorkspaceAndPermission(t *testing.T) {
	dir := t.TempDir()
	file := filepath.Join(dir, "example.txt")
	rec := &SessionRecord{NativeSessionID: "account-session", Permission: "workspace"}
	driver := &Driver{cwd: dir, sessions: map[*SessionRecord]bool{rec: true}}
	rt := &Runtime{}
	if _, err := rt.fsWrite(driver, map[string]any{"sessionId": "account-session", "path": file, "content": "hello"}); err != nil {
		t.Fatal(err)
	}
	result, err := rt.fsRead(driver, map[string]any{"path": file})
	if err != nil || result["content"] != "hello" {
		t.Fatalf("absolute read: %v %v", result, err)
	}
	outside := filepath.Join(t.TempDir(), "outside.txt")
	_ = os.WriteFile(outside, []byte("secret"), 0600)
	if _, err = rt.fsRead(driver, map[string]any{"path": outside}); err == nil {
		t.Fatal("outside read allowed")
	}
	if _, err = rt.fsWrite(driver, map[string]any{"sessionId": "account-session", "path": outside, "content": "bad"}); err == nil {
		t.Fatal("outside write allowed")
	}
	rec.Permission = "readonly"
	if _, err = rt.fsWrite(driver, map[string]any{"sessionId": "account-session", "path": file, "content": "bad"}); err == nil {
		t.Fatal("readonly write allowed")
	}
}
