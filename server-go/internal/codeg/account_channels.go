package codeg

import (
	"context"
	"fmt"
	"harnesshub/internal/accountchannels"
	"net/url"
)

func (s *Server) accountChannelCommand(command string, a Row) (any, error) {
	client, err := accountchannels.Ensure(s.Root)
	if err != nil {
		return nil, err
	}
	method, path := "GET", "/status"
	var body any
	switch command {
	case "codepluse_channel_status":
	case "codepluse_channel_login":
		method, path, body = "POST", "/login/start", Row{"provider": text(a["provider"])}
	case "codepluse_channel_login_status":
		path = "/login/" + url.PathEscape(text(a["id"]))
	case "codepluse_channel_cancel":
		method, path, body = "POST", "/login/cancel", Row{"id": text(a["id"])}
	case "codepluse_channel_logout":
		method, path, body = "POST", "/logout", Row{"provider": text(a["provider"]), "accountId": text(a["accountId"])}
	default:
		return nil, fmt.Errorf("账号渠道操作无效")
	}
	return client.Call(context.Background(), method, path, body)
}
