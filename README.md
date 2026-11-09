# codepluse · 多 Harness 工作台

codepluse 使用 Codeg 0.42.0 的 Next.js 界面、Go ACP 后台和 Windows WebView2 桌面窗口。它支持真实 ACP 会话、执行端委派、文件和 Git 操作、终端与画布。

## 安装与启动

需要 Node.js 22+、pnpm 11、Go 1.27 和 Windows WebView2 Runtime。执行端需要单独安装并配置模型服务。

```powershell
npm ci
pnpm --dir codeg-ui install --frozen-lockfile
npm run codepluse:build
npm start
```

打开 http://127.0.0.1:8840/workspace。构建 Windows 桌面版使用 `npm run codepluse:desktop:build`。安装、功能边界及数据位置见 [移植说明](docs/CODEG-GO-PORT.md)。

## 代码结构

- `codeg-ui/`：Codeg 前端。
- `server-go/`：Go 服务、ACP 运行时及 WebView2 桌面程序。
- `fixtures/`：Go 协议测试使用的 ACP fixture。
- `desktop/` 和 `public/`：保留的旧界面及 Electron 桌面壳，使用 Go 后台。
- `test/desktop.test.js`：保留的 Electron 桌面壳测试；其他旧 Node 服务测试不再适用。

验证：在 `server-go/` 执行 `go test ./...` 和 `go vet ./...`；根目录的 `npm test` 仅测试保留的 Electron 桌面壳。旧 Node 服务源码已移除，历史设计文档仅供参考。
