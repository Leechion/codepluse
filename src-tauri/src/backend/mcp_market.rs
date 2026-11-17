//! MCP 市场与本地服务 —— 补全批次。
//!
//! 前端（`codeg-ui/src/lib/api.ts`）已经在调用这 10 条命令，但 Rust 与已删除的
//! Go sidecar 都没有实现过它们（在 `server-go` 只读副本里搜不到命令字面量），
//! 所以契约只由调用点（`api.ts` 的参数名）与 `types.ts` 的返回结构决定：
//!
//! * 本地清单 [`LocalMcpServer`] = `{ id, spec, apps }`；扫描结果
//!   [`LocalMcpScan`] = `{ servers, warnings }`，单个来源读不动只记一条 warning，
//!   不让整次扫描失败（issue #632 的语义）。
//! * 市场侧 DTO：`McpMarketplaceProvider` / `McpMarketplaceItem` /
//!   `McpMarketplaceServerDetail` / `McpMarketplaceInstallOption`，字段名照
//!   `types.ts` 抄，缺省值补 `null` / 空数组。
//!
//! 落地分三档：
//!
//! * **本地侧（L1）**：持久清单在 `root/.data/mcp-servers.json`，结构
//!   `{ "servers": [LocalMcpServer], "hidden": [扫描条目标识] }`。`mcp_scan_local`
//!   在持久清单之外真实扫描两个仓库里既有 home 约定的来源：Claude Code 的
//!   `~/.claude.json`（`mcpServers`）与 Codex CLI 的 `~/.codex/config.toml`
//!   （`[mcp_servers.*]`）。其余智能体的 MCP 配置格式本工作台尚未读取，先不展开，
//!   避免编造来源；扫描条目标识固定为 `scan:<app>:<name>`。
//! * **市场侧（L2）**：真实 HTTP，8 秒短超时；上游地址由环境变量
//!   `CODEPLUSE_MCP_MARKETPLACE_BASE` 覆盖（测试桩走这个 seam）。本仓库没有内置
//!   上游目录，也没有可移植的 Go 契约；未配置地址或请求失败时返回中文 `Err`，
//!   不编造市场条目。
//! * **伴生服务（L3）**：`/api/codeg_mcp` 协作端点随主进程内联绑定，没有独立的
//!   broker 进程需要拉起，`start_codeg_mcp_service` 幂等空转返回。
//!
//! 注意：本模块与 `/api/codeg_mcp` 协议端点（`mcp_companion.rs`）不是一回事，
//! 后者只读参考，本文件不改动它。

use std::path::{Path, PathBuf};
use std::time::Duration;

use serde_json::{json, Value};
use url::Url;

use crate::backend::net;
use crate::backend::store_file;

/// 本地持久清单文件（相对 `root/.data/`）。
const SERVERS_FILE: &str = "mcp-servers.json";
/// 工具组开关持久化文件（相对 `root/.data/`）。
const GROUPS_FILE: &str = "mcp-tool-groups.json";
/// 市场上游地址覆盖；未设置时使用内置默认地址（见 [`marketplace_base`]）。
const MARKETPLACE_ENV: &str = "CODEPLUSE_MCP_MARKETPLACE_BASE";
/// 本仓库未内置市场上游；留空表示“地址未配置”，由部署方通过环境变量提供。
const DEFAULT_MARKETPLACE_BASE: &str = "";
/// 市场请求短超时，避免面板长时间卡在加载态。
const MARKETPLACE_TIMEOUT: Duration = Duration::from_secs(8);
const DEFAULT_SEARCH_LIMIT: u64 = 30;
const MAX_SEARCH_LIMIT: u64 = 100;

/// 与 `types.ts` 的 `McpAppType` 联合类型一一对应。
const ALL_MCP_APPS: [&str; 20] = [
    "claude_code",
    "codex",
    "gemini",
    "open_claw",
    "open_code",
    "cline",
    "hermes",
    "code_buddy",
    "kimi_code",
    "minimax_code",
    "grok",
    "cursor",
    "deepseek",
    "qoder",
    "antigravity",
    "kiro",
    "zcode",
    "trae",
    "command_code",
    "pi",
];

/// 与前端 `agent-tool-groups.ts` 的 `AGENT_TOOL_GROUPS` 键一一对应。
const TOOL_GROUPS: [&str; 9] = [
    "delegation",
    "feedback",
    "ask",
    "sessions",
    "browser",
    "browser_eval",
    "computer",
    "automations",
    "taskboard",
];

pub fn owns(command: &str) -> bool {
    matches!(
        command,
        "mcp_get_marketplace_server_detail"
            | "mcp_install_from_marketplace"
            | "mcp_list_marketplaces"
            | "mcp_remove_server"
            | "mcp_scan_local"
            | "mcp_search_marketplace"
            | "mcp_set_server_apps"
            | "mcp_upsert_local_server"
            | "set_codeg_mcp_tool_group"
            | "start_codeg_mcp_service"
    )
}

pub fn handle(root: &Path, command: &str, args: &Value) -> Option<Result<Value, String>> {
    if !owns(command) {
        return None;
    }
    Some(run(root, command, args))
}

fn run(root: &Path, command: &str, args: &Value) -> Result<Value, String> {
    match command {
        "mcp_scan_local" => scan_local(root),
        "mcp_list_marketplaces" => list_marketplaces(),
        "mcp_search_marketplace" => search_marketplace(args),
        "mcp_get_marketplace_server_detail" => get_marketplace_detail(args),
        "mcp_install_from_marketplace" => install_from_marketplace(root, args),
        "mcp_upsert_local_server" => upsert_local_server(root, args),
        "mcp_set_server_apps" => set_server_apps(root, args),
        "mcp_remove_server" => remove_server(root, args),
        "set_codeg_mcp_tool_group" => set_tool_group(root, args),
        "start_codeg_mcp_service" => start_service(),
        _ => Err(format!("未知的 MCP 命令：{command}")),
    }
}

// ── 路径与持久清单 ──────────────────────────────────────────────────────

fn data_dir(root: &Path) -> PathBuf {
    root.join(".data")
}

fn servers_path(root: &Path) -> PathBuf {
    data_dir(root).join(SERVERS_FILE)
}

fn groups_path(root: &Path) -> PathBuf {
    data_dir(root).join(GROUPS_FILE)
}

/// 读取本地清单；文件缺失或格式不对时按空清单处理，不把旧文件损坏暴露成扫描失败。
fn load_store(root: &Path) -> Value {
    store_file::read_json(&servers_path(root))
        .ok()
        .flatten()
        .filter(Value::is_object)
        .unwrap_or_else(|| json!({}))
}

fn save_store(root: &Path, doc: &Value) -> Result<(), String> {
    let _lock = store_file::lock(&servers_path(root))?;
    store_file::write_json(&servers_path(root), doc)
}

// ── 参数校验 ────────────────────────────────────────────────────────────

fn required_str(args: &Value, field: &str) -> Result<String, String> {
    args.get(field)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .ok_or_else(|| format!("缺少 {field} 参数"))
}

/// 服务器标识：只做安全字符校验，挡掉路径分隔符与 `..`。
fn validate_id(raw: Option<&Value>, field: &str) -> Result<String, String> {
    let id = raw
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| format!("缺少 {field} 参数"))?;
    if id.contains('/') || id.contains('\\') || id.contains("..") || id.contains(':') {
        return Err(format!("{field} 包含非法字符"));
    }
    Ok(id.to_string())
}

/// 校验 `apps`：必须是数组，每项都是已知的 `McpAppType`，去重保序。
fn normalize_apps(raw: Option<&Value>) -> Result<Vec<String>, String> {
    let arr = raw
        .and_then(Value::as_array)
        .ok_or_else(|| "apps 必须是数组".to_string())?;
    let mut apps: Vec<String> = Vec::new();
    for item in arr {
        let app = item
            .as_str()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .ok_or_else(|| "apps 包含无效的智能体标识".to_string())?
            .to_string();
        if !ALL_MCP_APPS.contains(&app.as_str()) {
            return Err(format!("未知的智能体类型：{app}"));
        }
        if !apps.iter().any(|existing| existing == &app) {
            apps.push(app);
        }
    }
    Ok(apps)
}

/// 归一化 MCP 类型：`streamablehttp` 这类写法归到 `http`。
fn normalize_type(raw: Option<&str>) -> String {
    let value = raw.unwrap_or("stdio").trim().to_ascii_lowercase();
    match value.as_str() {
        "" | "stdio" => "stdio".to_string(),
        "streamablehttp" => "http".to_string(),
        other => other.to_string(),
    }
}

/// 校验并回显 spec：对象；stdio 必须有 command，http/sse/local/remote 必须有 url。
fn validate_spec(spec: &Value) -> Result<Value, String> {
    let spec = spec.as_object().ok_or_else(|| "spec 必须是对象".to_string())?;
    let typ = normalize_type(spec.get("type").and_then(Value::as_str));
    match typ.as_str() {
        "stdio" => {
            let command = spec
                .get("command")
                .and_then(Value::as_str)
                .unwrap_or("")
                .trim();
            if command.is_empty() {
                return Err("stdio 类型的 MCP 必须填写 command".into());
            }
        }
        "http" | "sse" | "local" | "remote" => {
            let url = spec.get("url").and_then(Value::as_str).unwrap_or("").trim();
            if url.is_empty() {
                return Err(format!("{typ} 类型的 MCP 必须填写 url"));
            }
        }
        other => return Err(format!("未知的 MCP 类型：{other}")),
    }
    Ok(Value::Object(spec.clone()))
}

fn server_row(id: &str, spec: Value, apps: Vec<String>) -> Value {
    json!({ "id": id, "spec": spec, "apps": apps })
}

// ── 本地 CRUD ───────────────────────────────────────────────────────────

fn upsert_local_server(root: &Path, args: &Value) -> Result<Value, String> {
    let server_id = validate_id(args.get("serverId"), "serverId")?;
    let spec = args.get("spec").ok_or_else(|| "缺少 spec 参数".to_string())?;
    let spec = validate_spec(spec)?;
    let apps = normalize_apps(args.get("apps"))?;
    // `shownSpec` 是上游产品留给“按服务器保留私有设置”的线索；本工作台没有这类
    // 按服务器的私有设置，因此不参与合并，原样忽略。

    let mut doc = load_store(root);
    let obj = doc
        .as_object_mut()
        .ok_or_else(|| "本地 MCP 清单格式无效".to_string())?;
    let arr = obj
        .entry("servers")
        .or_insert_with(|| json!([]))
        .as_array_mut()
        .ok_or_else(|| "本地 MCP 清单格式无效".to_string())?;
    let row = server_row(&server_id, spec, apps);
    match arr
        .iter_mut()
        .find(|existing| existing.get("id").and_then(Value::as_str) == Some(server_id.as_str()))
    {
        Some(existing) => *existing = row.clone(),
        None => arr.push(row.clone()),
    }
    save_store(root, &doc)?;
    Ok(row)
}

fn set_server_apps(root: &Path, args: &Value) -> Result<Value, String> {
    let server_id = validate_id(args.get("serverId"), "serverId")?;
    let apps = normalize_apps(args.get("apps"))?;

    let mut doc = load_store(root);
    let record = match doc
        .get_mut("servers")
        .and_then(Value::as_array_mut)
        .and_then(|arr| {
            arr.iter_mut()
                .find(|row| row.get("id").and_then(Value::as_str) == Some(server_id.as_str()))
        }) {
        Some(existing) => {
            existing["apps"] = json!(apps);
            existing.clone()
        }
        None => return Ok(Value::Null),
    };
    save_store(root, &doc)?;
    Ok(record)
}

fn remove_server(root: &Path, args: &Value) -> Result<Value, String> {
    let server_id = validate_id(args.get("serverId"), "serverId")?;
    // `apps` 是“只从部分智能体摘除”的可选入参；本工作台的持久清单按服务器整体
    // 记录分配关系，摘除即整服务器移除，apps 入参保留兼容但不拆分。

    let mut doc = load_store(root);
    if let Some(arr) = doc.get_mut("servers").and_then(Value::as_array_mut) {
        arr.retain(|row| row.get("id").and_then(Value::as_str) != Some(server_id.as_str()));
    }
    // 扫描来的外部条目不在持久清单里：记入 hidden，下次扫描不再冒出来；重复移除幂等。
    let Ok(obj) = doc.as_object_mut().ok_or_else(|| "本地 MCP 清单格式无效".to_string()) else {
        return Err("本地 MCP 清单格式无效".into());
    };
    let hidden = obj.entry("hidden").or_insert_with(|| json!([]));
    if let Some(hidden) = hidden.as_array_mut() {
        if !hidden.iter().any(|item| item.as_str() == Some(server_id.as_str())) {
            hidden.push(json!(server_id));
        }
    }
    save_store(root, &doc)?;
    Ok(json!(true))
}

// ── 本机扫描 ───────────────────────────────────────────────────────────

/// 测试 seam 与 `providers_core::home` 一致：`HARNESS_HUB_FAKE_HOME` 指向临时目录。
fn home_dir() -> PathBuf {
    if let Ok(fake) = std::env::var("HARNESS_HUB_FAKE_HOME") {
        if !fake.is_empty() {
            return PathBuf::from(fake);
        }
    }
    std::env::var("USERPROFILE")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("/"))
}

fn scan_local(root: &Path) -> Result<Value, String> {
    let home = home_dir();
    scan_local_with_home(root, &home)
}

fn scan_local_with_home(root: &Path, home: &Path) -> Result<Value, String> {
    let doc = load_store(root);
    let persisted: Vec<Value> = doc
        .get("servers")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let hidden: Vec<String> = doc
        .get("hidden")
        .and_then(Value::as_array)
        .map(|rows| {
            rows.iter()
                .filter_map(|item| item.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default();

    let mut warnings: Vec<Value> = Vec::new();
    let mut scanned: Vec<Value> = Vec::new();
    scan_claude(home, &mut scanned, &mut warnings);
    scan_codex(home, &mut scanned, &mut warnings);

    // 合并：持久清单优先；hidden 里的扫描条目不返回；标识去重。
    let mut servers = persisted;
    for entry in scanned {
        let id = entry
            .get("id")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        if hidden.iter().any(|hidden_id| hidden_id == &id) {
            continue;
        }
        if servers
            .iter()
            .any(|row| row.get("id").and_then(Value::as_str) == Some(id.as_str()))
        {
            continue;
        }
        servers.push(entry);
    }
    Ok(json!({ "servers": servers, "warnings": warnings }))
}

/// 把一个外部条目归一成本工作台的 spec 形状。
fn external_spec(
    command: Option<&str>,
    args: Option<&Value>,
    env: Option<&Value>,
    url: Option<&str>,
    typ: Option<&str>,
) -> Value {
    let command = command.unwrap_or("").trim().to_string();
    let url = url.unwrap_or("").trim().to_string();
    if !url.is_empty() && command.is_empty() {
        let protocol = match normalize_type(typ).as_str() {
            "sse" => "sse",
            _ => "http",
        };
        return json!({ "type": protocol, "url": url });
    }
    let mut spec = json!({ "type": "stdio", "command": command });
    if let Some(args) = args.and_then(Value::as_array) {
        spec["args"] = json!(args);
    }
    if let Some(env) = env.and_then(Value::as_object) {
        spec["env"] = json!(env);
    }
    if !url.is_empty() {
        spec["url"] = json!(url);
    }
    spec
}

fn scan_claude(home: &Path, out: &mut Vec<Value>, warnings: &mut Vec<Value>) {
    let file = home.join(".claude.json");
    let bytes = match std::fs::read(&file) {
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return,
        Err(err) => {
            warnings.push(json!({ "app": "claude_code", "message": format!("读取 Claude Code 配置失败：{err}") }));
            return;
        }
        Ok(bytes) => bytes,
    };
    let payload: Value = match serde_json::from_slice(&bytes) {
        Ok(value) => value,
        Err(err) => {
            warnings.push(json!({ "app": "claude_code", "message": format!("Claude Code 配置 JSON 无效：{err}") }));
            return;
        }
    };
    let Some(servers) = payload.get("mcpServers").and_then(Value::as_object) else {
        return;
    };
    for (name, entry) in servers {
        let command = entry.get("command").and_then(Value::as_str);
        let url = entry.get("url").and_then(Value::as_str);
        if command.unwrap_or("").trim().is_empty() && url.unwrap_or("").trim().is_empty() {
            continue;
        }
        let spec = external_spec(
            command,
            entry.get("args"),
            entry.get("env"),
            url,
            entry.get("type").and_then(Value::as_str),
        );
        out.push(json!({
            "id": format!("scan:claude_code:{name}"),
            "spec": spec,
            "apps": ["claude_code"],
        }));
    }
}

fn scan_codex(home: &Path, out: &mut Vec<Value>, warnings: &mut Vec<Value>) {
    let file = home.join(".codex").join("config.toml");
    let text = match std::fs::read_to_string(&file) {
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return,
        Err(err) => {
            warnings.push(json!({ "app": "codex", "message": format!("读取 Codex 配置失败：{err}") }));
            return;
        }
        Ok(text) => text,
    };
    let cfg: toml::Value = match toml::from_str(&text) {
        Ok(value) => value,
        Err(err) => {
            warnings.push(json!({ "app": "codex", "message": format!("Codex 配置格式无效：{err}") }));
            return;
        }
    };
    let Some(servers) = cfg.get("mcp_servers").and_then(toml::Value::as_table) else {
        return;
    };
    for (name, entry) in servers {
        let command = entry.get("command").and_then(toml::Value::as_str);
        let url = entry.get("url").and_then(toml::Value::as_str);
        if command.unwrap_or("").trim().is_empty() && url.unwrap_or("").trim().is_empty() {
            continue;
        }
        let args = entry
            .get("args")
            .and_then(toml::Value::as_array)
            .map(|rows| {
                Value::Array(
                    rows.iter()
                        .filter_map(|item| item.as_str().map(str::to_string).map(Value::String))
                        .collect(),
                )
            });
        let env = entry.get("env").and_then(toml::Value::as_table).map(|table| {
            Value::Object(
                table
                    .iter()
                    .map(|(key, value)| {
                        (key.clone(), Value::String(value.as_str().unwrap_or_default().to_string()))
                    })
                    .collect(),
            )
        });
        let spec = external_spec(command, args.as_ref(), env.as_ref(), url, None);
        out.push(json!({
            "id": format!("scan:codex:{name}"),
            "spec": spec,
            "apps": ["codex"],
        }));
    }
}

// ── 市场侧（L2 HTTP）────────────────────────────────────────────────────

fn marketplace_base() -> Result<String, String> {
    let base = std::env::var(MARKETPLACE_ENV)
        .ok()
        .filter(|value| !value.trim().is_empty())
        .or_else(|| {
            (!DEFAULT_MARKETPLACE_BASE.is_empty()).then(|| DEFAULT_MARKETPLACE_BASE.to_string())
        });
    match base {
        Some(base) => Ok(base.trim_end_matches('/').to_string()),
        None => Err("MCP 市场上游地址未配置（设置 CODEPLUSE_MCP_MARKETPLACE_BASE 后重试）".into()),
    }
}

fn http_get_json(url: &str) -> Result<Value, String> {
    let response = net::no_proxy_client()
        .get(url)
        .timeout(MARKETPLACE_TIMEOUT)
        .send()
        .map_err(|err| format!("连接 MCP 市场失败：{err}"))?;
    let status = response.status().as_u16();
    if status != 200 {
        return Err(format!("MCP 市场返回错误：HTTP {status}"));
    }
    let body = response.bytes().map_err(|err| format!("读取 MCP 市场响应失败：{err}"))?;
    serde_json::from_slice::<Value>(&body).map_err(|err| format!("MCP 市场响应 JSON 无效：{err}"))
}

fn join_url(base: &str, path: &str, query: &[(&str, &str)]) -> Result<String, String> {
    let mut url = Url::parse(&format!("{base}{path}")).map_err(|err| format!("市场地址无效：{err}"))?;
    {
        let mut pairs = url.query_pairs_mut();
        for (key, value) in query {
            pairs.append_pair(key, value);
        }
    }
    Ok(url.to_string())
}

fn list_marketplaces() -> Result<Value, String> {
    let base = marketplace_base()?;
    list_marketplaces_with_base(&base)
}

fn list_marketplaces_with_base(base: &str) -> Result<Value, String> {
    let payload = http_get_json(&join_url(base, "/providers", &[])?)?;
    let providers = payload
        .get("providers")
        .and_then(Value::as_array)
        .or_else(|| payload.as_array())
        .ok_or_else(|| "MCP 市场缺少 providers 列表".to_string())?;
    let rows: Vec<Value> = providers
        .iter()
        .map(|provider| {
            json!({
                "id": provider.get("id").cloned().unwrap_or(Value::Null),
                "name": provider.get("name").cloned().unwrap_or(Value::Null),
                "description": provider.get("description").cloned().unwrap_or(Value::Null),
            })
        })
        .collect();
    Ok(Value::Array(rows))
}

fn normalize_item(entry: &Value) -> Value {
    let text_or_null = |key: &str| entry.get(key).cloned().unwrap_or(Value::Null);
    json!({
        "provider_id": entry.get("provider_id").cloned().unwrap_or(Value::Null),
        "server_id": entry.get("server_id").cloned().unwrap_or(Value::Null),
        "name": entry.get("name").cloned().unwrap_or(Value::Null),
        "description": entry.get("description").cloned().unwrap_or(Value::Null),
        "homepage": text_or_null("homepage"),
        "remote": entry.get("remote").cloned().unwrap_or(Value::Bool(false)),
        "verified": entry.get("verified").cloned().unwrap_or(Value::Bool(false)),
        "icon_url": text_or_null("icon_url"),
        "latest_version": text_or_null("latest_version"),
        "protocols": entry.get("protocols").cloned().unwrap_or(json!([])),
        "owner": text_or_null("owner"),
        "namespace": text_or_null("namespace"),
        "downloads": entry.get("downloads").cloned().unwrap_or(Value::Null),
        "score": entry.get("score").cloned().unwrap_or(Value::Null),
        "is_deployed": entry.get("is_deployed").cloned().unwrap_or(Value::Null),
    })
}

fn search_marketplace(args: &Value) -> Result<Value, String> {
    // 纯参数校验先于上游地址解析：缺参时报参数错，而不是报“地址未配置”。
    let provider_id = required_str(args, "providerId")?;
    let base = marketplace_base()?;
    let _ = provider_id;
    search_with_base(&base, args)
}

fn search_with_base(base: &str, args: &Value) -> Result<Value, String> {
    let provider_id = required_str(args, "providerId")?;
    let query = args
        .get("query")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string);
    let limit = args
        .get("limit")
        .and_then(Value::as_u64)
        .unwrap_or(DEFAULT_SEARCH_LIMIT)
        .clamp(1, MAX_SEARCH_LIMIT);
    let limit_str = limit.to_string();
    let mut query_pairs = vec![("provider_id", provider_id.as_str()), ("limit", limit_str.as_str())];
    let query_slot: String;
    if let Some(needle) = &query {
        query_slot = needle.clone();
        query_pairs.push(("query", query_slot.as_str()));
    }
    let url = join_url(base, "/servers", &query_pairs)?;
    let payload = http_get_json(&url)?;
    let items = payload
        .get("servers")
        .and_then(Value::as_array)
        .or_else(|| payload.as_array())
        .ok_or_else(|| "MCP 市场缺少 servers 列表".to_string())?;
    let mut rows: Vec<Value> = items.iter().map(normalize_item).collect();
    // 上游不支持关键词过滤时，本地兜底按名称/描述过滤一遍。
    if let Some(needle) = &query {
        let needle = needle.to_ascii_lowercase();
        rows.retain(|row| {
            let haystack = format!(
                "{} {}",
                row.get("name").and_then(Value::as_str).unwrap_or(""),
                row.get("description").and_then(Value::as_str).unwrap_or(""),
            )
            .to_ascii_lowercase();
            haystack.contains(&needle)
        });
    }
    Ok(Value::Array(rows))
}

fn normalize_parameter(entry: &Value) -> Value {
    json!({
        "key": entry.get("key").cloned().unwrap_or(Value::Null),
        "label": entry.get("label").cloned().unwrap_or(Value::Null),
        "description": entry.get("description").cloned().unwrap_or(Value::Null),
        "required": entry.get("required").cloned().unwrap_or(Value::Bool(false)),
        "secret": entry.get("secret").cloned().unwrap_or(Value::Bool(false)),
        "kind": entry.get("kind").cloned().unwrap_or(Value::Null),
        "default_value": entry.get("default_value").cloned().unwrap_or(Value::Null),
        "placeholder": entry.get("placeholder").cloned().unwrap_or(Value::Null),
        "enum_values": entry.get("enum_values").cloned().unwrap_or(json!([])),
        "location": entry.get("location").cloned().unwrap_or(Value::Null),
    })
}

fn normalize_option(entry: &Value) -> Value {
    let parameters: Vec<Value> = entry
        .get("parameters")
        .and_then(Value::as_array)
        .map(|rows| rows.iter().map(normalize_parameter).collect())
        .unwrap_or_default();
    json!({
        "id": entry.get("id").cloned().unwrap_or(Value::Null),
        "protocol": entry.get("protocol").cloned().unwrap_or(Value::Null),
        "label": entry.get("label").cloned().unwrap_or(Value::Null),
        "description": entry.get("description").cloned().unwrap_or(Value::Null),
        "spec": entry.get("spec").cloned().unwrap_or(json!({})),
        "parameters": parameters,
    })
}

fn normalize_detail(entry: &Value) -> Value {
    let options: Vec<Value> = entry
        .get("install_options")
        .and_then(Value::as_array)
        .map(|rows| rows.iter().map(normalize_option).collect())
        .unwrap_or_default();
    json!({
        "provider_id": entry.get("provider_id").cloned().unwrap_or(Value::Null),
        "server_id": entry.get("server_id").cloned().unwrap_or(Value::Null),
        "name": entry.get("name").cloned().unwrap_or(Value::Null),
        "description": entry.get("description").cloned().unwrap_or(Value::Null),
        "homepage": entry.get("homepage").cloned().unwrap_or(Value::Null),
        "remote": entry.get("remote").cloned().unwrap_or(Value::Bool(false)),
        "verified": entry.get("verified").cloned().unwrap_or(Value::Bool(false)),
        "icon_url": entry.get("icon_url").cloned().unwrap_or(Value::Null),
        "latest_version": entry.get("latest_version").cloned().unwrap_or(Value::Null),
        "protocols": entry.get("protocols").cloned().unwrap_or(json!([])),
        "owner": entry.get("owner").cloned().unwrap_or(Value::Null),
        "namespace": entry.get("namespace").cloned().unwrap_or(Value::Null),
        "downloads": entry.get("downloads").cloned().unwrap_or(Value::Null),
        "score": entry.get("score").cloned().unwrap_or(Value::Null),
        "is_deployed": entry.get("is_deployed").cloned().unwrap_or(Value::Null),
        "default_option_id": entry.get("default_option_id").cloned().unwrap_or(Value::Null),
        "install_options": options,
        "spec": entry.get("spec").cloned().unwrap_or(json!({})),
    })
}

fn fetch_detail(base: &str, provider_id: &str, server_id: &str) -> Result<Value, String> {
    let url = join_url(
        base,
        &format!("/servers/{server_id}"),
        &[("provider_id", provider_id)],
    )?;
    let payload = http_get_json(&url)?;
    Ok(normalize_detail(&payload))
}

fn get_marketplace_detail(args: &Value) -> Result<Value, String> {
    let provider_id = required_str(args, "providerId")?;
    let server_id = required_str(args, "serverId")?;
    let base = marketplace_base()?;
    fetch_detail(&base, &provider_id, &server_id)
}

/// 安装前的纯参数校验，不依赖上游可达性。
fn prepare_install(args: &Value) -> Result<(String, String, Vec<String>), String> {
    let provider_id = required_str(args, "providerId")?;
    let server_id = required_str(args, "serverId")?;
    let apps = normalize_apps(args.get("apps"))?;
    if apps.is_empty() {
        return Err("至少选择一个智能体".into());
    }
    Ok((provider_id, server_id, apps))
}

fn install_from_marketplace(root: &Path, args: &Value) -> Result<Value, String> {
    // 纯参数校验先于上游地址解析：缺参/空 apps 不依赖上游可达性。
    let (provider_id, server_id, apps) = prepare_install(args)?;
    let base = marketplace_base()?;
    install_validated(&base, root, &provider_id, &server_id, apps, args)
}

#[cfg(test)]
fn install_with_base(
    base: &str,
    root: &Path,
    args: &Value,
) -> Result<Value, String> {
    let (provider_id, server_id, apps) = prepare_install(args)?;
    install_validated(base, root, &provider_id, &server_id, apps, args)
}

fn install_validated(
    base: &str,
    root: &Path,
    provider_id: &str,
    server_id: &str,
    apps: Vec<String>,
    args: &Value,
) -> Result<Value, String> {
    // 基线 spec 必须来自上游详情；上游不可达时不允许伪装安装成功。
    let detail = fetch_detail(base, provider_id, server_id)?;

    let option_id = args
        .get("optionId")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty());
    let option = match option_id {
        Some(oid) => detail
            .get("install_options")
            .and_then(Value::as_array)
            .and_then(|rows| {
                rows.iter()
                    .find(|option| option.get("id").and_then(Value::as_str) == Some(oid))
                    .cloned()
            }),
        None => detail
            .get("install_options")
            .and_then(Value::as_array)
            .and_then(|rows| rows.first().cloned()),
    };
    let mut spec = option
        .as_ref()
        .and_then(|item| item.get("spec"))
        .cloned()
        .unwrap_or_else(|| detail.get("spec").cloned().unwrap_or(json!({})));

    // 填写的安装参数并入 spec.env；`specOverride`（编辑器里改过的整段 spec）整体覆盖。
    if let Some(values) = args.get("parameterValues").and_then(Value::as_object) {
        let env = spec
            .as_object_mut()
            .ok_or_else(|| "安装参数必须并入对象型 spec".to_string())?
            .entry("env")
            .or_insert_with(|| json!({}));
        if let Some(env) = env.as_object_mut() {
            for (key, value) in values {
                env.insert(key.clone(), value.clone());
            }
        }
    }
    if let Some(override_spec) = args.get("specOverride").filter(|value| !value.is_null()) {
        if override_spec.is_object() {
            spec = override_spec.clone();
        }
    }
    let spec = validate_spec(&spec)?;

    let mut doc = load_store(root);
    let obj = doc
        .as_object_mut()
        .ok_or_else(|| "本地 MCP 清单格式无效".to_string())?;
    let arr = obj
        .entry("servers")
        .or_insert_with(|| json!([]))
        .as_array_mut()
        .ok_or_else(|| "本地 MCP 清单格式无效".to_string())?;
    let row = server_row(server_id, spec, apps);
    match arr
        .iter_mut()
        .find(|existing| existing.get("id").and_then(Value::as_str) == Some(server_id))
    {
        Some(existing) => *existing = row.clone(),
        None => arr.push(row.clone()),
    }
    save_store(root, &doc)?;
    Ok(row)
}

// ── 工具组开关 / 伴生服务 ────────────────────────────────────────────────

fn set_tool_group(root: &Path, args: &Value) -> Result<Value, String> {
    let key = args
        .get("key")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .ok_or_else(|| "缺少 key 参数".to_string())?;
    if !TOOL_GROUPS.contains(&key.as_str()) {
        return Err(format!("非法的工具组：{key}"));
    }
    let enabled = args
        .get("enabled")
        .and_then(Value::as_bool)
        .ok_or_else(|| "enabled 必须是布尔值".to_string())?;

    let path = groups_path(root);
    let _lock = store_file::lock(&path)?;
    let mut doc = store_file::read_json(&path)?
        .filter(Value::is_object)
        .unwrap_or_else(|| json!({}));
    let groups = doc
        .as_object_mut()
        .ok_or_else(|| "工具组设置格式无效".to_string())?
        .entry("groups")
        .or_insert_with(|| json!({}));
    groups
        .as_object_mut()
        .ok_or_else(|| "工具组设置格式无效".to_string())?
        .insert(key.clone(), json!(enabled));
    store_file::write_json(&path, &doc)?;
    crate::backend::emit_event(
        "mcp-tool-groups://changed",
        json!({ "key": key, "enabled": enabled }),
    );
    Ok(Value::Null)
}

/// 本工作台的 `/api/codeg_mcp` 协作端点随主进程内联绑定，没有独立 broker 进程
/// 要拉起；前端语义是“未应答就绑定，幂等”，这里等价于确认端点已在本进程监听。
fn start_service() -> Result<Value, String> {
    Ok(Value::Null)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn temp_root(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "codepluse-mcp-market-{tag}-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn call(root: &Path, command: &str, args: Value) -> Result<Value, String> {
        handle(root, command, &args).unwrap()
    }

    /// 起一个本地 HTTP 桩：按路径前缀返回预置 JSON，避免测试走真实外网。
    fn serve_stub(routes: &'static [(&'static str, &'static str)]) -> String {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap().to_string();
        std::thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                let mut stream = stream;
                let mut buffer = [0u8; 4096];
                let _ = stream.read(&mut buffer);
                let request = String::from_utf8_lossy(&buffer);
                let path = request.split_whitespace().nth(1).unwrap_or("").to_string();
                let body = routes
                    .iter()
                    .find(|(route, _)| path.starts_with(*route))
                    .map(|(_, payload)| *payload)
                    .unwrap_or(r#"{"servers":[],"providers":[]}"#);
                let response = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    body.len(),
                    body
                );
                let _ = stream.write_all(response.as_bytes());
            }
        });
        format!("http://{address}")
    }

    #[test]
    fn owns_covers_the_ten_documented_commands_only() {
        for command in [
            "mcp_get_marketplace_server_detail",
            "mcp_install_from_marketplace",
            "mcp_list_marketplaces",
            "mcp_remove_server",
            "mcp_scan_local",
            "mcp_search_marketplace",
            "mcp_set_server_apps",
            "mcp_upsert_local_server",
            "set_codeg_mcp_tool_group",
            "start_codeg_mcp_service",
        ] {
            assert!(owns(command), "{command} should be owned");
        }
        assert!(!owns("get_codeg_mcp_service_status"));
        assert!(!owns("mcp_companion_dispatch"));
        assert!(!owns("MCP_UPSERT_LOCAL_SERVER"));
    }

    #[test]
    fn scan_local_empty_workspace_returns_empty_lists() {
        let root = temp_root("scan-empty");
        let scan = call(&root, "mcp_scan_local", json!({})).unwrap();
        assert_eq!(scan["servers"].as_array().unwrap().len(), 0);
        assert_eq!(scan["warnings"].as_array().unwrap().len(), 0);
    }

    #[test]
    fn scan_local_reads_claude_and_codex_configs_and_warns_on_bad_json() {
        let root = temp_root("scan-sources");
        let home = temp_root("scan-home");
        fs::create_dir_all(home.join(".codex")).unwrap();
        fs::write(
            home.join(".claude.json"),
            r#"{"mcpServers":{"chrome":{"command":"npx","args":["-y","chrome-devtools-mcp@latest"],"env":{"FOO":"bar"}},"remote":{"url":"https://example.com/mcp"}}}"#,
        )
        .unwrap();
        fs::write(
            home.join(".codex").join("config.toml"),
            "[mcp_servers.github]\ncommand = \"gh\"\nargs = [\"mcp\", \"serve\"]\n",
        )
        .unwrap();

        let scan = scan_local_with_home(&root, &home).unwrap();
        let servers = scan["servers"].as_array().unwrap();
        assert_eq!(servers.len(), 3, "claude 两条 + codex 一条");
        let chrome = servers
            .iter()
            .find(|row| row["id"] == "scan:claude_code:chrome")
            .unwrap();
        assert_eq!(chrome["spec"]["command"], "npx");
        assert_eq!(chrome["spec"]["args"][1], "chrome-devtools-mcp@latest");
        assert_eq!(chrome["apps"], json!(["claude_code"]));
        let remote = servers
            .iter()
            .find(|row| row["id"] == "scan:claude_code:remote")
            .unwrap();
        assert_eq!(remote["spec"]["type"], "http");
        assert_eq!(remote["spec"]["url"], "https://example.com/mcp");
        let codex = servers
            .iter()
            .find(|row| row["id"] == "scan:codex:github")
            .unwrap();
        assert_eq!(codex["spec"]["command"], "gh");
        assert_eq!(codex["apps"], json!(["codex"]));

        // 坏配置文件进入 warnings，而不是让整次扫描失败。
        fs::write(home.join(".claude.json"), "{not json").unwrap();
        let broken = scan_local_with_home(&root, &home).unwrap();
        assert_eq!(broken["warnings"].as_array().unwrap().len(), 1);
        assert_eq!(broken["warnings"][0]["app"], "claude_code");
    }

    #[test]
    fn upsert_persists_and_echoes_contract_shape() {
        let root = temp_root("upsert");
        let saved = call(
            &root,
            "mcp_upsert_local_server",
            json!({
                "serverId": "chrome-devtools",
                "spec": { "type": "stdio", "command": "npx", "args": ["-y", "chrome-devtools-mcp@latest"] },
                "apps": ["claude_code", "codex"]
            }),
        )
        .unwrap();
        assert_eq!(saved["id"], "chrome-devtools");
        assert_eq!(saved["spec"]["command"], "npx");
        assert_eq!(saved["apps"], json!(["claude_code", "codex"]));

        // 落盘后重新扫描，持久清单里能读回同一条。
        let scan = call(&root, "mcp_scan_local", json!({})).unwrap();
        assert_eq!(scan["servers"].as_array().unwrap().len(), 1);
        assert_eq!(scan["servers"][0]["id"], "chrome-devtools");
        assert!(servers_path(&root).is_file());
    }

    #[test]
    fn upsert_rejects_missing_or_invalid_inputs() {
        let root = temp_root("upsert-bad");
        assert!(call(&root, "mcp_upsert_local_server", json!({}))
            .unwrap_err()
            .contains("缺少 serverId"));
        assert!(call(
            &root,
            "mcp_upsert_local_server",
            json!({"serverId": "x", "spec": "not-an-object", "apps": []})
        )
        .unwrap_err()
        .contains("spec 必须是对象"));
        assert!(call(
            &root,
            "mcp_upsert_local_server",
            json!({"serverId": "x", "spec": { "type": "stdio" }, "apps": []})
        )
        .unwrap_err()
        .contains("command"));
        assert!(call(
            &root,
            "mcp_upsert_local_server",
            json!({"serverId": "x", "spec": { "type": "http", "url": "" }, "apps": []})
        )
        .unwrap_err()
        .contains("url"));
        assert!(call(
            &root,
            "mcp_upsert_local_server",
            json!({"serverId": "x", "spec": { "type": "stdio", "command": "a" }, "apps": ["nope_app"]})
        )
        .unwrap_err()
        .contains("未知的智能体类型"));
        assert!(call(
            &root,
            "mcp_upsert_local_server",
            json!({"serverId": "../escape", "spec": { "type": "stdio", "command": "a" }, "apps": []})
        )
        .unwrap_err()
        .contains("非法字符"));
    }

    #[test]
    fn set_server_apps_updates_record_and_returns_null_for_unknown_id() {
        let root = temp_root("set-apps");
        call(
            &root,
            "mcp_upsert_local_server",
            json!({"serverId": "s1", "spec": { "type": "stdio", "command": "a" }, "apps": ["claude_code"]}),
        )
        .unwrap();
        let updated = call(
            &root,
            "mcp_set_server_apps",
            json!({ "serverId": "s1", "apps": ["codex", "gemini"] }),
        )
        .unwrap();
        assert_eq!(updated["id"], "s1");
        assert_eq!(updated["apps"], json!(["codex", "gemini"]));

        let missing = call(
            &root,
            "mcp_set_server_apps",
            json!({ "serverId": "nope", "apps": ["codex"] }),
        )
        .unwrap();
        assert!(missing.is_null());

        assert!(call(
            &root,
            "mcp_set_server_apps",
            json!({ "serverId": "s1", "apps": "not-an-array" })
        )
        .unwrap_err()
        .contains("apps 必须是数组"));
    }

    #[test]
    fn remove_server_is_idempotent_and_hides_scanned_entries() {
        let root = temp_root("remove");
        call(
            &root,
            "mcp_upsert_local_server",
            json!({"serverId": "s1", "spec": { "type": "stdio", "command": "a" }, "apps": ["claude_code"]}),
        )
        .unwrap();
        let first = call(&root, "mcp_remove_server", json!({ "serverId": "s1" })).unwrap();
        assert_eq!(first, json!(true));
        // 已删除再删一次仍然成功（幂等）。
        let second = call(&root, "mcp_remove_server", json!({ "serverId": "s1" })).unwrap();
        assert_eq!(second, json!(true));
        let scan = call(&root, "mcp_scan_local", json!({})).unwrap();
        assert_eq!(scan["servers"].as_array().unwrap().len(), 0);

        assert!(call(&root, "mcp_remove_server", json!({}))
            .unwrap_err()
            .contains("缺少 serverId"));
    }

    #[test]
    fn list_marketplaces_reads_providers_from_local_stub() {
        let stub = serve_stub(&[(
            "/providers",
            r#"{"providers":[{"id":"official_registry","name":"Official Registry","description":"官方目录"}]}"#,
        )]);
        let rows = list_marketplaces_with_base(&stub).unwrap();
        let rows = rows.as_array().unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0]["id"], "official_registry");
        assert_eq!(rows[0]["name"], "Official Registry");
        assert_eq!(rows[0]["description"], "官方目录");
    }

    #[test]
    fn search_marketplace_filters_and_maps_item_shape() {
        let stub = serve_stub(&[(
            "/servers",
            r#"{"servers":[
                {"provider_id":"official_registry","server_id":"chrome-devtools","name":"Chrome DevTools","description":"浏览器调试","homepage":"https://x","remote":false,"verified":true,"icon_url":null,"latest_version":"1.2.3","protocols":["stdio"],"owner":"owner","namespace":"ns","downloads":100,"score":4.5,"is_deployed":null},
                {"provider_id":"official_registry","server_id":"other","name":"Other","description":"zzz","remote":true,"verified":false,"protocols":[]}
            ]}"#,
        )]);
        let rows = search_with_base(
            &stub,
            &json!({ "providerId": "official_registry", "query": "chrome", "limit": 10 }),
        )
        .unwrap();
        let rows = rows.as_array().unwrap();
        assert_eq!(rows.len(), 1);
        let item = &rows[0];
        assert_eq!(item["server_id"], "chrome-devtools");
        assert_eq!(item["latest_version"], "1.2.3");
        assert_eq!(item["protocols"], json!(["stdio"]));
        assert_eq!(item["downloads"], 100);
        assert_eq!(item["is_deployed"], Value::Null);
    }

    #[test]
    fn search_marketplace_rejects_missing_provider_id() {
        assert!(search_with_base("http://127.0.0.1:1", &json!({}))
            .unwrap_err()
            .contains("缺少 providerId"));
    }

    #[test]
    fn detail_reads_install_options_from_local_stub() {
        let stub = serve_stub(&[(
            "/servers/chrome-devtools",
            r#"{"provider_id":"official_registry","server_id":"chrome-devtools","name":"Chrome DevTools","description":"d","homepage":null,"remote":false,"verified":true,"icon_url":null,"latest_version":"1.2.3","protocols":["stdio"],"owner":null,"namespace":null,"downloads":null,"score":null,"is_deployed":null,"default_option_id":"opt-npx","install_options":[{"id":"opt-npx","protocol":"stdio","label":"npx","description":null,"spec":{"type":"stdio","command":"npx","args":["-y","chrome-devtools-mcp@latest"]},"parameters":[{"key":"TOKEN","label":"Token","description":null,"required":true,"secret":true,"kind":"string","default_value":null,"placeholder":null,"enum_values":[],"location":"env"}]}],"spec":{"type":"stdio","command":"npx"}}"#,
        )]);
        let detail = fetch_detail(&stub, "official_registry", "chrome-devtools").unwrap();
        assert_eq!(detail["server_id"], "chrome-devtools");
        assert_eq!(detail["default_option_id"], "opt-npx");
        assert_eq!(detail["install_options"].as_array().unwrap().len(), 1);
        let option = &detail["install_options"][0];
        assert_eq!(option["parameters"][0]["key"], "TOKEN");
        assert_eq!(option["parameters"][0]["secret"], true);
    }

    #[test]
    fn install_persists_detail_spec_with_parameter_values_and_override() {
        let stub = serve_stub(&[(
            "/servers/ext-srv",
            r#"{"provider_id":"official_registry","server_id":"ext-srv","name":"Ext","description":"d","install_options":[{"id":"opt-1","protocol":"stdio","label":"local","spec":{"type":"stdio","command":"npx","args":["-y","ext-srv"]},"parameters":[]}],"spec":{"type":"stdio","command":"npx"}}"#,
        )]);
        let root = temp_root("install");
        let row = install_with_base(
            &stub,
            &root,
            &json!({
                "providerId": "official_registry",
                "serverId": "ext-srv",
                "apps": ["claude_code"],
                "optionId": "opt-1",
                "parameterValues": { "TOKEN": "secret-123" },
                "specOverride": null
            }),
        )
        .unwrap();
        assert_eq!(row["id"], "ext-srv");
        assert_eq!(row["apps"], json!(["claude_code"]));
        assert_eq!(row["spec"]["env"]["TOKEN"], "secret-123");

        let scan = call(&root, "mcp_scan_local", json!({})).unwrap();
        assert_eq!(scan["servers"].as_array().unwrap().len(), 1);

        // 缺 apps 拒绝；缺 providerId/serverId 拒绝。
        assert!(install_with_base(
            &stub,
            &root,
            &json!({ "providerId": "p", "serverId": "ext-srv", "apps": [] })
        )
        .unwrap_err()
        .contains("至少选择一个智能体"));
        assert!(install_with_base(
            &stub,
            &root,
            &json!({ "serverId": "ext-srv", "apps": ["claude_code"] })
        )
        .unwrap_err()
        .contains("缺少 providerId"));
    }

    #[test]
    fn install_rejects_an_empty_app_list() {
        let root = temp_root("install-apps");
        assert!(call(
            &root,
            "mcp_install_from_marketplace",
            json!({ "providerId": "p", "serverId": "s", "apps": [] })
        )
        .unwrap_err()
        .contains("至少选择一个智能体"));
    }

    #[test]
    fn tool_group_persists_toggle_and_rejects_unknown_key() {
        let root = temp_root("tool-group");
        call(
            &root,
            "set_codeg_mcp_tool_group",
            json!({ "key": "delegation", "enabled": true }),
        )
        .unwrap();
        let doc = store_file::read_json(&groups_path(&root))
            .unwrap()
            .unwrap();
        assert_eq!(doc["groups"]["delegation"], true);

        assert!(call(
            &root,
            "set_codeg_mcp_tool_group",
            json!({ "key": "not_a_group", "enabled": true })
        )
        .unwrap_err()
        .contains("非法的工具组"));
        assert!(call(
            &root,
            "set_codeg_mcp_tool_group",
            json!({ "key": "ask", "enabled": "yes" })
        )
        .unwrap_err()
        .contains("enabled 必须是布尔值"));
        assert!(call(&root, "set_codeg_mcp_tool_group", json!({}))
            .unwrap_err()
            .contains("缺少 key"));
    }

    #[test]
    fn start_service_is_an_idempotent_noop() {
        let root = temp_root("start-service");
        let first = call(&root, "start_codeg_mcp_service", json!({})).unwrap();
        assert!(first.is_null());
        let second = call(&root, "start_codeg_mcp_service", json!({})).unwrap();
        assert!(second.is_null());
    }

    #[test]
    fn unknown_commands_fall_through_to_the_next_module() {
        assert!(handle(&temp_root("fallthrough"), "pet_list", &json!({})).is_none());
    }
}
