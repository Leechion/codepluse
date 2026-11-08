//go:build windows

package codeg

import (
	"context"
	"github.com/UserExistsError/conpty"
	"os"
	"os/exec"
	"strings"
	"syscall"
)

func hideCommand(cmd *exec.Cmd) { cmd.SysProcAttr = &syscall.SysProcAttr{HideWindow: true} }

type windowsPTY struct{ *conpty.ConPty }

func (p *windowsPTY) Wait() (uint32, error) { return p.ConPty.Wait(context.Background()) }
func startNativePTY(shell, cwd string, cols, rows int) (nativePTY, error) {
	command := `"` + shell + `"`
	if strings.Contains(strings.ToLower(shell), "powershell") || strings.EqualFold(filepathBase(shell), "pwsh.exe") {
		command += " -NoLogo"
	}
	p, err := conpty.Start(command, conpty.ConPtyWorkDir(cwd), conpty.ConPtyDimensions(cols, rows), conpty.ConPtyEnv(append(os.Environ(), "TERM=xterm-256color")))
	if err != nil {
		return nil, err
	}
	return &windowsPTY{p}, nil
}
func filepathBase(p string) string {
	p = strings.ReplaceAll(p, `\`, "/")
	parts := strings.Split(p, "/")
	return parts[len(parts)-1]
}
