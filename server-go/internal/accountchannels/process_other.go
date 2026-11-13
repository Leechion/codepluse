//go:build !windows

package accountchannels

import "os/exec"

func hideProcess(cmd *exec.Cmd) {}
