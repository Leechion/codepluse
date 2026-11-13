//go:build windows

// codepluse uses the Codeg web UI inside a native Windows WebView2 window.
package main

import (
	"encoding/json"
	"flag"
	"fmt"
	"net/http"
	"net/url"
	"os"
	"os/exec"
	"path/filepath"
	"strconv"
	"strings"
	"syscall"
	"time"
	"unsafe"

	webview2 "github.com/jchv/go-webview2"
)

func main() {
	// Let Windows render the webview at the display's native DPI instead of
	// bitmap-scaling a DPI-unaware window, which blurs text at 125% and above.
	syscall.NewLazyDLL("user32.dll").NewProc("SetProcessDPIAware").Call()
	exe, _ := os.Executable()
	root := flag.String("root", "", "workspace installation directory")
	port := flag.Int("port", 8840, "local Go backend port")
	page := flag.String("page", "/workspace", "local application page")
	flag.Parse()
	if *root == "" {
		*root = filepath.Dir(exe)
		// Builds live in dist/codepluse; resources point back to this checkout.
		var config struct {
			Root string `json:"root"`
		}
		if b, err := os.ReadFile(filepath.Join(*root, "workspace.json")); err == nil && json.Unmarshal(b, &config) == nil && config.Root != "" {
			*root = config.Root
		}
	}
	base := "http://127.0.0.1:" + strconv.Itoa(*port)
	if err := ensureBackend(*root, base, *port); err != nil {
		fail(err)
		return
	}
	data := filepath.Join(*root, ".data", "webview2")
	w := webview2.NewWithOptions(webview2.WebViewOptions{AutoFocus: true, DataPath: data, WindowOptions: webview2.WindowOptions{Title: "codepluse", Width: 1440, Height: 940, Center: true}})
	if w == nil {
		fail(fmt.Errorf("无法创建 WebView2 窗口，请安装 Microsoft Edge WebView2 Runtime"))
		return
	}
	defer w.Destroy()
	w.SetSize(900, 640, webview2.HintMin)
	hwnd := uintptr(w.Window())
	removeNativeCaption(hwnd)
	_ = w.Bind("__codegGoOpenWindow", func(path string) error {
		if !strings.HasPrefix(path, "/") || strings.HasPrefix(path, "//") || strings.ContainsAny(path, "\\\r\n") {
			return fmt.Errorf("只能打开本地应用页面")
		}
		program := filepath.Join(filepath.Dir(exe), "codepluse.exe")
		cmd := exec.Command(program, "--root", *root, "--port", strconv.Itoa(*port), "--page", path)
		cmd.Dir = *root
		if err := cmd.Start(); err != nil {
			return err
		}
		go func() { _ = cmd.Wait() }()
		return nil
	})
	_ = w.Bind("__codegGoCloseWindow", func() { w.Terminate() })
	_ = w.Bind("__codegGoOpenExternal", func(raw string) error {
		u, err := url.Parse(raw)
		if err != nil || u.Scheme != "https" || u.Host == "" || u.User != nil || strings.ContainsAny(raw, "\r\n") {
			return fmt.Errorf("只能打开有效的 HTTPS 登录页面")
		}
		cmd := exec.Command("rundll32.exe", "url.dll,FileProtocolHandler", u.String())
		cmd.SysProcAttr = &syscall.SysProcAttr{HideWindow: true}
		if err := cmd.Start(); err != nil {
			return err
		}
		go func() { _ = cmd.Wait() }()
		return nil
	})
	_ = w.Bind("__codegGoWindowMinimize", func() { showWindow(hwnd, 6) })
	_ = w.Bind("__codegGoWindowToggleMaximize", func() bool {
		if isWindowMaximized(hwnd) {
			showWindow(hwnd, 9)
		} else {
			showWindow(hwnd, 3)
		}
		return isWindowMaximized(hwnd)
	})
	_ = w.Bind("__codegGoWindowBeginDrag", func() {
		user32 := syscall.NewLazyDLL("user32.dll")
		user32.NewProc("ReleaseCapture").Call()
		user32.NewProc("SendMessageW").Call(hwnd, 0x00A1, 2, 0) // WM_NCLBUTTONDOWN, HTCAPTION
	})
	if !strings.HasPrefix(*page, "/") || strings.HasPrefix(*page, "//") || strings.ContainsAny(*page, "\\\r\n") {
		fail(fmt.Errorf("应用页面路径无效"))
		return
	}
	w.Navigate(base + *page)
	w.Run()
}

func removeNativeCaption(hwnd uintptr) {
	user32 := syscall.NewLazyDLL("user32.dll")
	getWindowLong := user32.NewProc("GetWindowLongW")
	setWindowLong := user32.NewProc("SetWindowLongW")
	style, _, _ := getWindowLong.Call(hwnd, uintptr(uint32(0xFFFFFFF0))) // GWL_STYLE
	style &^= 0x00C00000                                                 // WS_CAPTION; keep the resize frame and system menu.
	setWindowLong.Call(hwnd, uintptr(uint32(0xFFFFFFF0)), style)
	user32.NewProc("SetWindowPos").Call(hwnd, 0, 0, 0, 0, 0, 0x0027) // frame changed
}

func showWindow(hwnd uintptr, command uintptr) {
	syscall.NewLazyDLL("user32.dll").NewProc("ShowWindow").Call(hwnd, command)
}

func isWindowMaximized(hwnd uintptr) bool {
	maximized, _, _ := syscall.NewLazyDLL("user32.dll").NewProc("IsZoomed").Call(hwnd)
	return maximized != 0
}

func ensureBackend(root, base string, port int) error {
	client := &http.Client{Timeout: 750 * time.Millisecond}
	probe := func() (bool, error) {
		r, err := client.Get(base + "/api/health")
		if err != nil {
			return false, nil
		}
		defer r.Body.Close()
		var body struct {
			Backend string `json:"backend"`
			Version string `json:"codeg_version"`
		}
		if json.NewDecoder(r.Body).Decode(&body) != nil || body.Backend != "go" || body.Version == "" {
			return false, fmt.Errorf("端口 %d 已由其他服务占用", port)
		}
		return true, nil
	}
	if ok, err := probe(); ok || err != nil {
		return err
	}
	if _, err := os.Stat(filepath.Join(root, "codeg-ui", "out", "workspace.html")); err != nil {
		return fmt.Errorf("界面尚未构建，请在项目中执行 npm run codeg:build：%w", err)
	}
	if err := os.MkdirAll(filepath.Join(root, ".data"), 0755); err != nil {
		return err
	}
	log, err := os.OpenFile(filepath.Join(root, ".data", "codeg-desktop-backend.log"), os.O_CREATE|os.O_APPEND|os.O_WRONLY, 0600)
	if err != nil {
		return err
	}
	defer log.Close()
	backend := filepath.Join(root, "server-go", "serve-codepluse.exe")
	if _, err := os.Stat(backend); os.IsNotExist(err) {
		backend = filepath.Join(root, "server-go", "serve-codeg.exe")
	}
	cmd := exec.Command(backend)
	cmd.Dir = root
	env := append(os.Environ(), "HARNESS_HUB_CODEG=1", "PORT="+strconv.Itoa(port), "HOST=127.0.0.1")
	// 默认关闭连接预热：预热会在启动时并行拉起多个 agent 适配器进程，内存紧张的机器
	// 上会触发 "fatal error: out of memory" 崩溃。外部已显式设置 HARNESS_HUB_WARM 时尊重其值。
	if _, ok := os.LookupEnv("HARNESS_HUB_WARM"); !ok {
		env = append(env, "HARNESS_HUB_WARM=0")
	}
	// 桌面版是本机单用户、后端只绑 127.0.0.1，默认放开目录浏览，「打开文件夹」
	// 才能选到服务根之外的整机路径（与 scripts/start-codeg.ps1 的行为对齐）。
	// 外部已显式设置 HARNESS_HUB_ALLOW_ANY_PATH 时尊重其值（设 0 可收回边界）。
	// 注意这只放开只读的目录列举；文件读写仍要求目标位于服务根或已添加的工作空间内。
	if _, ok := os.LookupEnv("HARNESS_HUB_ALLOW_ANY_PATH"); !ok {
		env = append(env, "HARNESS_HUB_ALLOW_ANY_PATH=1")
	}
	cmd.Env = env
	cmd.Stdout, cmd.Stderr = log, log
	cmd.SysProcAttr = &syscall.SysProcAttr{HideWindow: true, CreationFlags: 0x08000000}
	if err := cmd.Start(); err != nil {
		return err
	}
	go func() { _ = cmd.Wait() }()
	for deadline := time.Now().Add(30 * time.Second); time.Now().Before(deadline); {
		if ok, err := probe(); ok || err != nil {
			return err
		}
		time.Sleep(150 * time.Millisecond)
	}
	return fmt.Errorf("Go 服务未在 30 秒内就绪，请查看 .data/codeg-desktop-backend.log")
}

func fail(err error) {
	text, _ := syscall.UTF16PtrFromString(err.Error())
	title, _ := syscall.UTF16PtrFromString("codepluse 启动失败")
	syscall.NewLazyDLL("user32.dll").NewProc("MessageBoxW").Call(0, uintptr(unsafe.Pointer(text)), uintptr(unsafe.Pointer(title)), 0x10)
}
