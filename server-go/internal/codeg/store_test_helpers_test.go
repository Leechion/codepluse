package codeg

import "testing"

// reloadStore opens the same on-disk store again and closes it at cleanup so
// SQLite file handles cannot keep the temporary directory alive on Windows.
func reloadStore(t *testing.T, file string) *Store {
	t.Helper()
	s, err := newStore(file)
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() { _ = s.Close() })
	return s
}
