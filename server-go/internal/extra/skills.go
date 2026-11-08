package extra

import (
	"fmt"
	"os"
	"path/filepath"
	"regexp"
	"sort"
	"strings"
	"sync"

	"harnesshub/internal/hub"
)

// Skills are codeg-style skill packs: reusable instruction snippets assigned
// to agents as a matrix (agent -> [skill names]). Bodies live in
// skills/<name>.md and are appended verbatim to the agent's prompts.
var (
	skillNameRe = regexp.MustCompile(`^[a-zA-Z0-9_-]{1,64}$`)
	MAX_BODY    = 4000
	MAX_SKILLS  = 3
)

type Skills struct {
	Dir   string
	mu    sync.Mutex
	cache map[string]*string
}

func NewSkills(dir string) *Skills {
	return &Skills{Dir: dir, cache: map[string]*string{}}
}

// SkillInfo is one list() row.
type SkillInfo struct {
	Name    string `json:"name"`
	Summary string `json:"summary"`
	Length  int    `json:"length"`
}

// List scans the skills directory.
func (s *Skills) List() []SkillInfo {
	entries, err := os.ReadDir(s.Dir)
	if err != nil {
		return []SkillInfo{}
	}
	var out []SkillInfo
	for _, e := range entries {
		if e.IsDir() || !strings.HasSuffix(e.Name(), ".md") {
			continue
		}
		name := strings.TrimSuffix(e.Name(), ".md")
		content := s.Load(name)
		summary := ""
		for _, line := range strings.Split(content, "\n") {
			if strings.TrimSpace(line) != "" {
				summary = line
				break
			}
		}
		summary = regexp.MustCompile(`^#+\s*`).ReplaceAllString(summary, "")
		out = append(out, SkillInfo{Name: name, Summary: short(summary, 120), Length: len(content)})
	}
	sort.Slice(out, func(i, j int) bool { return out[i].Name < out[j].Name })
	return out
}

// Load reads one skill body (cached).
func (s *Skills) Load(name string) string {
	if !skillNameRe.MatchString(name) {
		return ""
	}
	s.mu.Lock()
	defer s.mu.Unlock()
	if body, ok := s.cache[name]; ok {
		if body == nil {
			return ""
		}
		return *body
	}
	var body *string
	data, err := os.ReadFile(filepath.Join(s.Dir, name+".md"))
	if err == nil {
		trimmed := strings.TrimSpace(string(data))
		body = &trimmed
	}
	s.cache[name] = body
	if body == nil {
		return ""
	}
	return *body
}

// Assign validates an assignment list and returns sanitized names.
func (s *Skills) Assign(names []string) ([]string, error) {
	if len(names) > 8 {
		return nil, fmt.Errorf("技能列表无效")
	}
	seen := map[string]bool{}
	var out []string
	for _, n := range names {
		if !skillNameRe.MatchString(n) {
			return nil, fmt.Errorf("技能名称无效：%s", n)
		}
		if !seen[n] {
			seen[n] = true
			out = append(out, n)
		}
	}
	return out, nil
}

// PromptFor renders the prompt fragment for one agent: at most 3 skills,
// 4000 chars each.
func (s *Skills) PromptFor(agent *hub.Agent) string {
	if agent == nil {
		return ""
	}
	names := agent.Skills
	if len(names) > MAX_SKILLS {
		names = names[:MAX_SKILLS]
	}
	if len(names) == 0 {
		return ""
	}
	var parts []string
	for _, name := range names {
		if body := s.Load(name); body != "" {
			parts = append(parts, "【"+name+"】\n"+short(body, MAX_BODY))
		}
	}
	if len(parts) == 0 {
		return ""
	}
	return "\n以下是本执行端被指派的技能指引，请在完成工作时遵循：\n" + strings.Join(parts, "\n\n")
}

// DropCache clears the body cache.
func (s *Skills) DropCache() {
	s.mu.Lock()
	defer s.mu.Unlock()
	s.cache = map[string]*string{}
}
