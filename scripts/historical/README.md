# 迁移期工具（已归档）

这里的脚本是 **Go → Rust 后端迁移期间**用来做「双胞胎双跑」的工具：
它们同时启动 Go sidecar 与 Rust 前门，对同一请求序列逐项比对响应与落盘文件。
迁移完成后 sidecar 已删除（Phase 4），因此这些脚本**不再随日常构建运行**。

| 脚本 | 作用 | 依赖 |
|---|---|---|
| `parity.mjs` | 单命令对拍（同一文件两侧写、id 会漂移的场景） | Go sidecar |
| `parity-def.mjs` | D/E/F 组（worktask / 浏览器 / 桌面控制）106 步 | Go sidecar |
| `parity-final.mjs` | 收尾批 28 条命令 31 步 | Go sidecar |
| `parity-git.mjs` | git 组 64 步（双胞胎仓库） | Go sidecar |
| `parity-ghi.mjs` | G/H/I 组（终端 / 定时任务 / 杂项）38 步 | Go sidecar |
| `parity-mcp.mjs` | `mcp-codeg` 伴随服务的 JSON-RPC 帧对拍 | Go sidecar |
| `parity-net.mjs` | 联网组对拍 | Go sidecar |
| `parity-provider.mjs` | 外部 provider 写回（CC Switch / Codex / DSH / 模型源） | Go sidecar |
| `parity-store.mjs` | Store 读写 89 步（孪生 workspace） | Go sidecar |
| `parity-web.mjs` | Phase 3 服务面（静态资源 / health / 上传 / 防护）14 项 | Go sidecar |
| `inventory-commands.mjs` | 扫描 Go 源码重建命令清单 TSV | Go 源码 |

**需要时如何再跑**：Go sidecar 的最终状态留存在 tag `go-sidecar-final`
（`git checkout go-sidecar-final -- server-go`），构建出 `server-go/serve-codeg.exe`
与 `src-tauri/target/debug/codepluse.exe` 后，按各脚本头部的说明执行即可。

命令面清单 `docs/rust-backend-command-inventory.tsv` 是本仓库的**权威记录**，
不再自动重建（Go 源码已删除）。
