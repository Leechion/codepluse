package codeg

import (
	"crypto/sha256"
	"encoding/hex"
	"encoding/json"
	"fmt"
	"os"
	"path/filepath"
	"strings"
)

func (s *Server) bundledSkills(group string) ([]Row, error) {
	base := filepath.Join(s.Root, "codeg-assets", group)
	data, err := os.ReadFile(filepath.Join(base, "manifest.json"))
	if err != nil {
		return nil, err
	}
	var metadata []Row
	if err = json.Unmarshal(data, &metadata); err != nil {
		return nil, err
	}
	out := []Row{}
	for _, meta := range metadata {
		file := filepath.Join(base, "skills", text(meta["id"]), "SKILL.md")
		b, err := os.ReadFile(file)
		h := sha256.Sum256(b)
		out = append(out, Row{"metadata": meta, "installed_centrally": err == nil, "user_modified": hex.EncodeToString(h[:]) != text(meta["bundled_hash"]), "central_path": filepath.Dir(file)})
	}
	return out, nil
}
// skillTool resolves a wire to the tool whose skill directory serves it.
// A named instance (inst_claude2, inst_main_claude_<digest>) is the same
// harness with a pinned provider/model, so it reads and links into the BASE
// tool's directory: giving each profile a skill directory of its own would
// leave every instance-started conversation with an empty skill set.
func skillTool(agentType string) string {
	switch agentType {
	case "claude_code", "claude":
		return "claude"
	case "codex":
		return "codex"
	case "deepseek", "dsh":
		return "dsh"
	}
	if strings.HasPrefix(agentType, "inst_") {
		rest := strings.TrimPrefix(agentType, "inst_")
		rest = strings.TrimPrefix(rest, "main_")
		for _, tool := range []string{"claude", "codex", "dsh", "deepseek"} {
			if strings.HasPrefix(rest, tool) {
				return skillTool(tool)
			}
		}
	}
	return ""
}

func nativeSkillRoot(agentType string) string {
	home, _ := os.UserHomeDir()
	dir := ".agents"
	switch skillTool(agentType) {
	case "claude":
		dir = ".claude"
	case "codex":
		dir = ".codex"
	}
	return filepath.Join(home, dir, "skills")
}

// skillCopyMarker marks a skill directory codepluse copied (rather than
// linked) into an agent's skill directory. Copying is the fallback for Windows
// without Developer Mode, where creating a symlink needs a privilege this
// process does not hold. The marker carries the central path it came from, so
// disable only ever removes codepluse's own copy, never a user directory.
const skillCopyMarker = ".codeg-copy"

// skillStatus is the per-(skill, agent) row the settings matrix and the
// welcome-screen cards read: enabled only when the link at the agent's own
// skill directory points at codeg's central copy of the skill.
func skillStatus(name, agentType, central string) Row {
	link := filepath.Join(nativeSkillRoot(agentType), name)
	state := "not_linked"
	copyMode := false
	var target any
	if info, err := os.Lstat(link); err == nil {
		if info.Mode()&os.ModeSymlink != 0 {
			if actual, err := filepath.EvalSymlinks(link); err == nil {
				target = actual
				state = "linked_elsewhere"
				if strings.EqualFold(actual, central) {
					state = "linked_to_codeg"
				}
			} else {
				state = "broken"
			}
		} else if marker, err := os.ReadFile(filepath.Join(link, skillCopyMarker)); err == nil &&
			strings.EqualFold(strings.TrimSpace(string(marker)), central) {
			target = strings.TrimSpace(string(marker))
			state = "linked_to_codeg"
			copyMode = true
		} else {
			state = "blocked_by_real_directory"
		}
	}
	return Row{"expertId": name, "agentType": agentType, "state": state, "linkPath": link, "targetPath": target, "expectedTargetPath": central, "copyMode": copyMode}
}

// copySkillDir copies a skill directory tree and stamps it with the marker.
func copySkillDir(src, dst string) error {
	if err := os.MkdirAll(dst, 0o755); err != nil {
		return err
	}
	return filepath.WalkDir(src, func(path string, entry os.DirEntry, err error) error {
		if err != nil {
			return err
		}
		rel, relErr := filepath.Rel(src, path)
		if relErr != nil {
			return relErr
		}
		if rel == "." {
			return nil
		}
		out := filepath.Join(dst, rel)
		if entry.IsDir() {
			return os.MkdirAll(out, 0o755)
		}
		data, readErr := os.ReadFile(path)
		if readErr != nil {
			return readErr
		}
		return os.WriteFile(out, data, 0o644)
	})
}

// skillLink enables or disables one bundled skill for one agent by managing the
// link in that agent's skill directory. Only links/copies codeg itself put there
// are removed, and a same-named real directory is never touched.
func (s *Server) skillLink(group, skillID, agentType string, enable bool) (Row, error) {
	list, err := s.bundledSkills(group)
	if err != nil {
		return nil, err
	}
	central := ""
	for _, item := range list {
		if meta, ok := item["metadata"].(map[string]any); ok && text(meta["id"]) == skillID {
			central = text(item["central_path"])
			break
		}
	}
	if central == "" {
		return nil, fmt.Errorf("技能不存在：%s", skillID)
	}
	root := nativeSkillRoot(agentType)
	link := filepath.Join(root, skillID)
	if enable {
		if err = os.MkdirAll(root, 0o755); err != nil {
			return nil, fmt.Errorf("无法创建技能目录 %s：%w", root, err)
		}
		if info, statErr := os.Lstat(link); statErr == nil {
			if info.Mode()&os.ModeSymlink != 0 {
				actual, _ := filepath.EvalSymlinks(link)
				if !strings.EqualFold(actual, central) {
					return nil, fmt.Errorf("已存在指向 %s 的链接，未改动", actual)
				}
				return skillStatus(skillID, agentType, central), nil
			}
			if marker, readErr := os.ReadFile(filepath.Join(link, skillCopyMarker)); readErr == nil &&
				strings.EqualFold(strings.TrimSpace(string(marker)), central) {
				return skillStatus(skillID, agentType, central), nil
			}
			return nil, fmt.Errorf("目标位置已有同名目录，未改动：%s", link)
		}
		if err = os.Symlink(central, link); err != nil {
			// Windows without Developer Mode: copy the skill instead, which the
			// harness reads exactly like a linked directory.
			if copyErr := copySkillDir(central, link); copyErr != nil {
				return nil, fmt.Errorf("创建技能链接失败（可开启 Windows 开发者模式）：%w", err)
			}
			if markErr := os.WriteFile(filepath.Join(link, skillCopyMarker), []byte(central), 0o644); markErr != nil {
				_ = os.RemoveAll(link)
				return nil, markErr
			}
		}
		return skillStatus(skillID, agentType, central), nil
	}
	info, statErr := os.Lstat(link)
	if statErr != nil {
		return skillStatus(skillID, agentType, central), nil
	}
	if info.Mode()&os.ModeSymlink == 0 {
		marker, readErr := os.ReadFile(filepath.Join(link, skillCopyMarker))
		if readErr != nil || !strings.EqualFold(strings.TrimSpace(string(marker)), central) {
			return nil, fmt.Errorf("目标不是 codepluse 创建的链接或副本，未删除：%s", link)
		}
		if err = os.RemoveAll(link); err != nil {
			return nil, err
		}
		return skillStatus(skillID, agentType, central), nil
	}
	actual, _ := filepath.EvalSymlinks(link)
	if !strings.EqualFold(actual, central) {
		return nil, fmt.Errorf("链接指向别处，未删除：%s", actual)
	}
	if err = os.Remove(link); err != nil {
		return nil, err
	}
	return skillStatus(skillID, agentType, central), nil
}

func (s *Server) skills(command string, a Row) (any, error) {
	group := "experts"
	if strings.HasPrefix(command, "science_") {
		group = "science"
	}
	if command == "experts_list" || command == "science_list" {
		return s.bundledSkills(group)
	}
	if command == "experts_read_content" || command == "science_read_content" {
		name := text(a["expertId"])
		if name == "" {
			name = text(a["skillId"])
		}
		list, err := s.bundledSkills(group)
		if err != nil {
			return nil, err
		}
		for _, item := range list {
			if meta, ok := item["metadata"].(map[string]any); ok && meta["id"] == name {
				b, err := os.ReadFile(filepath.Join(text(item["central_path"]), "SKILL.md"))
				return string(b), err
			}
		}
		return nil, fmt.Errorf("技能不存在")
	}
	if strings.HasSuffix(command, "_list_all_install_statuses") {
		// NB: officecli_skill_list_all_install_statuses never reaches here —
		// server.go routes the officecli_* prefix to officeCommand first, which
		// returns an empty array. This branch handles experts/science only.
		list, err := s.bundledSkills(group)
		if err != nil {
			return nil, err
		}
		out := []Row{}
		for _, item := range list {
			meta := item["metadata"].(map[string]any)
			name := text(meta["id"])
			for _, agent := range s.agents() {
				out = append(out, skillStatus(name, text(agent["agent_type"]), text(item["central_path"])))
			}
		}
		return out, nil
	}
	if strings.HasSuffix(command, "_get_install_status") {
		name := text(a["expertId"])
		if name == "" {
			name = text(a["skillId"])
		}
		agentType := text(a["agentType"])
		list, err := s.bundledSkills(group)
		if err != nil {
			return nil, err
		}
		for _, item := range list {
			if meta, ok := item["metadata"].(map[string]any); ok && text(meta["id"]) == name {
				return skillStatus(name, agentType, text(item["central_path"])), nil
			}
		}
		return nil, fmt.Errorf("技能不存在：%s", name)
	}
	if command == "experts_link_to_agent" || command == "science_link_to_agent" {
		return s.skillLink(group, text(a["expertId"]), text(a["agentType"]), true)
	}
	if command == "experts_unlink_from_agent" || command == "science_unlink_from_agent" {
		return s.skillLink(group, text(a["expertId"]), text(a["agentType"]), false)
	}
	if command == "experts_apply_links" || command == "science_apply_links" {
		ops, _ := a["ops"].([]any)
		out := []Row{}
		for _, raw := range ops {
			op, _ := raw.(map[string]any)
			enable, _ := op["enable"].(bool)
			status, err := s.skillLink(group, text(op["expertId"]), text(op["agentType"]), enable)
			row := Row{"expertId": text(op["expertId"]), "agentType": text(op["agentType"]), "ok": err == nil, "status": nil, "error": nil}
			if err != nil {
				row["error"] = err.Error()
			} else if enable {
				row["status"] = status
			}
			out = append(out, row)
		}
		return out, nil
	}
	if command == "acp_list_agent_runtime_skills" || command == "acp_list_agent_skills" {
		agentType := text(a["agentType"])
		if s.pick(agentType) == nil {
			return nil, fmt.Errorf("执行端不存在")
		}
		locations := []Row{}
		items := []Row{}
		candidates := []Row{{"scope": "user", "path": nativeSkillRoot(agentType)}}
		if cwd := text(a["workspacePath"]); cwd != "" {
			if resolved, err := s.Resolve(cwd); err == nil {
				candidates = append(candidates, Row{"scope": "project", "path": filepath.Join(resolved, ".agents", "skills")})
			}
		}
		for _, location := range candidates {
			root := text(location["path"])
			entries, err := os.ReadDir(root)
			locations = append(locations, Row{"scope": location["scope"], "path": root, "exists": err == nil})
			for _, entry := range entries {
				file := filepath.Join(root, entry.Name(), "SKILL.md")
				if _, err := os.Stat(file); err != nil {
					continue
				}
				items = append(items, Row{"id": entry.Name(), "name": entry.Name(), "scope": location["scope"], "layout": "directory", "path": file, "description": nil, "read_only": true})
			}
		}
		return Row{"supported": true, "message": nil, "locations": locations, "skills": items}, nil
	}
	return nil, fmt.Errorf("技能操作尚未接通：%s", command)
}
