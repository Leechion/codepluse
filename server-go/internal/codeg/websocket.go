package codeg

import (
	"net/http"
	"net/url"
	"sync"
	"time"

	"github.com/gorilla/websocket"
)

const (
	wsIdleTimeout            = 90 * time.Second
	wsPingInterval           = 30 * time.Second
	maxSubscriptionsPerSocket = 64
)

func (s *Server) websocket(w http.ResponseWriter, r *http.Request) {
	upgrade := websocket.Upgrader{Subprotocols: []string{"codeg-events"}, CheckOrigin: func(r *http.Request) bool {
		origin := r.Header.Get("Origin")
		if origin == "" {
			// Native clients (desktop shell, tests) send no Origin; the HTTP
			// guard has already rejected cross-site browser requests.
			return true
		}
		u, err := url.Parse(origin)
		if err != nil || u.Host == "" {
			return false
		}
		return s.originAllowed(u.Host, r.Host)
	}}
	conn, err := upgrade.Upgrade(w, r, nil)
	if err != nil {
		return
	}
	defer conn.Close()
	conn.SetReadLimit(64 << 10)
	// Without a read deadline a half-open socket keeps its goroutine and
	// subscriber slot forever; the pong handler extends the deadline while the
	// peer is alive.
	_ = conn.SetReadDeadline(time.Now().Add(wsIdleTimeout))
	conn.SetPongHandler(func(string) error {
		return conn.SetReadDeadline(time.Now().Add(wsIdleTimeout))
	})
	ch := make(chan Row, 256)
	s.mu.Lock()
	s.subscribers[ch] = true
	s.mu.Unlock()
	defer func() { s.mu.Lock(); delete(s.subscribers, ch); s.mu.Unlock() }()
	var writeMu sync.Mutex
	write := func(v any) error {
		writeMu.Lock()
		defer writeMu.Unlock()
		_ = conn.SetWriteDeadline(time.Now().Add(10 * time.Second))
		return conn.WriteJSON(v)
	}
	stopPing := make(chan struct{})
	defer close(stopPing)
	go func() {
		ticker := time.NewTicker(wsPingInterval)
		defer ticker.Stop()
		for {
			select {
			case <-stopPing:
				return
			case <-ticker.C:
				writeMu.Lock()
				_ = conn.SetWriteDeadline(time.Now().Add(10 * time.Second))
				err := conn.WriteMessage(websocket.PingMessage, nil)
				writeMu.Unlock()
				if err != nil {
					_ = conn.Close()
					return
				}
			}
		}
	}()
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
	// lastSent is the highest seq forwarded to each subscription. A gap means
	// the per-connection buffer dropped events for this client; we detach with
	// "lagged" so it re-attaches with its cursor and gets a replay.
	lastSent := map[string]uint64{}
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
				sub := text(cmd["subscription_id"])
				delete(subscriptions, sub)
				delete(lastSent, sub)
			case "attach":
				sub, cid := text(cmd["subscription_id"]), text(cmd["connection_id"])
				if _, exists := subscriptions[sub]; !exists && len(subscriptions) >= maxSubscriptionsPerSocket {
					if write(Row{"type": "detached", "subscription_id": sub, "reason": "too_many_subscriptions"}) != nil {
						return
					}
					continue
				}
				if since := uint64(number(cmd["since_seq"])); since > 0 {
					if events, high, ok := s.replaySince(cid, since); ok {
						subscriptions[sub] = cid
						lastSent[sub] = high
						if write(Row{"type": "replay", "subscription_id": sub, "connection_id": cid, "events": events, "high_water_seq": high}) != nil {
							return
						}
						continue
					}
				}
				snap := s.snapshot(cid)
				if snap == nil {
					if write(Row{"type": "detached", "subscription_id": sub, "reason": "connection_gone"}) != nil {
						return
					}
					continue
				}
				subscriptions[sub] = cid
				lastSent[sub] = uint64(number(snap["event_seq"]))
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
				seq := eventSeq(e)
				for sub, cid := range subscriptions {
					if e["connection_id"] != cid {
						continue
					}
					last := lastSent[sub]
					if last != 0 {
						if seq <= last {
							// Already reflected in the snapshot/replay we just sent.
							// The broadcast channel still holds events queued before
							// the attach was processed; forwarding them would apply
							// the same text deltas twice.
							continue
						}
						if seq > last+1 {
							delete(subscriptions, sub)
							delete(lastSent, sub)
							if write(Row{"type": "detached", "subscription_id": sub, "reason": "lagged"}) != nil {
								return
							}
							continue
						}
					}
					lastSent[sub] = seq
					if write(Row{"type": "event", "subscription_id": sub, "envelope": e}) != nil {
						return
					}
				}
			}
		}
	}
}

// replaySince returns the retained events after sinceSeq for a connection.
// ok=false tells the caller to send a full snapshot: the cursor is unknown or
// the ring buffer has already rotated past the gap.
func (s *Server) replaySince(cid string, since uint64) ([]Row, uint64, bool) {
	s.mu.Lock()
	defer s.mu.Unlock()
	c := s.connections[cid]
	if c == nil {
		return nil, 0, false
	}
	high := c.Seq
	if since == 0 || since > high {
		return nil, high, false
	}
	if since == high {
		return []Row{}, high, true
	}
	var oldest uint64
	if len(c.Events) > 0 {
		oldest = eventSeq(c.Events[0])
	}
	if oldest == 0 || since+1 < oldest {
		return nil, high, false
	}
	out := make([]Row, 0, int(high-since))
	for _, e := range c.Events {
		if eventSeq(e) > since {
			out = append(out, clone(e))
		}
	}
	return out, high, true
}

func eventSeq(e Row) uint64 {
	if n := number(e["seq"]); n > 0 {
		return uint64(n)
	}
	return 0
}
