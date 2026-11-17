# codepluse 代码库全面解析与 less is more 优化报告

日期：2026-11-15 · 范围：`E:\code\codepluse`（Rust 后端 + `codeg-ui` Next.js 前端 + Tauri 桌面壳）
口径：只写有实测证据的结论；每个优化项附验证结果。

---

## 1. 体量与结构总览

| 区域 | 文件数 | 源码体积 | 说明 |
|---|---|---|---|
| `src-tauri/src` | 74 个 `.rs` | ≈ 2,277 KB | Rust 后端（原 Go sidecar 命令已全部归并至此） |
| `codeg-ui/src/components` | 910 | ≈ 10,885 KB | 前端最大区，UI 组件主体 |
| `codeg-ui/src/lib` | 480 | ≈ 4,130 KB | api.ts 防腐层、types.ts 契约、工具库 |
| `codeg-ui/src/contexts + stores + hooks` | 166 | ≈ 2,814 KB | 状态层 |
| `codeg-ui/src/app + i18n + types` | 70 | ≈ 189 KB | 路由（40 页静态导出）+ 10 语言文案 |

磁盘大头（非源码）：`src-tauri/target` ≈ 19 GB、`codeg-ui`（node_modules + out + .next）≈ 2.2 GB、`node_modules` ≈ 871 MB。

**架构一句话**：Tauri 壳内嵌 axum 服务（127.0.0.1:8840）+ WebView2 加载静态导出页；前端 `api.ts` 统一发 `POST /api/<command>`，后端 `EXT_MODULES` 表驱动派发（`owns`/`handle`），阻塞 I/O 一律 `spawn_blocking`，ACP 运行时独占异步层。134 条原 Go 命令已全部有 Rust 所有者（审计 `calls without a Rust command owner: 0`，197 条新测试）。

历史包袱已清：旧 `server.js`/`core/`/`public/` 链路与 Go sidecar 均已移出仓库，不再存在误改旧链路的风险。

---

## 2. 本轮落地的优化（全部已验证）

### 2.1 修复办公工具 tab 整页崩溃（后端契约 bug，最高优先级）

- **症状**：设置 → 技能包 → 办公工具 tab 渲染 `This page couldn't load`，CDP 捕获 `TypeError: j.map is not a function`，且后续「界面/自定义」tab 连带不可达。
- **根因**：`src-tauri/src/backend/office.rs` 的 `officecli_list_skills` 返回信封对象 `{"skills":[...]}`，而前端契约（`api.ts` → `OfficecliSkill[]`）是**裸数组**——组件直接 `skills.map(...)`。`officecli_sync_skills` 也返回同一形状，前端却按 `SkillSyncReport {synced, errors}` 读 `report.errors.length`；`officecli_skill_read_content` 返回对象而前端把返回值当字符串直接渲染。
- **修复**（`office.rs`，前端零改动）：
  - `list_skills` → 裸数组，且补齐 `OfficecliSkill` 声明的全部字段（`category` 按文件名推导、`icon`、`sortOrder`、`installedCentrally`=是否随 `codeg-assets` 分发、`displayName/description` 本地化映射）；
  - `sync_skills` → 真实同步逻辑（把 `codeg-assets` 中央技能拷入 `.data/office-skills`，**不覆盖已有本地副本**）+ `{synced, errors}` 报告；
  - `skill_read_content` → 裸字符串。
- **验证**：`cargo test --release backend::office` 14/14 通过（新增 `sync_skills_reports_counts_and_leaves_existing_copies_alone`，改写 2 个旧断言）；部署后真机 CDP 逐个点击 5 个子 tab（专家/科学研究/办公工具/界面/自定义）零异常，办公工具与自定义显示诚实空态（0 列，本机未分发办公技能）。

### 2.2 五处重复谓词收敛为 `isProviderAgent`（代码冗余）

- 技能包 5 个矩阵页面（自定义/界面/专家/办公工具/科学研究）各自复制同一行过滤 `agent.skills_capable && !piUsesCustomAgentDir(agent)`，且都漏排用户手建的 `inst_*` 实例——后端实测 9 条 agent **全部** `skills_capable=True`（含 3 条实例），实例各占一列（锁定、计数 0）。
- 新增共享判定 `src/lib/custom-agents.ts::isProviderAgent(agent)`：同时查 `instance` 标志位**和** `inst_` 前缀（列表漏发 `instance` 字段也不能让实例冒充供应商）；保留 `custom:` ACP（真供应商）。
- 五处统一为 `agent.skills_capable && isProviderAgent(agent) && !piUsesCustomAgentDir(agent)`。
- **验证**：`tsc --noEmit` 0 错误；真机矩阵表头 7 个 th = 角列 + 6 供应商列（Codex / Claude Code / WorkBuddy 国内版 / WorkBuddy 国际版 / Qoder / Qoder CN），`inst_*` 三列消失。

### 2.3 连接目录预热线程（吞吐）

- **实测**：`codepluse_connection_catalog` 同进程首调 ≈ 4.9 s，第二次 ≈ 83 ms。定位过程分两步，值得留档：
  1. 先猜「冷读机器状态（CC Switch SQLite / provider JSON / 原生配置）」→ 预热 `provider_catalog::public` → **无效**，真机首调仍 5040 ms；
  2. 顺 `connection_catalog` 组装路径逐段读代码，真凶在 `account_channels::list(root)`：每行 `account:*` 渠道都要 `ensure(root)` —— **首次调用要启动账号渠道协议进程**再取 `/status`；第二次快是因为进程已起。这与「登录状态与模型列表来自协议进程」的注释互相印证。
- **修复**：`backend/mod.rs::serve()` 启动即拉起 `connection-catalog-warmup` 线程，跑一遍**真实**的 `codepluse_connection_catalog` 组装（provider 目录 + 账号渠道进程 + 设置读取全路径），结果丢弃；只读，与真实请求并发安全。
- **验证**：部署后常驻实例（无调试端口）启动 12 s 后首调 `codepluse_connection_catalog` = **109 ms**（修复前同口径 5040 ms）；`cargo build --release` 通过。设置页打开时 catalog 已是热的，主智能体「未配置」误报的根因（4.9 s 阻塞 `refresh()` 的 `Promise.all`）在服务端就此消除。

### 2.4 删除纯转发 shim（文件冗余）

- `components/settings/harness-instance-settings.tsx` 全文一行：`export { HarnessProfiles as HarnessInstanceSection }`。唯一引用方 `delegation-settings.tsx` 改为直接 `import { HarnessProfiles } from "./harness-profiles"`，shim 文件删除。

### 2.5 陈旧测试对齐现 UI（测试噪声）

- `delegation-settings.test.tsx` 7 条失败断言仍在测**旧的 4-tab 布局**（`/Agent defaults/`、`/Assistants/` 两个已消失的页签、`Add model configuration` 按钮已并入主智能体卡片弹层）；按现 3-tab 布局（常规 / 主智能体配置 / 子智能体配置，子智能体列表内嵌于第三 tab）改写。
- **验证**：18/18 通过。至此设置区 harness 三件套测试（`harness-profiles.test.tsx`、`harness-profiles.harness-sync.test.tsx`、`delegation-settings.test.tsx`）全部转绿。

---

## 3. 内存与吞吐专项

| 项 | 状态 | 证据/建议 |
|---|---|---|
| catalog 首调 4.9 s | ✅ 已修（2.3） | 真凶是 `account_channels::ensure` 冷启动协议进程，非磁盘冷读；启动期预热真实 catalog 路径后首调 109 ms |
| 头像/积分 403/404 | 能力边界 | 上游账号接口限制，非本仓库缺陷 |
| headless 模式 panic | 既有未修 | `CODEPLUSE_HEADLESS=1` 在 `serve()` 返回后析构 tokio runtime 时 panic（"Cannot drop a runtime…"），进程 4 s 退出；修复需动启动路径，建议单独立项 |
| ConPTY 测试失败 | 环境性 | `conpty_session_round_trip`（CreatePseudoConsole 0x80070006）在无交互控制台环境必失败，与代码无关 |
| 构建产物体积 | 建议 | `src-tauri/target` 19 GB：发版前 `cargo clean` 或用共享缓存；CI 上建议 sccache |
| 前端 Promise.all 阻塞模式 | 局部已修 | `harness-profiles.refresh()` 是样本；其余多调用面板如再出现「未配置」误报，照 loading 态 + 预热两板斧处理 |

---

## 4. 冗余与债务清单（未动手，按价值排序）

1. **Rust warning 债务**：release 构建报 86 条 warning（41 条可 `cargo fix` 自动消除）。建议一次性清理后开 `-D warnings` 门禁，防新增。
2. **根目录散落日志**：`.tauri-build*.log`（6 个）、`.serve.log`、`.analysis-scratch/` 均非交付物，建议入 `.gitignore` 并清理。
3. **测试超时 flake**：`custom-skills-settings.import-filter.test.tsx` 单跑 4.9 s，默认 5s 超时临界。建议 `vitest.config` 全局 `testTimeout: 20000`，或瘦身 setup（本次跑批实测 setup 22 s / import 76 s）。
4. **i18n 维护成本**：10 个 locale 各持几百键，改名必须脚本批改（沿用 `find_keys.py`/`json_roundtrip.py`/`rename_labels.py` 一套 + `python json.load` 严格校验），禁止手工双引号插值。
5. **`next.config.ts` `ignoreBuildErrors: true`**：构建不做类型检查，`tsc --noEmit` 必须留在验证链里（已作为惯例执行），长期建议关掉该豁免。

---

## 5. 本轮验证矩阵

| 验证 | 结果 |
|---|---|
| `codeg-ui` `pnpm exec tsc --noEmit` | exit 0 |
| 触及的 11 个测试文件（技能矩阵 5 + harness 3 + delegation 1 + matrix/packs 2），`--testTimeout=20000` | 全绿（58 tests；delegation 单独复跑 18/18） |
| `cargo check` / `cargo build --release` | 干净（仅既有 warning） |
| `cargo test --release --bin codepluse`（全量） | 见下「最终门禁」 |
| 常驻实例（无调试端口）启动 12 s 后 catalog 首调 | 109 ms（修复前 5040 ms） |
| 真机 CDP（WebView2 9223，验收期临时开启）：技能包 5 子 tab 逐个点击 | 0 异常；专家/科学研究/界面 6 供应商列；办公工具/自定义空态正常 |

### 最终门禁

- `codeg-ui`：`tsc --noEmit` exit 0；`pnpm build` exit 0（40 页静态导出）。
- 后端：`cargo build --release` exit 0；全量 `cargo test --release --bin codepluse`：497 条中唯一失败为既有环境性 ConPTY（`CreatePseudoConsole() 0x80070006`，`terminal.rs`，本轮零改动、与代码无关），其余全绿。
- 常驻实例已以**不带** `--remote-debugging-port` 方式重启，`/api/health` 200。

## 6. 变更清单（均未 git commit）

- `src-tauri/src/backend/office.rs`（契约修复 + 真实同步 + 测试改写）
- `src-tauri/src/backend/mod.rs`（catalog 预热线程）
- `codeg-ui/src/lib/custom-agents.ts`（`isProviderAgent`）
- `codeg-ui/src/components/settings/{custom-skills,generative-ui,experts,office-tools,science}-settings.tsx`（过滤统一）
- `codeg-ui/src/components/settings/delegation-settings.tsx`（直连 HarnessProfiles）
- 删除 `codeg-ui/src/components/settings/harness-instance-settings.tsx`
- `codeg-ui/src/components/settings/delegation-settings.test.tsx`（断言对齐现 UI）
- 此前各轮：`assistant-editor-dialog.tsx`、`harness-profiles.tsx`、10 个 i18n JSON、`acp/runtime.rs`、`acp/connections.rs` 等（详见各自验收记录）

**一句话总结**：这一轮把「只显示供应商」的诉求从子智能体下拉推广到了全部 5 个技能矩阵，顺手修掉了办公工具 tab 的整页崩溃（后端返回形状与前端契约不符的真 bug）、给 4.9 s 的供应商目录加了启动预热、删掉了一个转发文件和 7 条陈旧断言——每一项都有真机或测试证据；更大的既有债务（headless panic、warning 清理、target 19 GB）已列清单待批。
