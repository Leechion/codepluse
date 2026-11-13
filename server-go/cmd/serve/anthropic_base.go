package main

import "strings"

// anthropicBaseURL normalizes an Anthropic-compatible endpoint before it is
// handed to the Claude Code adapter as ANTHROPIC_BASE_URL.
//
// The SDK appends "/v1/messages" itself. Some providers — and CC Switch
// presets of them, 讯飞星火 MaaS is one — store the FULL messages endpoint in
// the provider configuration, so passing it through makes every request go to
// /v1/messages/v1/messages, which the gateway answers with 404. Trimming the
// endpoint suffix turns such a value back into the base the SDK expects.
func anthropicBaseURL(raw string) string {
	u := strings.TrimSpace(raw)
	if u == "" {
		return u
	}
	u = strings.TrimRight(u, "/")
	for _, suffix := range []string{"/v1/messages", "/messages"} {
		if strings.HasSuffix(strings.ToLower(u), suffix) {
			u = u[:len(u)-len(suffix)]
			u = strings.TrimRight(u, "/")
		}
	}
	return u
}
