package codeg

// maskSecret renders a credential for display/echo without revealing it.
// Keep a short prefix and suffix so a user can recognise which key is
// configured; never return the full value from an HTTP response.
func maskSecret(value string) string {
	if value == "" {
		return ""
	}
	runes := []rune(value)
	if len(runes) <= 8 {
		return "********"
	}
	return string(runes[:3]) + "…" + string(runes[len(runes)-4:])
}
