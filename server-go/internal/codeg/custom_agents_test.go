package codeg

import (
	"archive/tar"
	"archive/zip"
	"bytes"
	"compress/gzip"
	"crypto/sha256"
	"encoding/json"
	"fmt"
	"net/http"
	"net/http/httptest"
	"os"
	"os/exec"
	"path/filepath"
	"strings"
	"testing"
	"time"

	"harnesshub/internal/acp"
	"harnesshub/internal/hub"
)

func TestCustomNPMLocalInstallAndProtocolLaunch(t *testing.T) {
	if _, _, err := npmCLI(); err != nil {
		t.Skip(err)
	}
	s := testServer(t)
	fixture, err := os.ReadFile("../../../fixtures/acp-agent.cjs")
	if err != nil {
		t.Fatal(err)
	}
	archive := filepath.Join(t.TempDir(), "fixture.tgz")
	file, err := os.Create(archive)
	if err != nil {
		t.Fatal(err)
	}
	gz := gzip.NewWriter(file)
	tw := tar.NewWriter(gz)
	for name, body := range map[string][]byte{"package/package.json": []byte(`{"name":"fixture-custom-acp","version":"1.0.0","bin":{"real-fixture-command":"bin/agent.cjs"}}`), "package/bin/agent.cjs": fixture} {
		if err = tw.WriteHeader(&tar.Header{Name: name, Size: int64(len(body)), Mode: 0755}); err != nil {
			t.Fatal(err)
		}
		if _, err = tw.Write(body); err != nil {
			t.Fatal(err)
		}
	}
	_ = tw.Close()
	_ = gz.Close()
	_ = file.Close()
	// Install a local, dependency-free npm fixture, not a remote agent. Its bin
	// differs from the package name, exercising actual package.json resolution.
	params := Row{"registryId": "local-npm-fixture", "name": "Local npm fixture", "distributionKind": "npx", "spec": Row{"npx": Row{"package": "fixture-custom-acp@file:" + filepath.ToSlash(archive)}}}
	if err = s.saveCustomAgent(params); err != nil {
		t.Fatal(err)
	}
	ag := s.pick("custom:local-npm-fixture")
	launch, ok, err := s.CustomAgentLaunch(ag)
	if err != nil || !ok {
		t.Fatal("local npm installation failed", err)
	}
	if len(launch.Args) != 1 || filepath.Base(launch.Args[0]) != "agent.cjs" {
		t.Fatal("guessed npm entry instead of resolving manifest")
	}
	s.Runtime.LaunchFn = func(a *hub.Agent, permission, cwd string) (*acp.Launch, error) {
		l, _, e := s.CustomAgentLaunch(a)
		return l, e
	}
	cid := command(t, s, "acp_connect", Row{"agentType": ag.Wire}).(string)
	if text(s.snapshot(cid)["status"]) != "connected" {
		t.Fatal("installed npm entry failed ACP initialization")
	}
}

func customFixtureCatalog(t *testing.T, s *Server) *httptest.Server {
	t.Helper()
	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		_ = json.NewEncoder(w).Encode(Row{"agents": []Row{
			{"id": "fixture-worker", "name": "Fixture worker", "description": "ACP fixture", "version": "1.2.3", "distribution": Row{"binary": Row{"unsupported-cpu": Row{"archive": "https://example.test/agent.zip", "cmd": "agent"}}, "npx": Row{"package": "fixture-agent@1.2.3", "args": []string{"--acp"}}}},
			{"id": "codex-acp", "name": "Codex", "distribution": Row{"npx": Row{"package": "@agentclientprotocol/codex-acp"}}},
			{"id": "other-platform", "name": "Other platform", "distribution": Row{"binary": Row{"unsupported-cpu": Row{"archive": "https://example.test/agent.zip", "cmd": "agent"}}}},
		}})
	}))
	s.RegistryURL = server.URL
	s.RegistryClient = server.Client()
	t.Cleanup(server.Close)
	return server
}

func TestCustomRegistryAddEditPersistAndACPStream(t *testing.T) {
	s := testServer(t)
	customFixtureCatalog(t, s)
	platform := command(t, s, "acp_current_platform", Row{}).(string)
	if platform != currentACPPlatform() {
		t.Fatal(platform)
	}
	rows := command(t, s, "acp_fetch_registry_catalog", Row{}).([]any)
	byID := map[string]Row{}
	for _, raw := range rows {
		row := raw.(map[string]any)
		byID[text(row["registryId"])] = row
	}
	if byID["codex-acp"]["builtin"] != true || byID["fixture-worker"]["supportedOnPlatform"] != true || byID["other-platform"]["supportedOnPlatform"] != false {
		t.Fatal("platform/builtin annotation incorrect")
	}
	command(t, s, "acp_add_registry_agent", Row{"registryId": "fixture-worker"})
	wire := "custom:fixture-worker"
	agent := s.pick(wire)
	if agent == nil || agent.Wire != wire {
		t.Fatal("added definition not registered")
	}
	defs := command(t, s, "acp_list_custom_agents", Row{}).([]any)
	d := defs[0].(map[string]any)
	if d["distributionKind"] != "npx" || d["source"] != "registry" || d["launchable"] != true {
		t.Fatal("unsupported binary did not fall back to npx")
	}
	d["supportsMcp"] = false
	d["name"] = "Updated worker"
	command(t, s, "acp_save_custom_agent", Row{"params": d})
	d = command(t, s, "acp_list_custom_agents", Row{}).([]any)[0].(map[string]any)
	if d["source"] != "registry" || d["supportsMcp"] != false {
		t.Fatal("metadata edit lost declarations")
	}
	for _, row := range s.agents() {
		if row["agent_type"] == wire && (row["instance"] != false || row["host_tools_supports_delegation"] != false) {
			t.Fatal("custom agent misclassified")
		}
	}
	loaded := reloadStore(t, s.store.file)
	if loaded.CustomAgents["fixture-worker"].Name != "Updated worker" {
		t.Fatal("definition not persisted")
	}
	s.Hub.DeleteAgent(agent.ID)
	s.loadCustomAgents()
	if s.pick(wire) == nil {
		t.Fatal("definition not restored")
	}
	// The shared protocol fixture exchanges actual JSON-RPC messages. No vendor
	// package installation or model inference is involved in this test.
	cid := command(t, s, "acp_connect", Row{"agentType": wire}).(string)
	command(t, s, "acp_prompt", Row{"connectionId": cid, "blocks": []Row{{"type": "text", "text": "custom ACP exchange"}}})
	deadline := time.Now().Add(5 * time.Second)
	found := false
	for time.Now().Before(deadline) {
		s.mu.Lock()
		c := s.connections[cid]
		events := clone(c.Events)
		busy := c.Busy
		s.mu.Unlock()
		for _, e := range events {
			if text(e["type"]) == "content_delta" && strings.Contains(text(e["text"]), "custom ACP exchange") {
				found = true
			}
		}
		if found && !busy {
			break
		}
		time.Sleep(20 * time.Millisecond)
	}
	if !found {
		t.Fatal("custom ACP stream did not return the actual prompt")
	}
	command(t, s, "acp_delete_custom_agent", Row{"registryId": "fixture-worker", "deleteTranscripts": false})
	if s.pick(wire) != nil {
		t.Fatal("removed definition still advertised")
	}
	if len(command(t, s, "list_all_conversations", Row{}).([]any)) == 0 {
		t.Fatal("removing definition deleted conversation history")
	}
}

func zipFixture(t *testing.T, name, body string) []byte {
	t.Helper()
	var buf bytes.Buffer
	z := zip.NewWriter(&buf)
	f, err := z.Create(name)
	if err != nil {
		t.Fatal(err)
	}
	_, _ = f.Write([]byte(body))
	if err = z.Close(); err != nil {
		t.Fatal(err)
	}
	return buf.Bytes()
}
func TestCustomBinaryInstallIntegrityPathsAndProcessEnv(t *testing.T) {
	s := testServer(t)
	blob := zipFixture(t, "bin/agent.exe", "binary fixture")
	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) { _, _ = w.Write(blob) }))
	defer server.Close()
	s.RegistryClient = server.Client()
	b := customBinarySpec{Archive: server.URL + "/agent.zip", Command: "./bin/agent.exe", SHA256: fmt.Sprintf("%x", sha256.Sum256(blob)), Args: []string{"--acp"}, Env: map[string]string{"CODEPLUSE_FIXTURE_KEY": "isolated"}}
	d := customAgentDef{RegistryID: "binary-fixture", Name: "Binary", Kind: "binary", Spec: customAgentSpec{Binary: map[string]customBinarySpec{currentACPPlatform(): b}}, Version: "2"}
	encoded, _ := json.Marshal(d)
	params := Row{}
	_ = json.Unmarshal(encoded, &params)
	if err := s.saveCustomAgent(params); err != nil {
		t.Fatal(err)
	}
	ag := s.pick("custom:binary-fixture")
	launch, ok, err := s.CustomAgentLaunch(ag)
	if err != nil || !ok {
		t.Fatal(err)
	}
	if !strings.HasPrefix(launch.Command, s.customCacheDir(d)) || len(launch.Args) != 1 || launch.Args[0] != "--acp" {
		t.Fatal("wrong binary launch")
	}
	found := false
	for _, e := range launch.Env {
		if e == "CODEPLUSE_FIXTURE_KEY=isolated" {
			found = true
		}
	}
	if !found || os.Getenv("CODEPLUSE_FIXTURE_KEY") != "" {
		t.Fatal("launch env leaked or was dropped")
	}
	if s.customInstalledVersion(d) != "2" {
		t.Fatal("completed install not detected")
	}
	server.Close()
	if _, err = s.prepareCustom(d); err != nil {
		t.Fatal("cached binary attempted another download", err)
	}
	for _, name := range []string{"../escape.exe", `..\escape.exe`, "/absolute", "C:/escape", "file:stream", ".. /escape.exe", "sub./escape.exe"} {
		if _, err = safeCustomPath(t.TempDir(), name); err == nil {
			t.Fatalf("accepted unsafe path %s", name)
		}
	}
	bad := zipFixture(t, "../escape.exe", "outside")
	badFile := filepath.Join(t.TempDir(), "bad.zip")
	_ = os.WriteFile(badFile, bad, 0600)
	if err = extractCustomArchive(t.TempDir(), badFile, "https://example.test/bad.zip"); err == nil {
		t.Fatal("zip traversal accepted")
	}
	b.SHA256 = strings.Repeat("0", 64)
	server2 := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) { _, _ = w.Write(blob) }))
	defer server2.Close()
	b.Archive = server2.URL + "/bad.zip"
	s.RegistryClient = server2.Client()
	if _, err = s.installCustomArchive(t.TempDir(), b); err == nil || !strings.Contains(err.Error(), "SHA-256") {
		t.Fatal("checksum mismatch accepted", err)
	}
}

func TestCustomPackageLaunchUsesCachedEntryAndOverlay(t *testing.T) {
	s := testServer(t)
	node, err := exec.LookPath("node")
	if err != nil {
		t.Skip("node required")
	}
	for _, kind := range []string{"npx", "uvx"} {
		t.Run(kind, func(t *testing.T) {
			p := &customPackageSpec{Package: "example-agent@1", Args: []string{"--acp"}, Env: map[string]string{"CODEPLUSE_FIXTURE_KEY": "declared"}}
			spec := customAgentSpec{}
			if kind == "npx" {
				spec.Npx = p
			} else {
				p.Package = "example-agent==1"
				spec.Uvx = p
			}
			d := customAgentDef{RegistryID: "cached-" + kind, Name: kind, Kind: kind, Spec: spec, Version: "1", SupportsMCP: true}
			data, _ := json.Marshal(d)
			params := Row{}
			_ = json.Unmarshal(data, &params)
			if err = s.saveCustomAgent(params); err != nil {
				t.Fatal(err)
			}
			dir := s.customCacheDir(d)
			_ = os.MkdirAll(dir, 0700)
			entry := filepath.Join(dir, "actual-entry.cjs")
			_ = os.WriteFile(entry, []byte("// fixture"), 0600)
			cache, _ := json.Marshal(customLaunch{Command: node, Args: []string{entry}, Version: "1.1"})
			_ = os.WriteFile(filepath.Join(dir, "launch.json"), cache, 0600)
			_, err = s.updateAgentSettings("acp_update_agent_env", Row{"agentType": "custom:" + d.RegistryID, "env": Row{"CODEPLUSE_FIXTURE_KEY": "overlay"}})
			if err != nil {
				t.Fatal(err)
			}
			launch, ok, err := s.CustomAgentLaunch(s.pick("custom:" + d.RegistryID))
			if err != nil || !ok || launch.Command != node || len(launch.Args) != 2 || launch.Args[0] != entry {
				t.Fatal("cached launch not used", err)
			}
			found := false
			for _, e := range launch.Env {
				if e == "CODEPLUSE_FIXTURE_KEY=overlay" {
					found = true
				}
			}
			if !found {
				t.Fatal("per-agent overlay not applied")
			}
		})
	}
}

func TestCustomRegistryErrorsAreRealFailures(t *testing.T) {
	s := testServer(t)
	remote := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) { http.Error(w, "unavailable", 503) }))
	defer remote.Close()
	s.RegistryURL = remote.URL
	s.RegistryClient = remote.Client()
	if _, err := s.registryCatalog(); err == nil || !strings.Contains(err.Error(), "503") {
		t.Fatal("network failure hidden", err)
	}
	if err := s.saveCustomAgent(Row{"registryId": "../escape", "distributionKind": "npx", "spec": Row{"npx": Row{"package": "example"}}}); err == nil {
		t.Fatal("invalid registry ID accepted")
	}
}
