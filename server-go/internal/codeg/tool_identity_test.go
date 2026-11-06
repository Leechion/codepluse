package codeg

import "testing"

func TestInternalToolIdentityRequiresExactBoundaries(t *testing.T) {
	for _, name := range []string{"mcp__codeg-mcp__list_agents", "codeg-mcp/delegate_to_agent", "codeg_mcp:get_delegation_status", " MCP.codeg-mcp.cancel_delegation "} {
		if internalToolName(name) == "" {
			t.Errorf("rejected own tool %q", name)
		}
	}
	for _, name := range []string{"mcp__other-codeg-mcp__list_agents", "codeg-mcp-x/list_agents", "codeg-mcp/list_agents_twice", "codeg/mcp_list_agents", "mcp__codeg-mcp__Bash", "Approve Plan", "Edit file"} {
		if internalToolName(name) != "" {
			t.Errorf("accepted foreign or native tool %q", name)
		}
	}
}
