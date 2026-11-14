package codeg

import (
	"bytes"
	"encoding/base64"
	"encoding/json"
	"net/http/httptest"
	"strings"
	"testing"
	"time"

	"harnesshub/internal/acp"
	"harnesshub/internal/hub"
)

func TestNativeFeedbackWithImagePersistsAndReachesAgent(t *testing.T) {
	s := testServer(t)
	launch := s.Runtime.LaunchFn
	s.Runtime.LaunchFn = func(a *hub.Agent, permission, cwd string) (*acp.Launch, error) {
		v, err := launch(a, permission, cwd)
		if err == nil {
			v.Args = append(v.Args, "--fork", "--steer", "--images", "--embedded")
		}
		return v, err
	}
	cid := command(t, s, "acp_connect", Row{"agentType": "codex"}).(string)
	if err := s.prompt(Row{"connectionId": cid, "blocks": []any{Row{"type": "text", "text": "special-wait"}}}); err != nil {
		t.Fatal(err)
	}
	data := base64.StdEncoding.EncodeToString([]byte("image data"))
	feedback := Row{"connectionId": cid, "text": "see image", "blocks": []any{Row{"type": "text", "text": "see image"}, Row{"type": "image", "mime_type": "image/png", "data": data}, Row{"type": "resource_link", "name": "spec", "uri": "https://example.test/spec"}, Row{"type": "resource", "uri": "memory://note", "mime_type": "text/plain", "text": "note body"}}}
	deadline := time.Now().Add(3 * time.Second)
	for {
		_, err := s.submitFeedback(feedback)
		if err == nil {
			break
		}
		if !strings.Contains(err.Error(), "no active turn") || time.Now().After(deadline) {
			t.Fatal(err)
		}
		time.Sleep(10 * time.Millisecond)
	}
	for {
		s.mu.Lock()
		busy, conv := s.connections[cid].Busy, s.connections[cid].ConversationID
		s.mu.Unlock()
		if !busy {
			restored := reloadStore(t, s.store.file)
			turns := restored.turnsLocked(conv)
			encoded, _ := json.Marshal(turns)
			if len(turns) != 3 || !bytes.Contains(encoded, []byte("steered:see imageimage:image/png:"+data+"resource:spec:https://example.test/spec"+"embedded:memory://note:note body")) || !bytes.Contains(encoded, []byte(`"mime_type":"image/png"`)) || !bytes.Contains(encoded, []byte("[spec](https://example.test/spec)")) {
				t.Fatalf("missing image or result: %s", encoded)
			}
			return
		}
		if time.Now().After(deadline) {
			t.Fatal("turn did not settle")
		}
		time.Sleep(10 * time.Millisecond)
	}
}

func TestNativeFeedbackSteersAndPersistsBeforeReply(t *testing.T) {
	s := testServer(t)
	launch := s.Runtime.LaunchFn
	s.Runtime.LaunchFn = func(a *hub.Agent, permission, cwd string) (*acp.Launch, error) {
		v, err := launch(a, permission, cwd)
		if err == nil {
			v.Args = append(v.Args, "--fork", "--steer")
		}
		return v, err
	}
	cid := command(t, s, "acp_connect", Row{"agentType": "codex"}).(string)
	if s.snapshot(cid)["native_steering_available"] != true {
		t.Fatal("native steering not advertised")
	}
	if err := s.prompt(Row{"connectionId": cid, "blocks": []any{Row{"type": "text", "text": "special-wait"}}}); err != nil {
		t.Fatal(err)
	}
	deadline := time.Now().Add(3 * time.Second)
	for {
		v, err := s.submitFeedback(Row{"connectionId": cid, "text": "follow up"})
		if err == nil {
			if v.(Row)["status"] != "delivered" {
				t.Fatalf("wrong status: %v", v)
			}
			break
		}
		if !strings.Contains(err.Error(), "no active turn") || time.Now().After(deadline) {
			t.Fatal(err)
		}
		time.Sleep(10 * time.Millisecond)
	}
	for {
		s.mu.Lock()
		busy, conv := s.connections[cid].Busy, s.connections[cid].ConversationID
		s.mu.Unlock()
		if !busy {
			s.store.mu.Lock()
			turns := s.store.turnsLocked(conv)
			s.store.mu.Unlock()
			if len(turns) != 3 || text(turns[1]["role"]) != "user" || text(turns[2]["role"]) != "assistant" {
				t.Fatalf("wrong turn order: %v", turns)
			}
			return
		}
		if time.Now().After(deadline) {
			t.Fatal("turn did not settle")
		}
		time.Sleep(10 * time.Millisecond)
	}
}

func TestFeedbackAfterTurnEndsReturnsComposerFallbackMarker(t *testing.T) {
	s := testServer(t)
	cid := command(t, s, "acp_connect", Row{"agentType": "codex"}).(string)
	body, _ := json.Marshal(Row{"connectionId": cid, "text": "follow up"})
	r := httptest.NewRequest("POST", "/api/submit_session_feedback", bytes.NewReader(body))
	w := httptest.NewRecorder()
	s.ServeHTTP(w, r)
	var result Row
	if err := json.Unmarshal(w.Body.Bytes(), &result); err != nil {
		t.Fatal(err)
	}
	if w.Code != 400 || !strings.Contains(text(result["message"]), "no active turn") {
		t.Fatalf("composer cannot recover the draft: %d %v", w.Code, result)
	}
}
