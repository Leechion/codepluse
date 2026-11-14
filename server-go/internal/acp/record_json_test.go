package acp

import (
	"encoding/json"
	"testing"

	"harnesshub/internal/hub"
)

func TestRecordJSONSerialisesOneRecord(t *testing.T) {
	r := NewRuntime(hub.NewHub(), ":memory:", 0)
	r.records = append(r.records,
		&SessionRecord{ID: "s1", Key: "k1", Turns: []*Turn{{ID: "t1", Status: "completed", Output: "hello"}}},
		&SessionRecord{ID: "s2", Key: "k2"},
	)

	var got map[string]any
	if err := json.Unmarshal(r.RecordJSON("s1"), &got); err != nil {
		t.Fatal(err)
	}
	if got["id"] != "s1" {
		t.Fatalf("wrong record: %v", got["id"])
	}
	turns, ok := got["turns"].([]any)
	if !ok || len(turns) != 1 {
		t.Fatalf("turns missing or wrong: %#v", got["turns"])
	}

	var missing map[string]any
	if err := json.Unmarshal(r.RecordJSON("does-not-exist"), &missing); err != nil {
		t.Fatal(err)
	}
	if len(missing) != 0 {
		t.Fatalf("unknown id should yield {}, got %v", missing)
	}

	var all []map[string]any
	if err := json.Unmarshal(r.ListJSON(""), &all); err != nil {
		t.Fatal(err)
	}
	if len(all) != 2 {
		t.Fatalf("ListJSON returned %d records, want 2", len(all))
	}
}
