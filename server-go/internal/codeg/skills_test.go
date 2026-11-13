package codeg

import (
	"os"
	"path/filepath"
	"strings"
	"testing"
)

// A bundled skill manifest + content under the server root, so the link logic
// can be exercised without the real codeg-assets tree.
func seedBundledSkill(t *testing.T, s *Server, group, id string) string {
	t.Helper()
	base := filepath.Join(s.Root, "codeg-assets", group)
	central := filepath.Join(base, "skills", id)
	if err := os.MkdirAll(central, 0o755); err != nil {
		t.Fatal(err)
	}
	if err := os.WriteFile(filepath.Join(central, "SKILL.md"), []byte("# "+id), 0o644); err != nil {
		t.Fatal(err)
	}
	manifest := `[{"id":"` + id + `","bundled_hash":"deadbeef"}]`
	if err := os.WriteFile(filepath.Join(base, "manifest.json"), []byte(manifest), 0o644); err != nil {
		t.Fatal(err)
	}
	return central
}

func TestSkillLinkManagesOnlyItsOwnLinksAndCopies(t *testing.T) {
	home := t.TempDir()
	t.Setenv("USERPROFILE", home)
	t.Setenv("HOME", home)
	s := testServer(t)
	seedBundledSkill(t, s, "experts", "fixture-skill")

	status, err := s.skillLink("experts", "fixture-skill", "claude_code", true)
	if err != nil {
		t.Fatal(err)
	}
	if text(status["state"]) != "linked_to_codeg" {
		t.Fatalf("enable did not take effect: %v", status)
	}
	link := filepath.Join(home, ".claude", "skills", "fixture-skill")
	body, err := os.ReadFile(filepath.Join(link, "SKILL.md"))
	if err != nil || strings.TrimSpace(string(body)) != "# fixture-skill" {
		t.Fatalf("agent skill directory has no readable content: %v %v", string(body), err)
	}

	// An instance is the same harness: its status must read the base tool's
	// directory, otherwise every profile-started conversation sees no skills.
	instance := skillStatus("fixture-skill", "inst_main_claude_abc123", text(status["expectedTargetPath"]))
	if text(instance["state"]) != "linked_to_codeg" {
		t.Fatalf("instance did not inherit the base harness link: %v", instance)
	}

	// Enabling twice is a no-op, not an error.
	if _, err = s.skillLink("experts", "fixture-skill", "claude_code", true); err != nil {
		t.Fatalf("second enable failed: %v", err)
	}

	// A real directory of the same name is never touched.
	codex := filepath.Join(home, ".codex", "skills", "fixture-skill")
	if err = os.MkdirAll(codex, 0o755); err != nil {
		t.Fatal(err)
	}
	if _, err = s.skillLink("experts", "fixture-skill", "codex", true); err == nil {
		t.Fatal("enable overwrote a real directory")
	}
	if _, err = s.skillLink("experts", "fixture-skill", "codex", false); err == nil {
		t.Fatal("disable removed a real directory")
	}
	if _, err = os.Stat(codex); err != nil {
		t.Fatal("real directory was deleted")
	}

	// Disabling removes what codeg created — link or copy — and nothing else.
	if _, err = s.skillLink("experts", "fixture-skill", "claude_code", false); err != nil {
		t.Fatal(err)
	}
	if _, err = os.Lstat(link); !os.IsNotExist(err) {
		t.Fatal("disable left the entry behind")
	}
	if _, err = os.Stat(codex); err != nil {
		t.Fatal("disable removed the unrelated real directory")
	}
}

func TestSkillApplyLinksReportsPerOpOutcomes(t *testing.T) {
	home := t.TempDir()
	t.Setenv("USERPROFILE", home)
	t.Setenv("HOME", home)
	s := testServer(t)
	seedBundledSkill(t, s, "experts", "fixture-skill")

	out, err := s.skills("experts_apply_links", Row{"ops": []any{
		map[string]any{"expertId": "fixture-skill", "agentType": "codex", "enable": true},
		map[string]any{"expertId": "no-such-skill", "agentType": "codex", "enable": true},
	}})
	if err != nil {
		t.Fatal(err)
	}
	rows := out.([]Row)
	if len(rows) != 2 || rows[0]["ok"] != true || rows[1]["ok"] != false || rows[1]["error"] == nil {
		t.Fatalf("per-op outcomes wrong: %v", rows)
	}
	if status, _ := rows[0]["status"].(map[string]any); text(status["state"]) != "linked_to_codeg" {
		t.Fatalf("successful op did not return the enabled status: %v", rows[0]["status"])
	}
}
