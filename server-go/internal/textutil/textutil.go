// Package textutil holds string helpers shared by the backend packages.
package textutil

import "unicode/utf8"

// TruncateRunes cuts s to at most n runes without splitting a multi-byte
// character. Byte slicing (s[:n]) on Chinese text, emoji or accented input
// produces invalid UTF-8, which JSON then renders as U+FFFD.
func TruncateRunes(s string, n int) string {
	if n <= 0 {
		return ""
	}
	if len(s) <= n { // bytes <= n implies runes <= n
		return s
	}
	count := 0
	for i := range s {
		if count == n {
			return s[:i]
		}
		count++
	}
	return s
}

// TruncateRunesEllipsis is TruncateRunes with a trailing ellipsis when the
// value was actually shortened.
func TruncateRunesEllipsis(s string, n int) string {
	if utf8.RuneCountInString(s) <= n {
		return s
	}
	return TruncateRunes(s, n) + "…"
}
