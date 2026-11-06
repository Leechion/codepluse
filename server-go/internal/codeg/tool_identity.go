package codeg

import "strings"

// Port of acp/delegation/mod.rs::is_codeg_mcp_tool: namespace and tool
// boundaries are significant; near matches must keep their permission card.
func internalToolName(name string) string {
	name = strings.ToLower(strings.TrimSpace(name))
	separators := []string{"__", "/", ":", "."}
	for _, sep := range separators {
		if strings.HasPrefix(name, "mcp"+sep) {
			name = strings.TrimPrefix(name, "mcp"+sep)
			break
		}
	}
	at := -1
	boundary := ""
	for _, sep := range separators {
		if index := strings.Index(name, sep); index >= 0 && (at < 0 || index < at) {
			at = index
			boundary = sep
		}
	}
	if at < 0 {
		return ""
	}
	server, tool := strings.TrimSpace(name[:at]), strings.TrimSpace(name[at+len(boundary):])
	if server != "codeg-mcp" && server != "codeg_mcp" {
		return ""
	}
	for _, definition := range ToolDefinitions() {
		if definition["name"] == tool {
			return tool
		}
	}
	return ""
}
func (s *Server) allowInternalTool(runtimeID string, toolCall Row) bool {
	if internalToolName(text(toolCall["title"])) == "" {
		return false
	}
	s.mu.Lock()
	defer s.mu.Unlock()
	for _, c := range s.connections {
		if c.RuntimeID == runtimeID && s.callers[c.ID] != nil {
			return true
		}
	}
	return false
}
