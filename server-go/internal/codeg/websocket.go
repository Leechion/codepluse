package codeg

import (
	"net/http"
	"net/url"
	"strings"
	"time"

	"github.com/gorilla/websocket"
)

func (s *Server) websocket(w http.ResponseWriter, r *http.Request) {
	upgrade := websocket.Upgrader{Subprotocols: []string{"codeg-events"}, CheckOrigin: func(r *http.Request) bool {
		origin := r.Header.Get("Origin")
		if origin == "" {
			return true
		}
		u, err := url.Parse(origin)
		return err == nil && strings.EqualFold(u.Host, r.Host)
	}}
	conn, err := upgrade.Upgrade(w, r, nil)
	if err != nil {
		return
	}
	defer conn.Close()
	conn.SetReadLimit(64 << 10)
	ch := make(chan Row, 256)
	s.mu.Lock()
	s.subscribers[ch] = true
	s.mu.Unlock()
	defer func() { s.mu.Lock(); delete(s.subscribers, ch); s.mu.Unlock() }()
	write := func(v any) error {
		_ = conn.SetWriteDeadline(time.Now().Add(10 * time.Second))
		return conn.WriteJSON(v)
	}
	if err = write(Row{"channel": "__ready__", "payload": Row{}}); err != nil {
		return
	}
	incoming := make(chan Row, 16)
	done := make(chan struct{})
	go func() {
		defer close(done)
		for {
			var cmd Row
			if conn.ReadJSON(&cmd) != nil {
				return
			}
			select {
			case incoming <- cmd:
			case <-r.Context().Done():
				return
			}
		}
	}()
	subscriptions := map[string]string{}
	for {
		select {
		case <-done:
			return
		case cmd := <-incoming:
			switch text(cmd["action"]) {
			case "ping":
				if write(Row{"type": "pong"}) != nil {
					return
				}
			case "detach":
				delete(subscriptions, text(cmd["subscription_id"]))
			case "attach":
				sub, cid := text(cmd["subscription_id"]), text(cmd["connection_id"])
				snap := s.snapshot(cid)
				if snap == nil {
					if write(Row{"type": "detached", "subscription_id": sub, "reason": "connection_gone"}) != nil {
						return
					}
					continue
				}
				subscriptions[sub] = cid
				if write(Row{"type": "snapshot", "subscription_id": sub, "connection_id": cid, "snapshot": snap, "event_seq": snap["event_seq"]}) != nil {
					return
				}
			}
		case event := <-ch:
			if write(event) != nil {
				return
			}
			if event["channel"] == "acp://event" {
				e, _ := event["payload"].(map[string]any)
				for sub, cid := range subscriptions {
					if e["connection_id"] == cid {
						if write(Row{"type": "event", "subscription_id": sub, "envelope": e}) != nil {
							return
						}
					}
				}
			}
		}
	}
}
