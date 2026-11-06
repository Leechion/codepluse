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
func nativeSkillRoot(agentType string) string {
	home, _ := os.UserHomeDir()
	dir := ".agents"
	if agentType == "claude_code" {
		dir = ".claude"
	}
	if agentType == "codex" {
		dir = ".codex"
	}
	return filepath.Join(home, dir, "skills")
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
		if command == "officecli_skill_list_all_install_statuses" {
			return nil, fmt.Errorf("Go 版尚未接入 Office CLI，未报告虚假安装状态")
		}
		list, err := s.bundledSkills(group)
		if err != nil {
			return nil, err
		}
		out := []Row{}
		for _, item := range list {
			meta := item["metadata"].(map[string]any)
			name := text(meta["id"])
			for _, agent := range s.agents() {
				agentType := text(agent["agent_type"])
				link := filepath.Join(nativeSkillRoot(agentType), name)
				expected := text(item["central_path"])
				state := "not_linked"
				var target any
				if info, err := os.Lstat(link); err == nil {
					if info.Mode()&os.ModeSymlink != 0 {
						if actual, err := filepath.EvalSymlinks(link); err == nil {
							target = actual
							state = "linked_elsewhere"
							if strings.EqualFold(actual, expected) {
								state = "linked_to_codeg"
							}
						} else {
							state = "broken"
						}
					} else {
						state = "blocked_by_real_directory"
					}
				}
				out = append(out, Row{"expertId": name, "agentType": agentType, "state": state, "linkPath": link, "targetPath": target, "expectedTargetPath": expected, "copyMode": false})
			}
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
