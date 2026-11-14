package codeg

import (
	"sync"
	"testing"
)

func TestMergeLockIsScopedPerServerAndFolder(t *testing.T) {
	a, b := &Server{}, &Server{}
	if a.mergeLock(7) != a.mergeLock(7) {
		t.Fatal("same folder must share one lock")
	}
	if a.mergeLock(7) == a.mergeLock(8) {
		t.Fatal("different folders must not share a lock")
	}
	if a.mergeLock(7) == b.mergeLock(7) {
		t.Fatal("two servers must not share a lock")
	}

	// Exercise concurrent LoadOrStore under -race.
	var wg sync.WaitGroup
	for i := 0; i < 64; i++ {
		wg.Add(1)
		go func(i int) {
			defer wg.Done()
			_ = a.mergeLock(i % 8)
		}(i)
	}
	wg.Wait()
}
