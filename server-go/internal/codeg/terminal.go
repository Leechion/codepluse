package codeg

import (
	"bufio"
	"fmt"
	"io"
	"os/exec"
	"path/filepath"
	"strings"
	"sync"
	"unicode/utf8"
)

type nativePTY interface {
	io.ReadWriteCloser
	Resize(int, int) error
	Wait() (uint32, error)
}
type TerminalSession struct {
	mu                            sync.Mutex
	writeMu                       sync.Mutex
	closeOnce                     sync.Once
	ID, Generation, Title, Output string
	Seq                           uint64
	Alive                         bool
	ExitCode                      *uint32
	Cols, Rows                    int
	pty                           nativePTY
	readDone                      chan struct{}
}

func (t *TerminalSession) close() { t.closeOnce.Do(func() { _ = t.pty.Close() }) }
func terminalShell(shell string) (string, error) {
	if shell == "" {
		for _, candidate := range []string{"pwsh.exe", "powershell.exe", "cmd.exe"} {
			if path, err := exec.LookPath(candidate); err == nil {
				return path, nil
			}
		}
		return "", fmt.Errorf("找不到可用的终端 Shell")
	}
	if strings.ContainsAny(shell, "\r\n\x00") {
		return "", fmt.Errorf("Shell 路径无效")
	}
	return exec.LookPath(shell)
}
func (s *Server) terminalCommand(command string, a Row) (any, error) {
	tid := text(a["terminalId"])
	if command == "get_available_terminal_shells" || command == "probe_terminal_shell_path" {
		shell := text(a["path"])
		if command == "get_available_terminal_shells" {
			s.store.mu.Lock()
			if saved, ok := s.store.Settings["get_system_terminal_settings"].(map[string]any); ok {
				shell = text(saved["default_shell"])
			}
			s.store.mu.Unlock()
		}
		resolved, err := terminalShell(shell)
		if err != nil {
			return nil, err
		}
		options := []Row{{"id": "default", "label_key": "terminalSystemDefault", "value": nil, "exists": true, "accepts_custom_path": false}}
		for _, name := range []string{"pwsh.exe", "powershell.exe", "cmd.exe"} {
			path, err := exec.LookPath(name)
			options = append(options, Row{"id": name, "label_key": map[string]string{"pwsh.exe": "terminalPowerShell7", "powershell.exe": "terminalWindowsPowerShell", "cmd.exe": "terminalCmd"}[name], "value": path, "exists": err == nil, "accepts_custom_path": false})
		}
		options = append(options, Row{"id": "custom", "label_key": "terminalShellCustom", "value": shell, "exists": true, "accepts_custom_path": true})
		return Row{"options": options, "resolved_shell": resolved}, nil
	}
	if command == "terminal_spawn" {
		cwd, err := s.Resolve(text(a["workingDir"]))
		if err != nil {
			return nil, err
		}
		shell := text(a["shell"])
		if shell == "" {
			s.store.mu.Lock()
			if saved, ok := s.store.Settings["get_system_terminal_settings"].(map[string]any); ok {
				shell = text(saved["default_shell"])
			}
			s.store.mu.Unlock()
		}
		shell, err = terminalShell(shell)
		if err != nil {
			return nil, err
		}
		if tid == "" {
			tid = id()
		}
		s.terminalMu.Lock()
		defer s.terminalMu.Unlock()
		if old := s.terminals[tid]; old != nil {
			old.mu.Lock()
			alive := old.Alive
			old.mu.Unlock()
			if alive {
				return tid, nil
			}
		}
		pty, err := startNativePTY(shell, cwd, 120, 30)
		if err != nil {
			return nil, err
		}
		t := &TerminalSession{ID: tid, Generation: id(), Title: filepath.Base(shell), Alive: true, Cols: 120, Rows: 30, pty: pty, readDone: make(chan struct{})}
		s.terminals[tid] = t
		go s.readTerminal(t)
		go func() {
			code, waitErr := pty.Wait()
			t.close()
			<-t.readDone
			t.mu.Lock()
			t.Alive = false
			if waitErr == nil {
				t.ExitCode = &code
			}
			generation := t.Generation
			t.mu.Unlock()
			exitText := ""
			if waitErr == nil {
				exitText = fmt.Sprint(code)
			}
			s.emit("terminal://exit/"+tid, Row{"terminal_id": tid, "data": exitText, "seq": 0, "generation": generation})
		}()
		if initial := text(a["initialCommand"]); initial != "" {
			go func() { t.writeMu.Lock(); defer t.writeMu.Unlock(); _, _ = pty.Write([]byte(initial + "\r")) }()
		}
		return tid, nil
	}
	s.terminalMu.Lock()
	t := s.terminals[tid]
	s.terminalMu.Unlock()
	switch command {
	case "terminal_list":
		out := []Row{}
		s.terminalMu.Lock()
		for _, t := range s.terminals {
			out = append(out, Row{"id": t.ID, "title": t.Title})
		}
		s.terminalMu.Unlock()
		return out, nil
	case "terminal_snapshot":
		if t == nil {
			return Row{"exists": false, "alive": false, "data": "", "seq": 0, "generation": nil, "exit_code": nil, "cols": nil, "rows": nil}, nil
		}
		t.mu.Lock()
		defer t.mu.Unlock()
		return Row{"exists": true, "alive": t.Alive, "data": t.Output, "seq": t.Seq, "generation": t.Generation, "exit_code": t.ExitCode, "cols": t.Cols, "rows": t.Rows}, nil
	case "terminal_kill":
		if t == nil {
			return nil, nil
		}
		if a["keepOutput"] != true {
			s.terminalMu.Lock()
			delete(s.terminals, tid)
			s.terminalMu.Unlock()
		}
		t.close()
		return nil, nil
	case "terminal_write":
		if t == nil {
			return nil, fmt.Errorf("终端不存在")
		}
		t.writeMu.Lock()
		defer t.writeMu.Unlock()
		_, err := t.pty.Write([]byte(text(a["data"])))
		return nil, err
	case "terminal_resize":
		if t == nil {
			return nil, fmt.Errorf("终端不存在")
		}
		cols, rows := number(a["cols"]), number(a["rows"])
		if cols < 1 || rows < 1 || cols > 1000 || rows > 1000 {
			return nil, fmt.Errorf("终端尺寸无效")
		}
		if err := t.pty.Resize(cols, rows); err != nil {
			return nil, err
		}
		t.mu.Lock()
		t.Cols, t.Rows = cols, rows
		t.mu.Unlock()
		return nil, nil
	}
	return nil, fmt.Errorf("未知终端接口：%s", command)
}
func (s *Server) readTerminal(t *TerminalSession) {
	defer close(t.readDone)
	reader := bufio.NewReader(t.pty)
	buf := make([]byte, 8192)
	pending := []byte{}
	for {
		n, err := reader.Read(buf)
		pending = append(pending, buf[:n]...)
		end := 0
		for end < len(pending) && utf8.FullRune(pending[end:]) {
			_, size := utf8.DecodeRune(pending[end:])
			end += size
		}
		if end > 0 {
			chunk := strings.ToValidUTF8(string(pending[:end]), "�")
			pending = append([]byte{}, pending[end:]...)
			t.mu.Lock()
			t.Output += chunk
			if len(t.Output) > 2<<20 {
				cut := len(t.Output) - (2 << 20)
				for cut < len(t.Output) && !utf8.RuneStart(t.Output[cut]) {
					cut++
				}
				t.Output = t.Output[cut:]
			}
			t.Seq++
			s.emit("terminal://output/"+t.ID, Row{"terminal_id": t.ID, "data": chunk, "seq": t.Seq, "generation": t.Generation})
			t.mu.Unlock()
		}
		if err != nil {
			return
		}
	}
}
func (s *Server) Close() {
	s.watchMu.Lock()
	for _, w := range s.watches {
		w.close()
	}
	s.watchMu.Unlock()
	s.terminalMu.Lock()
	all := []*TerminalSession{}
	for _, t := range s.terminals {
		all = append(all, t)
	}
	s.terminalMu.Unlock()
	for _, t := range all {
		t.close()
	}
}
