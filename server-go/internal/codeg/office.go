// Office tooling layer: manages the external OfficeCLI binary the agents use
// for docx/xlsx/pptx work, plus its skill packs (codeg-ui office-tools
// settings + officecli_* commands). The binary itself ships upstream, not in
// this repository — detection and the skill layer are real; install reports
// clearly that no distribution source is configured.
package codeg

import (
	"fmt"
	"os"
	"os/exec"
	"path/filepath"
	"strings"
)

// officecliDetect looks for the OfficeCLI binary on PATH and the configured
// locations, returning where it was found.
func officecliDetect() Row {
	name := "officecli"
	if _, err := exec.LookPath(name); err == nil {
		return Row{"installed": true, "path": name, "source": "PATH"}
	}
	home, _ := os.UserHomeDir()
	for _, candidate := range []string{
		filepath.Join(home, ".officecli", "officecli.exe"),
		filepath.Join(home, ".local", "bin", "officecli.exe"),
	} {
		if _, err := os.Stat(candidate); err == nil {
			return Row{"installed": true, "path": candidate, "source": "local"}
		}
	}
	return Row{"installed": false, "path": "", "source": ""}
}

// officeSkillDir lists the skill packs that ship with the repository
// (codeg-assets) plus any user-installed ones under .data/office-skills.
func (s *Server) officeSkillDirs() []string {
	return []string{filepath.Join(s.Root, "codeg-assets"), filepath.Join(s.Root, ".data", "office-skills")}
}

func (s *Server) officeCommand(command string, a Row) (any, error) {
	switch command {
	case "officecli_detect":
		return officecliDetect(), nil
	case "officecli_install":
		if officecliDetect()["installed"] == true {
			return Row{"installed": true, "message": "OfficeCLI 已安装"}, nil
		}
		return nil, fmt.Errorf("本仓库不分发 OfficeCLI 二进制：请从上游获取后放入 PATH 或 ~/.officecli/")
	case "officecli_uninstall":
		return Row{"uninstalled": true, "message": "未托管安装，无需卸载"}, nil
	case "officecli_list_skills", "officecli_sync_skills":
		return s.officeSkills()
	case "officecli_skill_read_content":
		return s.officeSkillContent(text(a["skillId"]))
	case "officecli_skill_list_all_install_statuses", "officecli_skill_get_install_status":
		// Skill installation status follows skill sync; without the binary the
		// skills exist as content only.
		return Row{}, nil
	case "officecli_skill_link_to_agent", "officecli_skill_unlink_from_agent", "officecli_skill_apply_links":
		return nil, fmt.Errorf("OfficeCLI 未安装，技能无法关联到执行端")
	case "officecli_render_html", "start_office_watch", "stop_office_watch":
		return nil, fmt.Errorf("Office 渲染需要 OfficeCLI 二进制，当前未安装")
	}
	return nil, fmt.Errorf("未知的 Office 命令：%s", command)
}

// officeSkills scans the skill directories; each markdown file is a skill.
func (s *Server) officeSkills() (any, error) {
	skills := []Row{}
	seen := map[string]bool{}
	for _, dir := range s.officeSkillDirs() {
		entries, err := os.ReadDir(dir)
		if err != nil {
			continue
		}
		for _, entry := range entries {
			if entry.IsDir() || !strings.HasSuffix(entry.Name(), ".md") {
				continue
			}
			name := strings.TrimSuffix(entry.Name(), ".md")
			if seen[name] {
				continue
			}
			seen[name] = true
			skills = append(skills, Row{"id": "office-skill-" + name, "name": name,
				"source": filepath.Base(dir), "path": filepath.Join(dir, entry.Name())})
		}
	}
	return Row{"skills": skills}, nil
}

func (s *Server) officeSkillContent(skillID string) (any, error) {
	if !strings.HasPrefix(skillID, "office-skill-") {
		return nil, fmt.Errorf("未知的技能：%s", skillID)
	}
	name := strings.TrimPrefix(skillID, "office-skill-")
	for _, dir := range s.officeSkillDirs() {
		data, err := os.ReadFile(filepath.Join(dir, name+".md"))
		if err == nil {
			return Row{"id": skillID, "name": name, "content": string(data)}, nil
		}
	}
	return nil, fmt.Errorf("技能文件不存在：%s", name)
}
