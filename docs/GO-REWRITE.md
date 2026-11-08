# Go 全量重写服务端（server-go）

> 状态：可运行、测试通过。Node 版（server.js + core/*.js）保留作为行为对拍基线。

## 为什么换 Go

用户要求把服务端从 Node 换成 Go 架构，全量重写。原则：

- **零第三方依赖**：只用标准库（net/http、encoding/json、os/exec），与 Node 版「3 个 npm 依赖」的极简哲学一致。
- **单二进制多角色**：主服务 + `mcp-result` + `mcp-files` 两个 MCP 子命令，编译产物 `serve.exe` 约 11MB，无运行时依赖。
- **前端零改动**：`public/` 原样复用，REST/SSE 接口形状与 Node 版对齐。

## 目录结构

```
server-go/
├── go.mod                      # module harnesshub, go 1.27, 无依赖
├── serve.exe                   # 构建产物
├── cmd/serve/                  # 入口
│   ├── main.go                 # 路由（REST 全量）+ 响应封装
│   ├── state.go                # 配置 / server 结构 / 静态 / SSE
│   ├── wiring.go               # 装配：组件创建、持久化、automations tick、main()
│   └── cli.go                  # CLI 定位（codex/claude）、CODEX_HOME 隔离、网关配置
└── internal/
    ├── hub/       hub.go       # 引擎：agents/tasks/bus/hash-chain audit/logs/events
    ├── acp/       runtime.go   # ACP v1 运行时：drivers、sessions、权限卡、可暂停时钟、MCP 注入
    ├── orch/      orchestrator.go  # 宰相编排：plan/波次/消息/steer/fork/验收
    │              chat.go          # 对话控制：决策路由、@-mention、mission 折叠
    │              automations.go   # cron 定时任务（5 字段自研解析）
    ├── extra/     worktree.go  # git worktree 隔离/diff/merge/discard
    │              skills.go        # 技能包
    │              notifier.go      # webhook 通知（一次静默重试）
    │              handoff.go       # 会话移交（简报 + 新原生会话）
    └── mcpserver/ result.go    # submit_result 结构化结果 sink（schema 校验 + MAX_REFUSED=5）
                   files.go     # 只读工作区文件服务
```

## 构建与运行

```bash
cd server-go
I:/tools/go/bin/go.exe build -o serve.exe ./cmd/serve   # 或系统 go
# 仓库根目录运行（静态目录、node_modules 探测均以 cwd 为 ROOT）
HARNESS_HUB_STATE_FILE=:memory: PORT=8790 ./serve.exe
# 子命令
./serve.exe mcp-result '<schema JSON>'
./serve.exe mcp-files <workspace>
```

环境变量与 Node 版同名兼容：PORT/HOST/HARNESS_HUB_REAL/HARNESS_HUB_WORKDIR/HARNESS_HUB_ALLOWED_ROOT/HARNESS_HUB_TIMEOUT/HARNESS_HUB_CONCURRENCY/HARNESS_HUB_TOOL/HARNESS_HUB_STATE_FILE/HARNESS_HUB_WEBHOOK_URL/HARNESS_HUB_CODEX_BIN 等。

## 测试

```bash
I:/tools/go/bin/go.exe test ./...    # cron / JSON 提取 / schema 校验 / 审计链 / 消息密级
```

对拍验证（fixture ACP 代理 + 真实 HTTP 冒烟）已覆盖：会话建立/回合/更新流、权限卡拒绝路径、submit_result 校验与放行、@-委派 mission 全链路、worktree diff/merge/discard、automations 校验、SSE、持久化。

## 与 Node 版的行为差异

| 项 | Node 版 | Go 版 |
|---|---|---|
| HTTP 桥接执行端（http-adapter） | 支持 | 暂不支持（注册时提示走 ACP） |
| 模拟执行端（SimulatedAdapter）/ 演示层 context/grants 写入 | 支持 | 只读兼容（返回空集合/占位响应） |
| 全局并发队列（invocationQueue + per-agent 计数） | 有 | 简化为 per-agent 串行 + 无全局队列 |
| ACP 通知处理 | 单线程事件循环 | 按输入顺序处理通知，再交付 RPC 响应；权限等请求异步处理（turnMu 串行回合 / activeMu 流式更新） |
| `-race` | N/A | 需 CGO，本机 portable Go 未启用 |

## 移植过程中修复的关键问题

1. **通知乱序与回合死锁**：通知不获取整轮的 turnMu，仅使用 activeMu，按线路顺序完成更新后才交付 RPC 响应；等待人工授权的请求保持异步，避免阻塞读循环。
2. **Snapshot 重入死锁**：Snapshot 持锁调用 Agents() 再次加锁（Go Mutex 不可重入）。
3. **d.call 零超时**：session/prompt 需要无超时调用（可暂停时钟由调用方控制）。
4. **空集合 JSON null**：Go nil 切片序列化为 null，前端期望 [] → 全部初始化/兜底。
