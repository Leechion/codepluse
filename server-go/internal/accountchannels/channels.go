// Package accountchannels manages the local WorkBuddy/Qoder protocol process.
// Credentials never cross the browser boundary; only models and account metadata do.
package accountchannels

import (
	"bufio"
	"bytes"
	"context"
	"crypto/rand"
	"encoding/hex"
	"encoding/json"
	"fmt"
	"io"
	"net/http"
	"net/url"
	"os"
	"os/exec"
	"path/filepath"
	"strings"
	"sync"
	"time"
)

type Model struct {
	ID   string `json:"id"`
	Name string `json:"name"`
}

func Supported(id string) bool {
	return id == "buddy" || id == "workbuddy" || id == "qoder" || id == "qodercn"
}

type Channel struct {
	ID         string           `json:"id"`
	Name       string           `json:"name"`
	Configured bool             `json:"configured"`
	Accounts   []map[string]any `json:"accounts"`
	Models     []Model          `json:"models"`
}
type Client struct {
	URL, Key string
	cmd      *exec.Cmd
	input    io.WriteCloser
	done     chan struct{}
}

var clients = struct {
	sync.Mutex
	values map[string]*Client
}{values: map[string]*Client{}}

func Available(root string) bool {
	_, err := os.Stat(filepath.Join(root, "bridges", "account-channels", "server.mjs"))
	return err == nil
}

func running(root string) *Client {
	var state struct {
		URL string `json:"url"`
		Key string `json:"key"`
	}
	b, err := os.ReadFile(filepath.Join(root, ".data", "account-channels", "runtime.json"))
	if err != nil || json.Unmarshal(b, &state) != nil || len(state.Key) < 32 {
		return nil
	}
	u, err := url.Parse(state.URL)
	if err != nil || u.Scheme != "http" || u.Hostname() != "127.0.0.1" || u.User != nil || u.Path != "" || u.RawQuery != "" || u.Fragment != "" {
		return nil
	}
	c := &Client{URL: state.URL, Key: state.Key, done: make(chan struct{})}
	ctx, cancel := context.WithTimeout(context.Background(), time.Second)
	defer cancel()
	if _, err := c.Call(ctx, "GET", "/health", nil); err != nil {
		return nil
	}
	return c
}

func Ensure(root string) (*Client, error) {
	root, _ = filepath.Abs(root)
	clients.Lock()
	defer clients.Unlock()
	if c := clients.values[root]; c != nil {
		if c.cmd == nil {
			if shared := running(root); shared != nil {
				clients.values[root] = shared
				return shared, nil
			}
			delete(clients.values, root)
		} else {
			select {
			case <-c.done:
				delete(clients.values, root)
			default:
				return c, nil
			}
		}
	}
	if c := running(root); c != nil {
		clients.values[root] = c
		return c, nil
	}
	if !Available(root) {
		return nil, fmt.Errorf("账号渠道适配器未安装")
	}
	node := os.Getenv("HARNESS_HUB_NODE_BIN")
	if node == "" {
		var err error
		node, err = exec.LookPath("node")
		if err != nil {
			return nil, fmt.Errorf("账号渠道需要 Node.js 22 或更新版本")
		}
	}
	data := filepath.Join(root, ".data", "account-channels")
	if err := os.MkdirAll(data, 0700); err != nil {
		return nil, err
	}
	secret := make([]byte, 32)
	if _, err := rand.Read(secret); err != nil {
		return nil, err
	}
	c := &Client{Key: hex.EncodeToString(secret), done: make(chan struct{})}
	c.cmd = exec.Command(node, filepath.Join(root, "bridges", "account-channels", "server.mjs"), data)
	c.cmd.Dir = root
	for _, value := range os.Environ() {
		if !strings.HasPrefix(value, "CODEPLUSE_CHANNEL_KEY=") {
			c.cmd.Env = append(c.cmd.Env, value)
		}
	}
	c.cmd.Env = append(c.cmd.Env, "CODEPLUSE_CHANNEL_KEY="+c.Key)
	hideProcess(c.cmd)
	log, err := os.OpenFile(filepath.Join(data, "bridge.log"), os.O_CREATE|os.O_APPEND|os.O_WRONLY, 0600)
	if err != nil {
		return nil, err
	}
	c.cmd.Stderr = log
	c.input, err = c.cmd.StdinPipe()
	if err != nil {
		log.Close()
		return nil, err
	}
	output, err := c.cmd.StdoutPipe()
	if err != nil {
		c.input.Close()
		log.Close()
		return nil, err
	}
	if err = c.cmd.Start(); err != nil {
		c.input.Close()
		log.Close()
		return nil, err
	}
	ready := make(chan string, 1)
	go func() {
		scanner := bufio.NewScanner(output)
		if scanner.Scan() {
			ready <- scanner.Text()
		}
		io.Copy(io.Discard, output)
	}()
	go func() { _ = c.cmd.Wait(); log.Close(); close(c.done) }()
	select {
	case line := <-ready:
		var state struct {
			URL string `json:"url"`
		}
		if json.Unmarshal([]byte(line), &state) != nil {
			c.input.Close()
			return nil, fmt.Errorf("账号渠道启动响应无效")
		}
		u, e := url.Parse(state.URL)
		if e != nil || u.Scheme != "http" || u.Hostname() != "127.0.0.1" {
			c.input.Close()
			return nil, fmt.Errorf("账号渠道监听地址无效")
		}
		c.URL = state.URL
		clients.values[root] = c
		return c, nil
	case <-c.done:
		for i := 0; i < 5; i++ {
			if shared := running(root); shared != nil {
				clients.values[root] = shared
				return shared, nil
			}
			time.Sleep(100 * time.Millisecond)
		}
		return nil, fmt.Errorf("账号渠道启动失败，请查看 .data/account-channels/bridge.log")
	case <-time.After(30 * time.Second):
		c.input.Close()
		_ = c.cmd.Process.Kill()
		return nil, fmt.Errorf("账号渠道启动超时")
	}
}

func (c *Client) Call(ctx context.Context, method, path string, body any) (json.RawMessage, error) {
	var data io.Reader
	if body != nil {
		b, err := json.Marshal(body)
		if err != nil {
			return nil, err
		}
		data = bytes.NewReader(b)
	}
	req, err := http.NewRequestWithContext(ctx, method, c.URL+path, data)
	if err != nil {
		return nil, err
	}
	req.Header.Set("Authorization", "Bearer "+c.Key)
	req.Header.Set("Content-Type", "application/json")
	response, err := (&http.Client{Timeout: 45 * time.Second}).Do(req)
	if err != nil {
		return nil, fmt.Errorf("账号渠道不可达: %w", err)
	}
	defer response.Body.Close()
	b, err := io.ReadAll(io.LimitReader(response.Body, 8<<20))
	if err != nil {
		return nil, err
	}
	if response.StatusCode >= 400 {
		var reply struct {
			Error struct {
				Message string `json:"message"`
			} `json:"error"`
		}
		_ = json.Unmarshal(b, &reply)
		if reply.Error.Message != "" {
			return nil, fmt.Errorf("%s", reply.Error.Message)
		}
		return nil, fmt.Errorf("账号渠道 HTTP %d", response.StatusCode)
	}
	if !json.Valid(b) {
		return nil, fmt.Errorf("账号渠道响应格式无效")
	}
	return b, nil
}
func List(root string) ([]Channel, *Client, error) {
	c, err := Ensure(root)
	if err != nil {
		return nil, nil, err
	}
	b, err := c.Call(context.Background(), "GET", "/status", nil)
	if err != nil {
		return nil, c, err
	}
	var rows []Channel
	err = json.Unmarshal(b, &rows)
	return rows, c, err
}
