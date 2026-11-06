Codeg frontend and adapted backend contracts originate from spacering-net/codeg (local snapshot 0.42.0), licensed under Apache-2.0. The original license is preserved in codeg-ui/LICENSE. Go backend implementations are maintained in server-go.

The Windows desktop window uses github.com/jchv/go-webview2 (MIT), github.com/jchv/go-winloader (ISC), and golang.org/x/sys (BSD-3-Clause). Event transport uses github.com/gorilla/websocket (BSD-2-Clause). Versions and checksums are pinned in server-go/go.mod and go.sum. Original dependency license files are included in dist/codepluse/licenses by the desktop build script.

Windows interactive terminals use github.com/UserExistsError/conpty v0.1.4 (MIT), providing access to the operating system's real ConPTY implementation. Its license is included by the same desktop build script.
