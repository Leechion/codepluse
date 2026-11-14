package providers

import "testing"

func TestSaveSwitchAndKeepKey(t *testing.T) {
	root := t.TempDir()
	t.Setenv("USERPROFILE", root)
	p := Provider{Name: "first", Agent: "dsh", URL: "https://example.test/v1", Model: "model-a", Key: "secret"}
	if _, err := SaveConfiguration(root, p); err != nil {
		t.Fatal(err)
	}
	pub, err := Public(root)
	if err != nil {
		t.Fatal(err)
	}
	if len(pub.Providers) != 1 || !pub.Providers[0].HasKey || pub.Providers[0].Key != "" {
		t.Fatal("public listing must hide credentials")
	}
	id := pub.Providers[0].ID
	if err = Activate(root, "dsh", id); err != nil {
		t.Fatal(err)
	}
	p.ID = id
	p.Model = "model-b"
	p.Key = ""
	if _, err = SaveConfiguration(root, p); err != nil {
		t.Fatal(err)
	}
	active, ok, err := Active(root, "dsh")
	if err != nil || !ok || active.Key != "secret" || active.Model != "model-b" {
		t.Fatalf("unexpected active provider: %v", err)
	}
	if err = Delete(root, id); err != nil {
		t.Fatal(err)
	}
	_, ok, err = Active(root, "dsh")
	if err != nil || ok {
		t.Fatal("deleted provider must not remain active")
	}
}
