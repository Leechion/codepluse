// Computer tools: the computer_* web contract (codeg-ui computer-api). The
// management layer (settings, permissions, window enumeration, thumbnails) is
// real; the input driver (synthetic mouse/keyboard by the agent) is a
// desktop-shell feature and reports clearly as not implemented here.
package codeg

import (
	"bytes"
	"encoding/base64"
	"fmt"
	"image"
	"image/color"
	"image/png"
	"strings"
	"sync"
	"syscall"
	"unsafe"
)

var (
	user32                    = syscall.NewLazyDLL("user32.dll")
	gdi32                     = syscall.NewLazyDLL("gdi32.dll")
	procEnumWindows           = user32.NewProc("EnumWindows")
	procGetWindowTextW        = user32.NewProc("GetWindowTextW")
	procGetClassNameW         = user32.NewProc("GetClassNameW")
	procIsWindowVisible       = user32.NewProc("IsWindowVisible")
	procGetWindowRect         = user32.NewProc("GetWindowRect")
	procGetWindowThreadProcID = user32.NewProc("GetWindowThreadProcessId")
	procGetDC                 = user32.NewProc("GetDC")
	procReleaseDC             = user32.NewProc("ReleaseDC")
	procPrintWindow           = user32.NewProc("PrintWindow")
	procCreateCompatibleDC    = gdi32.NewProc("CreateCompatibleDC")
	procCreateCompatibleBitmap = gdi32.NewProc("CreateCompatibleBitmap")
	procSelectObject          = gdi32.NewProc("SelectObject")
	procDeleteObject          = gdi32.NewProc("DeleteObject")
	procDeleteDC              = gdi32.NewProc("DeleteDC")
	procGetDIBits             = gdi32.NewProc("GetDIBits")
)

type computerState struct {
	mu        sync.Mutex
	enabled   bool
	prefs     Row
	shared    []Row
	nextSeq   int
}

func (s *Server) computerAvailable() Row {
	return Row{"available": true, "platform": "windows", "driver": false,
		"note": "窗口枚举与截图可用；输入驱动层待实现"}
}

func (s *Server) computerEnabled() bool {
	s.computer.mu.Lock()
	defer s.computer.mu.Unlock()
	return s.computer.enabled
}

func (s *Server) computerCommand(command string, a Row) (any, error) {
	switch command {
	case "computer_available":
		return s.computerAvailable(), nil
	case "get_computer_tools_settings":
		return s.computerSettings(), nil
	case "set_computer_tools_enabled":
		s.computer.mu.Lock()
		s.computer.enabled = a["enabled"] == true
		s.computer.mu.Unlock()
		return nil, nil
	case "set_computer_tools_preferences":
		if prefs, ok := a["prefs"].(Row); ok {
			s.computer.mu.Lock()
			s.computer.prefs = clone(prefs)
			s.computer.mu.Unlock()
		}
		return nil, nil
	case "computer_status", "computer_shared_state":
		s.computer.mu.Lock()
		defer s.computer.mu.Unlock()
		return Row{"enabled": s.computer.enabled, "shared": s.computer.shared,
			"available": true, "driver": false}, nil
	case "computer_request_permission", "computer_open_permission_settings":
		return nil, fmt.Errorf("电脑操作权限申请需要桌面助手，当前不可用")
	case "computer_reveal_helper":
		return nil, fmt.Errorf("桌面助手未安装")
	case "computer_list_shareable_windows", "computer_window_thumbnail",
		"computer_share_window", "computer_share_windows":
		// The master switch must gate every capture/share path, otherwise the
		// setting is cosmetic and a disabled user's screen can still be read.
		if !s.computerEnabled() {
			return nil, fmt.Errorf("电脑操作未启用")
		}
	}
	switch command {
	case "computer_list_shareable_windows":
		return Row{"windows": listShareableWindows()}, nil
	case "computer_window_thumbnail":
		data, err := windowThumbnail(text(a["targetId"]))
		if err != nil {
			return nil, err
		}
		return Row{"mime_type": "image/png", "base64": data}, nil
	case "computer_share_window":
		return s.computerShare(text(a["targetId"]), text(a["level"])), nil
	case "computer_share_windows":
		if ids, ok := a["targetIds"].([]any); ok {
			for _, id := range ids {
				s.computerShare(text(id), text(a["level"]))
			}
		}
		return nil, nil
	case "computer_share_app", "computer_share_screen":
		return nil, fmt.Errorf("整屏/应用共享需要桌面助手，当前不可用")
	case "computer_revoke_all":
		s.computer.mu.Lock()
		s.computer.shared = nil
		s.computer.mu.Unlock()
		s.emit("computer://state", Row{"kind": "revoked"})
		return nil, nil
	case "computer_stop", "computer_stop_key_status":
		return Row{"stopped": true}, nil
	case "computer_indicator_fit":
		return Row{"width": number(a["width"]), "height": number(a["height"])}, nil
	case "computer_driver_info", "computer_driver_uninstall":
		return nil, fmt.Errorf("输入驱动层待实现：本构建提供窗口枚举与截图，不含合成输入")
	}
	return nil, fmt.Errorf("未知的电脑操作命令：%s", command)
}

func (s *Server) computerSettings() Row {
	s.computer.mu.Lock()
	defer s.computer.mu.Unlock()
	if s.computer.prefs == nil {
		s.computer.prefs = Row{"grantTtlMinutes": 15, "stopShortcut": "ctrl+alt+q", "showIndicator": true}
	}
	return Row{"enabled": s.computer.enabled, "preferences": s.computer.prefs, "available": true, "driver": false}
}

func (s *Server) computerShare(targetID, level string) Row {
	s.computer.mu.Lock()
	defer s.computer.mu.Unlock()
	for _, entry := range s.computer.shared {
		if text(entry["target_id"]) == targetID {
			entry["level"] = level
			return entry
		}
	}
	entry := Row{"target_id": targetID, "level": level}
	s.computer.shared = append(s.computer.shared, entry)
	s.emit("computer://agent-grant", Row{"target_id": targetID, "level": level})
	return entry
}

// listShareableWindows enumerates visible top-level windows with titles.
func listShareableWindows() []Row {
	out := []Row{}
	cb := syscall.NewCallback(func(hwnd, lParam uintptr) uintptr {
		visible, _, _ := procIsWindowVisible.Call(hwnd)
		if visible == 0 {
			return 1
		}
		title := windowText(hwnd)
		if strings.TrimSpace(title) == "" {
			return 1
		}
		class := windowClass(hwnd)
		var pid uint32
		procGetWindowThreadProcID.Call(hwnd, uintptr(unsafe.Pointer(&pid)))
		rect := windowRect(hwnd)
		out = append(out, Row{"target_id": fmt.Sprint(hwnd), "title": title, "class": class,
			"process_id": pid, "bounds": rect})
		return 1
	})
	procEnumWindows.Call(cb, 0)
	return out
}

func windowText(hwnd uintptr) string {
	buf := make([]uint16, 256)
	n, _, _ := procGetWindowTextW.Call(hwnd, uintptr(unsafe.Pointer(&buf[0])), 256)
	return syscall.UTF16ToString(buf[:n])
}

func windowClass(hwnd uintptr) string {
	buf := make([]uint16, 128)
	n, _, _ := procGetClassNameW.Call(hwnd, uintptr(unsafe.Pointer(&buf[0])), 128)
	return syscall.UTF16ToString(buf[:n])
}

type rect struct{ Left, Top, Right, Bottom int32 }

func windowRect(hwnd uintptr) rect {
	var r rect
	procGetWindowRect.Call(hwnd, uintptr(unsafe.Pointer(&r)))
	return r
}

// windowThumbnail captures one window via PrintWindow and encodes PNG.
func windowThumbnail(targetID string) (string, error) {
	var hwnd uintptr
	fmt.Sscanf(targetID, "%d", &hwnd)
	if hwnd == 0 {
		return "", fmt.Errorf("窗口不存在")
	}
	r := windowRect(hwnd)
	width := int(r.Right - r.Left)
	height := int(r.Bottom - r.Top)
	if width <= 0 || height <= 0 || width > 8000 || height > 8000 {
		return "", fmt.Errorf("窗口尺寸异常")
	}
	hdc, _, _ := procGetDC.Call(0)
	if hdc == 0 {
		return "", fmt.Errorf("无法获取设备上下文")
	}
	defer procReleaseDC.Call(0, hdc)
	memDC, _, _ := procCreateCompatibleDC.Call(hdc)
	defer procDeleteDC.Call(memDC)
	bitmap, _, _ := procCreateCompatibleBitmap.Call(hdc, uintptr(width), uintptr(height))
	defer procDeleteObject.Call(bitmap)
	procSelectObject.Call(memDC, bitmap)
	procPrintWindow.Call(hwnd, memDC, 2) // PW_RENDERFULLCONTENT

	type bmiHeader struct {
		Size          uint32
		Width, Height int32
		Planes, BitCount uint16
		Compression   uint32
		SizeImage     uint32
		XPPM, YPPM    int32
		Used, Important uint32
	}
	bmi := struct {
		Header bmiHeader
		Colors [1]uint32
	}{}
	bmi.Header.Size = 40
	bmi.Header.Width = int32(width)
	bmi.Header.Height = int32(-height) // top-down
	bmi.Header.Planes = 1
	bmi.Header.BitCount = 32
	pixels := make([]byte, width*height*4)
	procGetDIBits.Call(memDC, bitmap, 0, uintptr(height), uintptr(unsafe.Pointer(&pixels[0])), uintptr(unsafe.Pointer(&bmi)), 0)

	img := image.NewRGBA(image.Rect(0, 0, width, height))
	// Windows DIB rows are BGRA; image.RGBA wants RGBA.
	for y := 0; y < height; y++ {
		for x := 0; x < width; x++ {
			i := (y*width + x) * 4
			img.SetRGBA(x, y, color.RGBA{R: pixels[i+2], G: pixels[i+1], B: pixels[i], A: pixels[i+3]})
		}
	}
	var buf bytes.Buffer
	if err := png.Encode(&buf, img); err != nil {
		return "", err
	}
	return base64.StdEncoding.EncodeToString(buf.Bytes()), nil
}
