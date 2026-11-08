//go:build !windows

package codeg

import (
	"fmt"
	"os/exec"
)

func hideCommand(cmd *exec.Cmd) {}
func startNativePTY(shell, cwd string, cols, rows int) (nativePTY, error) {
	return nil, fmt.Errorf("此终端移植当前使用 Windows ConPTY")
}
