package textutil

import "testing"

func TestTruncateRunesKeepsValidUTF8(t *testing.T) {
	cases := []struct {
		in   string
		n    int
		want string
	}{
		{"abcdef", 3, "abc"},
		{"abcd", 10, "abcd"},
		{"中文字符", 2, "中文"},
		{"中文字符", 0, ""},
		{"a中b", 2, "a中"},
		{"🙂🙂", 1, "🙂"},
	}
	for _, tc := range cases {
		if got := TruncateRunes(tc.in, tc.n); got != tc.want {
			t.Fatalf("TruncateRunes(%q,%d) = %q, want %q", tc.in, tc.n, got, tc.want)
		}
	}
}
