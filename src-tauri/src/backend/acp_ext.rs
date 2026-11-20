//! ACP 与智能体扩展 —— 补全批次（19 条）。
//!
//! 前端（`codepluse-ui/src/lib/api.ts`）已经在调用这些命令，但原 Go sidecar
//! （提交 `76712f5`）与 Rust 都没有实现过它们，所以契约只由调用点参数名与
//! `types.ts` 的返回结构决定，没有可对照的 Go 原型。本模块在 `src-tauri/src`
//! 全树复核过：这 19 条字面量此前只出现在本文件的注释里，没有别的真实分派者。
//!
//! 分级说明（对应 SPEC 的 L1/L2/L3）：
//!
//! * **真实落地（L1）**：计划审批记录、Antigravity 登录状态与 settings.json
//!   同步、DeepSeek Harness 目录读写、Kimi Code config.toml 写入、Claude 选择器
//!   id 持久化、泄漏临时目录扫描与回收，都落到真实文件（`.data/` 或各厂商配置
//!   路径）。
//! * **上游不可达 / 能力在本构建不存在（L2/L3）**：cursor-agent / qoder CLI 探针、
//!   pi 运行时目录、Antigravity 浏览器外 OAuth 链接签发与令牌交换、uv 工具链
//!   下载解压、系统智能体整包更新，这些需要独立的 CLI 进程或外部下载链路，归属
//!   ACP 运行时；本模块按前端类型返回确定的空态 / 失败态，不编造凭据、模型或
//!   发布版本。
//!
//! 落盘位置：本工作台状态写 `root/.data/`；厂商配置走各自约定路径，并用环境变量
//! 做测试接缝（`DSH_HOME`、`GEMINI_HOME`、`KIMI_CODE_HOME`、
//! `CODEPLUSE_LEAKED_TEMP_ROOT`、`CODEPLUSE_ACP_RELEASE_URL`）。凭据只落盘，
//! 不写日志、不在响应里回显。

use std::cell::RefCell;
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::{json, Value};

use crate::backend::store_file;

// 测试线程本地覆盖：厂商目录 / 临时根 / 发布源 URL 不走进程级环境变量，
// 避免并行测试互相串改环境变量。生产路径只看环境变量与默认主目录。
thread_local! {
    static OVERRIDES: RefCell<HashMap<&'static str, String>> = RefCell::new(HashMap::new());
}

fn override_str(key: &str) -> Option<String> {
    OVERRIDES.with(|map| map.borrow().get(key).cloned())
}

/// 本项目在系统临时目录里使用的前缀；泄漏扫描与回收只认这些前缀，其余路径
/// 一律不碰。
const LEAK_PREFIXES: [&str; 3] = ["acp-persist-", "acp-splits-", "acp-storeconv-"];
/// 登录尝试的默认有效期（秒），与前端 `expiresInSecs` 字段对齐。
const LOGIN_EXPIRES_SECS: u64 = 900;

pub fn owns(command: &str) -> bool {
    matches!(
        command,
        "acp_answer_plan_approval"
            | "acp_antigravity_login_cancel"
            | "acp_antigravity_login_finish"
            | "acp_antigravity_login_start"
            | "acp_antigravity_sign_out"
            | "acp_cursor_auth_status"
            | "acp_cursor_list_models"
            | "acp_fetch_agent_latest_release"
            | "acp_install_uv_tool"
            | "acp_list_pi_model_capabilities"
            | "acp_load_deepseek_model_catalog"
            | "acp_qoder_auth_status"
            | "acp_reclaim_leaked_temp"
            | "acp_scan_leaked_temp"
            | "acp_sync_antigravity_settings"
            | "acp_update_claude_model_picker"
            | "acp_update_deepseek_model_catalog"
            | "acp_update_kimi_code_config"
            | "acp_update_system_agent"
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
        "acp_answer_plan_approval" => answer_plan_approval(root, args),
        "acp_antigravity_login_start" => antigravity_login_start(root, args),
        "acp_antigravity_login_finish" => antigravity_login_finish(root, args),
        "acp_antigravity_login_cancel" => antigravity_login_cancel(root, args),
        "acp_antigravity_sign_out" => antigravity_sign_out(root),
        "acp_sync_antigravity_settings" => sync_antigravity_settings(root),
        "acp_cursor_auth_status" => cursor_auth_status(args),
        "acp_cursor_list_models" => cursor_list_models(args),
        "acp_qoder_auth_status" => qoder_auth_status(args),
        "acp_fetch_agent_latest_release" => fetch_agent_latest_release(args),
        "acp_install_uv_tool" => install_uv_tool(args),
        "acp_list_pi_model_capabilities" => list_pi_model_capabilities(),
        "acp_load_deepseek_model_catalog" => load_deepseek_catalog(),
        "acp_update_deepseek_model_catalog" => update_deepseek_catalog(args),
        "acp_reclaim_leaked_temp" => reclaim_leaked_temp(args),
        "acp_scan_leaked_temp" => scan_leaked_temp(),
        "acp_update_claude_model_picker" => update_claude_picker(root, args),
        "acp_update_kimi_code_config" => update_kimi_config(args),
        "acp_update_system_agent" => update_system_agent(root, args),
        _ => Err(format!("未知的 ACP 扩展命令：{command}")),
    }
}

// ── 路径与小工具 ──────────────────────────────────────────────────────────

fn data_dir(root: &Path) -> PathBuf {
    root.join(".data")
}

fn home_dir() -> PathBuf {
    crate::backend::acp::providers_core::home()
}

/// 环境变量优先，缺省回到用户主目录下的固定子目录。
fn env_home(env: &str, suffix: &str) -> PathBuf {
    if let Some(override_) = override_str(env) {
        return PathBuf::from(override_);
    }
    if let Ok(value) = std::env::var(env) {
        if !value.trim().is_empty() {
            return PathBuf::from(value);
        }
    }
    home_dir().join(suffix)
}

/// 确保文档里存在一个数组键，返回可变借用；缺失或类型不符时先重置为空数组。
fn ensure_array<'a>(doc: &'a mut Value, key: &str) -> &'a mut Vec<Value> {
    if !doc.get(key).and_then(Value::as_array).is_some() {
        doc[key] = Value::Array(Vec::new());
    }
    doc.get_mut(key).and_then(Value::as_array_mut).unwrap()
}

fn antigravity_settings_path() -> PathBuf {
    env_home("GEMINI_HOME", ".gemini")
        .join("antigravity-acp")
        .join("settings.json")
}

fn dsh_settings_path() -> PathBuf {
    env_home("DSH_HOME", ".dsh").join("settings.yaml")
}

fn kimi_home() -> PathBuf {
    env_home("KIMI_CODE_HOME", ".kimi-code")
}

fn leaked_root() -> PathBuf {
    if let Some(override_) = override_str("CODEPLUSE_LEAKED_TEMP_ROOT") {
        return PathBuf::from(override_);
    }
    if let Ok(value) = std::env::var("CODEPLUSE_LEAKED_TEMP_ROOT") {
        if !value.trim().is_empty() {
            return PathBuf::from(value);
        }
    }
    std::env::temp_dir()
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_millis() as u64)
        .unwrap_or_default()
}

fn text<'a>(args: &'a Value, key: &str) -> Option<&'a str> {
    args.get(key).and_then(Value::as_str)
}

fn require_text(args: &Value, key: &str) -> Result<String, String> {
    text(args, key)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .ok_or_else(|| format!("缺少 {key} 参数"))
}

fn read_store(path: &Path) -> Value {
    store_file::read_json(path)
        .ok()
        .flatten()
        .unwrap_or(Value::Object(Default::default()))
}

// ── 计划审批 ──────────────────────────────────────────────────────────────

const PLAN_DECISIONS: [&str; 3] = ["approve", "request_changes", "abandon"];

/// `acp_answer_plan_approval`：记录用户对阻塞型计划审批的决定。
///
/// 真实的 parked 请求挂在 ACP 会话层；本模块把决定持久化到 `.data/`，供后续
/// 会话拾取。幂等：重复回答同一个 `approvalId` 只是覆盖记录，未知 id 也按
/// 成功处理（与前端注释一致）。
fn answer_plan_approval(root: &Path, args: &Value) -> Result<Value, String> {
    let approval_id = require_text(args, "approvalId")?;
    let connection_id = text(args, "connectionId").unwrap_or("").trim().to_string();
    let answer = args
        .get("answer")
        .ok_or_else(|| "缺少 answer 参数".to_string())?;
    let decision = answer
        .get("decision")
        .and_then(Value::as_str)
        .map(str::trim)
        .ok_or_else(|| "answer.decision 不能为空".to_string())?;
    if !PLAN_DECISIONS.contains(&decision) {
        return Err(format!(
            "answer.decision 无效；可选 {}、{} 或 {}",
            PLAN_DECISIONS[0], PLAN_DECISIONS[1], PLAN_DECISIONS[2]
        ));
    }
    let feedback = answer.get("feedback").cloned().unwrap_or(Value::Null);

    let path = data_dir(root).join("plan-approvals.json");
    let mut doc = read_store(&path);
    let approvals = ensure_array(&mut doc, "approvals");
    let record = json!({
        "connectionId": connection_id,
        "approvalId": approval_id,
        "decision": decision,
        "feedback": feedback,
        "answeredAtMs": now_ms(),
    });
    match approvals.iter_mut().find(|row| row.get("approvalId") == Some(&Value::String(approval_id.clone()))) {
        Some(existing) => *existing = record.clone(),
        None => approvals.push(record),
    }
    store_file::write_json(&path, &doc).map_err(|e| format!("写入审批记录失败：{e}"))?;
    Ok(Value::Null)
}

// ── Antigravity 登录状态 ───────────────────────────────────────────────────

fn auth_state_path(root: &Path) -> PathBuf {
    data_dir(root).join("antigravity-auth.json")
}

fn pending_path(root: &Path) -> PathBuf {
    data_dir(root).join("antigravity-login.json")
}

fn read_auth_state(root: &Path) -> Value {
    read_store(&auth_state_path(root))
}

fn is_signed_in(state: &Value) -> bool {
    state.get("tokenPresent").and_then(Value::as_bool) == Some(true)
}

/// `acp_antigravity_login_start`：浏览器外登录第一步。
///
/// 已持有可用凭据时，直接返回 `alreadySignedIn` 分支（真实读取 `.data/` 状态）；
/// 否则授权链接由智能体进程在启动时签发，本构建未接入该进程链路，按 L3 返回
/// 明确错误，不伪造 consent URL。
fn antigravity_login_start(root: &Path, args: &Value) -> Result<Value, String> {
    let method_id = require_text(args, "methodId")?;
    let state = read_auth_state(root);
    if is_signed_in(&state) {
        return Ok(json!({
            "alreadySignedIn": true,
            "handle": Value::Null,
            "authUrl": Value::Null,
            "redirectUri": Value::Null,
            "methodId": method_id,
            "expiresInSecs": LOGIN_EXPIRES_SECS,
        }));
    }
    Err("当前后端未接入 Antigravity 浏览器外登录流程，无法生成授权链接".into())
}

/// `acp_antigravity_login_finish`：用浏览器回跳地址完成登录。
///
/// 令牌交换需要与智能体进程往返；本构建未接入，返回前端可接受的失败结果
/// （`signedIn: false` + 原因 + 是否可重试），不伪造凭据路径。
fn antigravity_login_finish(root: &Path, args: &Value) -> Result<Value, String> {
    let handle = require_text(args, "handle")?;
    let redirect = require_text(args, "redirect")?;
    if !redirect.starts_with("http://") && !redirect.starts_with("https://") {
        return Err("redirect 必须是 http(s) 地址".into());
    }

    let path = pending_path(root);
    let mut doc = read_store(&path);
    let attempts = ensure_array(&mut doc, "attempts");
    let found = attempts.iter().any(|row| row.get("handle") == Some(&Value::String(handle.clone())));

    if !found {
        return Ok(json!({
            "signedIn": false,
            "message": "登录会话不存在或已过期",
            "retryable": false,
            "credentialPath": Value::Null,
        }));
    }
    // 监听器只应答一次：无论成败，尝试记录都在此刻移除。
    attempts.retain(|row| row.get("handle") != Some(&Value::String(handle.clone())));
    store_file::write_json(&path, &doc).ok();

    Ok(json!({
        "signedIn": false,
        "message": "当前后端未接入 Antigravity 令牌交换流程",
        "retryable": true,
        "credentialPath": Value::Null,
    }))
}

/// `acp_antigravity_login_cancel`：放弃待处理的登录尝试并清理记录。
/// 未知 handle 按幂等成功处理。
fn antigravity_login_cancel(root: &Path, args: &Value) -> Result<Value, String> {
    let handle = require_text(args, "handle")?;
    let path = pending_path(root);
    let mut doc = read_store(&path);
    let attempts = ensure_array(&mut doc, "attempts");
    attempts.retain(|row| row.get("handle") != Some(&Value::String(handle.clone())));
    store_file::write_json(&path, &doc).ok();
    Ok(Value::Null)
}

/// `acp_antigravity_sign_out`：清除本地保存的凭据状态，并从厂商 settings.json
/// 里摘掉 `auth` 块；返回 settings.json 同步报告。
fn antigravity_sign_out(root: &Path) -> Result<Value, String> {
    let _ = fs::remove_file(auth_state_path(root));

    let settings = antigravity_settings_path();
    let report = match fs::read(&settings) {
        Ok(bytes) => {
            let mut doc: Value = serde_json::from_slice(&bytes)
                .map_err(|e| format!("解析 Antigravity settings.json 失败：{e}"))?;
            let Some(obj) = doc.as_object_mut() else {
                return Ok(sync_report(&settings, "skipped", Some("settings.json 根节点不是对象")));
            };
            obj.remove("auth");
            let body = serde_json::to_vec_pretty(&doc).map_err(|e| format!("序列化 settings.json 失败：{e}"))?;
            store_file::write_bytes(&settings, &body)
                .map_err(|e| format!("写入 Antigravity settings.json 失败：{e}"))?;
            sync_report(&settings, "written", None)
        }
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            sync_report(&settings, "skipped", Some("settings.json 不存在，无需清除"))
        }
        Err(err) => return Err(format!("读取 Antigravity settings.json 失败：{err}")),
    };
    Ok(report)
}

fn sync_report(path: &Path, status: &str, reason: Option<&str>) -> Value {
    json!({
        "path": path.to_string_lossy().to_string(),
        "status": status,
        "reason": reason.map(str::to_string),
    })
}

/// `acp_sync_antigravity_settings`：把 `.data/` 里保存的登录方式写进厂商
/// `settings.json` 的 `auth.type`。文件已是当前值报 `already_current`；文件结构
/// 不允许改写时报 `skipped` 并说明原因，不假装保存成功。
fn sync_antigravity_settings(root: &Path) -> Result<Value, String> {
    let settings = antigravity_settings_path();
    let state = read_auth_state(root);
    let method_id = state
        .get("methodId")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty());
    let Some(method_id) = method_id else {
        return Ok(sync_report(&settings, "skipped", Some("尚未保存 Antigravity 登录方式")));
    };

    let existing = match fs::read(&settings) {
        Ok(bytes) => Some(
            serde_json::from_slice::<Value>(&bytes)
                .map_err(|e| format!("解析 Antigravity settings.json 失败：{e}"))?,
        ),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => None,
        Err(err) => return Err(format!("读取 Antigravity settings.json 失败：{err}")),
    };

    let Some(mut doc) = existing else {
        let doc = json!({ "auth": { "type": method_id } });
        if let Some(parent) = settings.parent() {
            fs::create_dir_all(parent).map_err(|e| format!("创建 Antigravity 配置目录失败：{e}"))?;
        }
        let body = serde_json::to_vec_pretty(&doc).map_err(|e| format!("序列化 settings.json 失败：{e}"))?;
        store_file::write_bytes(&settings, &body)
            .map_err(|e| format!("写入 Antigravity settings.json 失败：{e}"))?;
        return Ok(sync_report(&settings, "written", None));
    };

    let Some(obj) = doc.as_object_mut() else {
        return Ok(sync_report(&settings, "skipped", Some("settings.json 根节点不是对象")));
    };
    let auth = obj
        .get_mut("auth")
        .ok_or(())
        .and_then(|value| value.as_object_mut().ok_or(()));
    let Ok(auth) = auth else {
        return Ok(sync_report(&settings, "skipped", Some("settings.json 的 auth 节点不是对象")));
    };

    let current = auth.get("type").and_then(Value::as_str).unwrap_or("");
    if current == method_id {
        return Ok(sync_report(&settings, "already_current", None));
    }
    auth.insert("type".into(), json!(method_id));
    let body = serde_json::to_vec_pretty(&doc).map_err(|e| format!("序列化 settings.json 失败：{e}"))?;
    store_file::write_bytes(&settings, &body)
        .map_err(|e| format!("写入 Antigravity settings.json 失败：{e}"))?;
    Ok(sync_report(&settings, "written", None))
}

// ── cursor / qoder 授权状态与模型列表 ──────────────────────────────────────

/// `acp_cursor_auth_status`：探针需要拉起 `cursor-agent status --format json`。
/// 该 CLI 探针归属 ACP 运行时，本模块不拉起外部进程；返回前端可渲染的空态，
/// `error` 说明原因，不臆造登录态。传入的 `apiKey` 只用于屏幕上的实时探针，
/// 这里不落盘、不回显。
fn cursor_auth_status(_args: &Value) -> Result<Value, String> {
    Ok(json!({
        "installed": false,
        "is_authenticated": false,
        "raw_status": Value::Null,
        "email": Value::Null,
        "membership": Value::Null,
        "error": "当前后端未接入 cursor-agent 状态探针",
        "binary_path": Value::Null,
    }))
}

/// `acp_cursor_list_models`：同上，模型列表由 `cursor-agent models` 提供。
fn cursor_list_models(_args: &Value) -> Result<Value, String> {
    Ok(json!({
        "models": [],
        "default_model": Value::Null,
        "error": "当前后端未接入 cursor-agent 模型列表探针",
    }))
}

/// `acp_qoder_auth_status`：探针需要拉起 `qoder status -o json`，本模块不拉起。
fn qoder_auth_status(_args: &Value) -> Result<Value, String> {
    Ok(json!({
        "installed": false,
        "logged_in": false,
        "username": Value::Null,
        "email": Value::Null,
        "user_type": Value::Null,
        "version": Value::Null,
        "allow_byok": Value::Null,
        "error": "当前后端未接入 qoder 状态探针",
        "binary_path": Value::Null,
    }))
}

// ── 上游发布版本 / uv 工具链 / pi 目录 ─────────────────────────────────────

/// `acp_fetch_agent_latest_release`：查询某智能体比钉住版本更新的上游发布。
///
/// 上游地址由 `CODEPLUSE_ACP_RELEASE_URL` 覆盖（测试桩）；未配置时本构建没有
/// 可用的发布源，按 L3 返回 `null`。请求短超时，支持 `{version}`、
/// `{dist-tags:{latest}}` 与 `{tag_name}` 三种上游形状。
fn fetch_agent_latest_release(args: &Value) -> Result<Value, String> {
    let agent_type = require_text(args, "agentType")?;
    let url = override_str("CODEPLUSE_ACP_RELEASE_URL")
        .or_else(|| {
            std::env::var("CODEPLUSE_ACP_RELEASE_URL")
                .ok()
                .filter(|value| !value.trim().is_empty())
        });
    let Some(url) = url else {
        return Ok(Value::Null);
    };

    let response = crate::backend::net::client()
        .get(&url)
        .timeout(std::time::Duration::from_secs(10))
        .send()
        .map_err(|e| format!("查询智能体 {agent_type} 最新版本失败：{e}"))?;
    let status = response.status().as_u16();
    if status != 200 {
        return Err(format!("查询智能体 {agent_type} 最新版本失败：HTTP {status}"));
    }
    let body = response.bytes().map_err(|e| format!("读取版本响应失败：{e}"))?;
    let payload: Value =
        serde_json::from_slice(&body).map_err(|e| format!("版本响应 JSON 无效：{e}"))?;
    let version = payload
        .get("version")
        .and_then(Value::as_str)
        .or_else(|| payload.pointer("/dist-tags/latest").and_then(Value::as_str))
        .or_else(|| payload.get("tag_name").and_then(Value::as_str))
        .map(str::trim)
        .filter(|value| !value.is_empty());
    match version {
        Some(version) => Ok(json!({ "version": version })),
        None => Ok(Value::Null),
    }
}

/// `acp_install_uv_tool`：下载并解压 uv 工具链。下载链路归属 ACP 运行时，
/// 本构建未接入；先按既有约定发出安装事件，再返回明确错误，不伪装成功。
fn install_uv_tool(args: &Value) -> Result<Value, String> {
    let task_id = require_text(args, "taskId")?;
    if task_id.len() > 256 {
        return Err("taskId 长度无效".into());
    }
    crate::backend::emit_event(
        "app://agent-install",
        json!({
            "task_id": task_id,
            "agent_type": "uv",
            "kind": "started",
            "payload": "正在准备 uv 工具链",
        }),
    );
    Err("当前后端未接入 uv 工具链下载与解压流程".into())
}

/// `acp_list_pi_model_capabilities`：目录由配置好的 pi 运行时离线回答；本模块
/// 不拉起 pi 进程，按类型返回 `failed` 空目录。
fn list_pi_model_capabilities() -> Result<Value, String> {
    Ok(json!({ "status": "failed", "models": [] }))
}

// ── DeepSeek Harness 模型目录 ─────────────────────────────────────────────

const DEEPSEEK_MODEL_FIELDS: [&str; 9] = [
    "id",
    "name",
    "description",
    "contextWindow",
    "maxTokens",
    "inputModalities",
    "imagePixelBudget",
    "imageMaxBytes",
    "systemPromptUpdate",
];

fn yaml_to_json(value: &serde_yaml::Value) -> Value {
    use serde_yaml::Value as Y;
    match value {
        Y::Null => Value::Null,
        Y::Bool(b) => json!(b),
        Y::Number(n) => n
            .as_i64()
            .map(|v| json!(v))
            .or_else(|| n.as_u64().map(|v| json!(v)))
            .unwrap_or_else(|| json!(n.as_f64().unwrap_or(0.0))),
        Y::String(s) => json!(s),
        Y::Sequence(seq) => Value::Array(seq.iter().map(yaml_to_json).collect()),
        Y::Mapping(map) => {
            let mut out = serde_json::Map::new();
            for (key, item) in map {
                let key = key.as_str().unwrap_or_default().to_string();
                out.insert(key, yaml_to_json(item));
            }
            Value::Object(out)
        }
        Y::Tagged(tagged) => yaml_to_json(&tagged.value),
    }
}

fn json_to_yaml(value: &Value) -> serde_yaml::Value {
    use serde_yaml::Value as Y;
    match value {
        Value::Null => Y::Null,
        Value::Bool(b) => Y::Bool(*b),
        Value::Number(n) => Y::Number(
            n.as_i64()
                .map(serde_yaml::Number::from)
                .or_else(|| n.as_u64().map(serde_yaml::Number::from))
                .unwrap_or_else(|| serde_yaml::Number::from(n.as_f64().unwrap_or(0.0))),
        ),
        Value::String(s) => Y::String(s.clone()),
        Value::Array(items) => Y::Sequence(items.iter().map(json_to_yaml).collect()),
        Value::Object(map) => {
            let mapping = map
                .iter()
                .map(|(key, item)| (Y::String(key.clone()), json_to_yaml(item)))
                .collect();
            Y::Mapping(mapping)
        }
    }
}

/// 校验目录条目；重复 id、非正上下文窗口、非法模态/投递方式在此被拒。
fn validate_deepseek_models(rows: &[Value]) -> Result<(), String> {
    let mut ids = std::collections::HashSet::new();
    for row in rows {
        let id = row
            .get("id")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .ok_or_else(|| "模型条目缺少 id".to_string())?
            .to_string();
        if !ids.insert(id.clone()) {
            return Err(format!("模型 id 重复：{id}"));
        }
        if let Some(ctx) = row.get("contextWindow").and_then(Value::as_i64) {
            if ctx <= 0 {
                return Err(format!("模型 {id} 的 contextWindow 必须大于 0"));
            }
        }
        if let Some(sys) = row.get("systemPromptUpdate").and_then(Value::as_str) {
            if sys != "in-history" {
                return Err(format!("模型 {id} 的 systemPromptUpdate 只接受 in-history"));
            }
        }
        if let Some(mods) = row.get("inputModalities").and_then(Value::as_array) {
            for modality in mods {
                let Some(modality) = modality.as_str() else {
                    return Err(format!("模型 {id} 的 inputModalities 必须是字符串数组"));
                };
                if !["text", "image"].contains(&modality) {
                    return Err(format!("模型 {id} 的 inputModalities 包含非法值：{modality}"));
                }
            }
        }
    }
    Ok(())
}

fn deepseek_models_of(doc: &serde_yaml::Value) -> Option<&serde_yaml::Sequence> {
    doc.get("llm-deepseek")?
        .get("models")?
        .as_sequence()
}

/// `acp_load_deepseek_model_catalog`：读取 `$DSH_HOME/settings.yaml` 的
/// `llm-deepseek.models`。文件缺失是“继承内置目录”而非错误；文件不可读时
/// `error` 置位，面板据此拒绝覆盖。
fn load_deepseek_catalog() -> Result<Value, String> {
    let path = dsh_settings_path();
    let exists = path.is_file();
    if !exists {
        return Ok(json!({
            "path": path.to_string_lossy().to_string(),
            "exists": false,
            "configured": false,
            "models": [],
            "error": Value::Null,
            "invalid": Value::Null,
        }));
    }
    let raw = fs::read_to_string(&path)
        .map_err(|e| format!("读取 DeepSeek settings.yaml 失败：{e}"))?;
    let doc: serde_yaml::Value = match serde_yaml::from_str(&raw) {
        Ok(doc) => doc,
        Err(err) => {
            return Ok(json!({
                "path": path.to_string_lossy().to_string(),
                "exists": true,
                "configured": false,
                "models": [],
                "error": format!("settings.yaml 解析失败：{err}"),
                "invalid": Value::Null,
            }));
        }
    };

    let Some(sequence) = deepseek_models_of(&doc) else {
        return Ok(json!({
            "path": path.to_string_lossy().to_string(),
            "exists": true,
            "configured": false,
            "models": [],
            "error": Value::Null,
            "invalid": Value::Null,
        }));
    };

    let models: Vec<Value> = sequence.iter().map(yaml_to_json).collect();
    let invalid = validate_deepseek_models(&models).err();
    Ok(json!({
        "path": path.to_string_lossy().to_string(),
        "exists": true,
        "configured": true,
        "models": models,
        "error": Value::Null,
        "invalid": invalid,
    }))
}

/// `acp_update_deepseek_model_catalog`：替换 `llm-deepseek.models`，其余键保留。
/// `null` 或空列表移除该键，回到内置目录继承；非法条目在写盘前拒绝。
fn update_deepseek_catalog(args: &Value) -> Result<Value, String> {
    let path = dsh_settings_path();
    let stored = match fs::read_to_string(&path) {
        Ok(raw) => serde_yaml::from_str::<serde_yaml::Value>(&raw)
            .map_err(|e| format!("settings.yaml 解析失败：{e}"))?,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            serde_yaml::Value::Mapping(Default::default())
        }
        Err(err) => return Err(format!("读取 DeepSeek settings.yaml 失败：{err}")),
    };

    let incoming = args.get("models").cloned().unwrap_or(Value::Null);
    let rows: Vec<Value> = match incoming {
        Value::Null => Vec::new(),
        Value::Array(items) => items
            .iter()
            .map(|row| {
                let mut kept = serde_json::Map::new();
                for field in DEEPSEEK_MODEL_FIELDS {
                    if let Some(value) = row.get(field) {
                        kept.insert((*field).into(), value.clone());
                    }
                }
                Value::Object(kept)
            })
            .collect(),
        _ => return Err("models 必须是数组或 null".into()),
    };
    validate_deepseek_models(&rows)?;

    let mut doc = stored;
    let mapping = doc
        .as_mapping_mut()
        .ok_or_else(|| "settings.yaml 根节点必须是 YAML 对象".to_string())?;
    let deepseek = mapping
        .entry(serde_yaml::Value::String("llm-deepseek".into()))
        .or_insert_with(|| serde_yaml::Value::Mapping(Default::default()));
    let deepseek = deepseek
        .as_mapping_mut()
        .ok_or_else(|| "llm-deepseek 节点必须是 YAML 对象".to_string())?;

    if rows.is_empty() {
        deepseek.remove(serde_yaml::Value::String("models".into()));
    } else {
        let sequence = rows.iter().map(json_to_yaml).collect();
        deepseek.insert(
            serde_yaml::Value::String("models".into()),
            serde_yaml::Value::Sequence(sequence),
        );
    }

    let rendered = serde_yaml::to_string(&doc).map_err(|e| format!("序列化 settings.yaml 失败：{e}"))?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|e| format!("创建 DeepSeek 配置目录失败：{e}"))?;
    }
    store_file::write_bytes(&path, rendered.as_bytes())
        .map_err(|e| format!("写入 DeepSeek settings.yaml 失败：{e}"))?;
    Ok(Value::Null)
}

// ── 泄漏临时目录扫描与回收 ────────────────────────────────────────────────

fn dir_size(path: &Path) -> u64 {
    let mut total = 0u64;
    let Ok(entries) = fs::read_dir(path) else {
        return total;
    };
    for entry in entries.flatten() {
        let Ok(meta) = entry.metadata() else { continue };
        if meta.is_dir() {
            total += dir_size(&entry.path());
        } else {
            total += meta.len();
        }
    }
    total
}

fn scan_leaked_entries(root: &Path) -> (Vec<Value>, u64, usize) {
    let mut entries = Vec::new();
    let mut total_bytes = 0u64;
    let mut skipped = 0usize;
    let Ok(dir) = fs::read_dir(root) else {
        return (entries, total_bytes, skipped);
    };
    for entry in dir.flatten() {
        let name = entry.file_name().to_string_lossy().to_string();
        if !LEAK_PREFIXES.iter().any(|prefix| name.starts_with(prefix)) {
            continue;
        }
        let Ok(meta) = entry.metadata() else { continue };
        let age_hours = meta
            .modified()
            .ok()
            .and_then(|mtime| SystemTime::now().duration_since(mtime).ok())
            .map(|elapsed| elapsed.as_secs_f64() / 3600.0)
            .unwrap_or(0.0);
        // 太新的可能仍在使用，留给下一轮；占用检测需要进程信息，本模块不做。
        if age_hours < 1.0 {
            skipped += 1;
            continue;
        }
        let bytes = if meta.is_dir() { dir_size(&entry.path()) } else { meta.len() };
        total_bytes += bytes;
        entries.push(json!({
            "path": entry.path().to_string_lossy().to_string(),
            "bytes": bytes,
            "age_hours": age_hours,
            "is_dir": meta.is_dir(),
        }));
    }
    entries.sort_by(|a, b| {
        a.get("path")
            .and_then(Value::as_str)
            .cmp(&b.get("path").and_then(Value::as_str))
    });
    (entries, total_bytes, skipped)
}

/// `acp_scan_leaked_temp`：只读扫描本项目前缀的临时目录/文件。
fn scan_leaked_temp() -> Result<Value, String> {
    let root = leaked_root();
    let (entries, total_bytes, skipped) = scan_leaked_entries(&root);
    Ok(json!({
        "root": root.to_string_lossy().to_string(),
        "entries": entries,
        "total_bytes": total_bytes,
        "skipped": skipped,
    }))
}

/// `acp_reclaim_leaked_temp`：先重新扫描，再逐条复核路径——必须位于扫描根之下、
/// 且文件名匹配本项目前缀，否则只进 `failed`，绝不删除未知路径。
fn reclaim_leaked_temp(args: &Value) -> Result<Value, String> {
    let requested = args
        .get("paths")
        .and_then(Value::as_array)
        .ok_or_else(|| "缺少 paths 参数".to_string())?;
    let root = leaked_root();
    let root_canon = fs::canonicalize(&root).unwrap_or_else(|_| root.clone());

    let mut removed = 0usize;
    let mut freed_bytes = 0u64;
    let mut failed: Vec<String> = Vec::new();

    for item in requested {
        let Some(raw) = item.as_str() else {
            failed.push("<非字符串路径>".to_string());
            continue;
        };
        let path = PathBuf::from(raw);
        let Ok(canon) = fs::canonicalize(&path) else {
            failed.push(raw.to_string());
            continue;
        };
        let name = canon
            .file_name()
            .and_then(|value| value.to_str())
            .unwrap_or("");
        let under_root = canon.starts_with(&root_canon);
        let prefix_ok = LEAK_PREFIXES.iter().any(|prefix| name.starts_with(prefix));
        if !under_root || !prefix_ok {
            failed.push(raw.to_string());
            continue;
        }
        let Ok(meta) = fs::metadata(&canon) else {
            failed.push(raw.to_string());
            continue;
        };
        let bytes = if meta.is_dir() { dir_size(&canon) } else { meta.len() };
        let result = if meta.is_dir() {
            fs::remove_dir_all(&canon)
        } else {
            fs::remove_file(&canon)
        };
        match result {
            Ok(()) => {
                removed += 1;
                freed_bytes += bytes;
            }
            Err(_) => failed.push(raw.to_string()),
        }
    }
    Ok(json!({
        "removed": removed,
        "freed_bytes": freed_bytes,
        "failed": failed,
    }))
}

// ── Claude 模型选择器 ─────────────────────────────────────────────────────

/// `acp_update_claude_model_picker`：规范化并持久化 codepluse 为未绑定 Claude
/// 智能体补充的选择器 id。返回需要重启的在跑会话数；会话池归属 ACP 运行时，
/// 本模块统计不到，按 0 返回（limited）。
fn update_claude_picker(root: &Path, args: &Value) -> Result<Value, String> {
    let raw = args
        .get("models")
        .and_then(Value::as_array)
        .ok_or_else(|| "缺少 models 参数".to_string())?;
    let mut models: Vec<String> = Vec::new();
    for item in raw {
        let Some(id) = item.as_str() else {
            return Err("models 条目必须是非空字符串".into());
        };
        let id = id.trim();
        if id.is_empty() {
            continue;
        }
        if !models.iter().any(|existing| existing == id) {
            models.push(id.to_string());
        }
    }
    let path = data_dir(root).join("claude-model-picker.json");
    let doc = json!({ "models": models, "updatedAtMs": now_ms() });
    store_file::write_json(&path, &doc).map_err(|e| format!("写入 Claude 选择器配置失败：{e}"))?;
    Ok(json!(0))
}

// ── Kimi Code 配置 ─────────────────────────────────────────────────────────

const KIMI_MODES: [&str; 3] = ["apikey", "login", "raw"];

fn kimi_gate_token_path() -> PathBuf {
    kimi_home().join(".gate-token")
}

fn seed_gate_token() -> Result<(), String> {
    let home = kimi_home();
    fs::create_dir_all(&home).map_err(|e| format!("创建 Kimi 配置目录失败：{e}"))?;
    let token = format!("{:032x}", rand::random::<u128>());
    store_file::write_bytes(&kimi_gate_token_path(), token.as_bytes())
}

fn clear_gate_token() {
    let _ = fs::remove_file(kimi_gate_token_path());
}

/// 受管 provider 块的标量键：非空写入，空/缺失删除该键（与 types.ts 的
/// “空字符串标量移除该键”约定一致）。
fn kimi_set_scalar(provider: &mut toml::Table, key: &str, value: Option<&str>) {
    match value.map(str::trim) {
        Some(value) if !value.is_empty() => {
            provider.insert(key.into(), toml::Value::String(value.to_string()));
        }
        _ => {
            provider.remove(key);
        }
    }
}

/// `acp_update_kimi_code_config`：写 `~/.kimi-code/config.toml`。
///
/// * `raw`：原样写入 TOML（先解析校验），再播种门控令牌；
/// * `apikey`：合并受管 provider 块并播种门控令牌；
/// * `login`：移除受管块与合成令牌，交回真实 OAuth 登录。
///
/// 返回需要重启的在跑 Kimi 会话数；会话池归属 ACP 运行时，按 0 返回（limited）。
fn update_kimi_config(args: &Value) -> Result<Value, String> {
    let mode = text(args, "mode")
        .map(str::trim)
        .ok_or_else(|| "缺少 mode 参数".to_string())?;
    if !KIMI_MODES.contains(&mode) {
        return Err(format!("mode 无效；可选 {}、{} 或 {}", KIMI_MODES[0], KIMI_MODES[1], KIMI_MODES[2]));
    }

    let home = kimi_home();
    fs::create_dir_all(&home).map_err(|e| format!("创建 Kimi 配置目录失败：{e}"))?;
    let path = home.join("config.toml");

    match mode {
        "raw" => {
            let raw = text(args, "rawConfigToml")
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .ok_or_else(|| "raw 模式缺少 rawConfigToml 参数".to_string())?;
            if raw.len() > 2 * 1024 * 1024 {
                return Err("Kimi config.toml 超过 2 MiB".into());
            }
            toml::from_str::<toml::Value>(raw).map_err(|e| format!("Kimi TOML 格式无效：{e}"))?;
            store_file::write_bytes(&path, raw.as_bytes())
                .map_err(|e| format!("写入 Kimi config.toml 失败：{e}"))?;
            seed_gate_token()?;
        }
        "login" => {
            let mut doc = match fs::read_to_string(&path) {
                Ok(raw) => toml::from_str::<toml::Value>(&raw)
                    .map_err(|e| format!("Kimi TOML 格式无效：{e}"))?,
                Err(err) if err.kind() == std::io::ErrorKind::NotFound => toml::Value::Table(Default::default()),
                Err(err) => return Err(format!("读取 Kimi config.toml 失败：{err}")),
            };
            if let Some(table) = doc.as_table_mut() {
                table.remove("provider");
            }
            let rendered = toml::to_string(&doc).map_err(|e| format!("序列化 Kimi config.toml 失败：{e}"))?;
            store_file::write_bytes(&path, rendered.as_bytes())
                .map_err(|e| format!("写入 Kimi config.toml 失败：{e}"))?;
            clear_gate_token();
        }
        "apikey" => {
            let mut doc = match fs::read_to_string(&path) {
                Ok(raw) => toml::from_str::<toml::Value>(&raw)
                    .map_err(|e| format!("Kimi TOML 格式无效：{e}"))?,
                Err(err) if err.kind() == std::io::ErrorKind::NotFound => toml::Value::Table(Default::default()),
                Err(err) => return Err(format!("读取 Kimi config.toml 失败：{err}")),
            };
            let provider = doc
                .as_table_mut()
                .ok_or_else(|| "Kimi config.toml 根节点必须是表".to_string())?
                .entry("provider")
                .or_insert_with(|| toml::Value::Table(Default::default()));
            let provider = provider
                .as_table_mut()
                .ok_or_else(|| "config.toml 的 provider 节点必须是表".to_string())?;

            kimi_set_scalar(provider, "type", text(args, "interfaceType"));
            kimi_set_scalar(provider, "auth_type", text(args, "authType"));
            kimi_set_scalar(provider, "base_url", text(args, "baseUrl"));
            kimi_set_scalar(provider, "api_key", text(args, "apiKey"));
            kimi_set_scalar(provider, "model", text(args, "model"));
            kimi_set_scalar(provider, "default_effort", text(args, "defaultEffort"));
            if let Some(ctx) = args.get("maxContextSize").and_then(Value::as_i64) {
                provider.insert("max_context_size".into(), toml::Value::Integer(ctx));
            } else {
                provider.remove("max_context_size");
            }
            kimi_set_scalar(provider, "vertex_project", text(args, "vertexProject"));
            kimi_set_scalar(provider, "vertex_location", text(args, "vertexLocation"));
            if let Some(flag) = args.get("reasoningEnabled").and_then(Value::as_bool) {
                provider.insert("reasoning_enabled".into(), toml::Value::Boolean(flag));
            } else {
                provider.remove("reasoning_enabled");
            }
            if let Some(flag) = args.get("alwaysThinking").and_then(Value::as_bool) {
                provider.insert("always_thinking".into(), toml::Value::Boolean(flag));
            } else {
                provider.remove("always_thinking");
            }
            match args.get("supportEfforts").and_then(Value::as_array) {
                Some(items) => {
                    let efforts: Vec<toml::Value> = items
                        .iter()
                        .filter_map(|item| item.as_str().map(str::trim).filter(|s| !s.is_empty()).map(|s| toml::Value::String(s.to_string())))
                        .collect();
                    provider.insert("support_efforts".into(), toml::Value::Array(efforts));
                }
                None => {
                    provider.remove("support_efforts");
                }
            }

            let rendered = toml::to_string(&doc).map_err(|e| format!("序列化 Kimi config.toml 失败：{e}"))?;
            store_file::write_bytes(&path, rendered.as_bytes())
                .map_err(|e| format!("写入 Kimi config.toml 失败：{e}"))?;
            seed_gate_token()?;
        }
        _ => unreachable!(),
    }
    Ok(json!(0))
}

// ── 系统智能体更新 ────────────────────────────────────────────────────────

/// `acp_update_system_agent`：记录一次系统智能体整包更新请求。整包下载与更新
/// 归属 ACP 运行时；本构建不下载构建产物，返回 `null`（无新版本），请求记录
/// 落 `.data/` 供运行时拾取。
fn update_system_agent(root: &Path, args: &Value) -> Result<Value, String> {
    let agent_type = require_text(args, "agentType")?;
    let task_id = require_text(args, "taskId")?;
    if task_id.len() > 256 {
        return Err("taskId 长度无效".into());
    }
    let path = data_dir(root).join("agent-updates.json");
    let mut doc = read_store(&path);
    let updates = ensure_array(&mut doc, "updates");
    updates.push(json!({
        "agentType": agent_type,
        "taskId": task_id,
        "requestedAtMs": now_ms(),
    }));
    store_file::write_json(&path, &doc).map_err(|e| format!("写入更新记录失败：{e}"))?;
    Ok(Value::Null)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::io::{Read, Write};
    use std::net::TcpListener;

    fn temp_root(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "codepluse-acp-ext-{tag}-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(dir.join(".data")).unwrap();
        dir
    }

    fn call(root: &Path, command: &str, args: Value) -> Result<Value, String> {
        handle(root, command, &args).unwrap()
    }

    struct OverrideGuard {
        key: &'static str,
    }

    impl OverrideGuard {
        fn set(key: &'static str, value: &str) -> Self {
            OVERRIDES.with(|map| map.borrow_mut().insert(key, value.to_string()));
            Self { key }
        }
    }

    impl Drop for OverrideGuard {
        fn drop(&mut self) {
            OVERRIDES.with(|map| map.borrow_mut().remove(self.key));
        }
    }

    #[test]
    fn owns_covers_all_nineteen_commands_only() {
        for command in [
            "acp_answer_plan_approval",
            "acp_antigravity_login_cancel",
            "acp_antigravity_login_finish",
            "acp_antigravity_login_start",
            "acp_antigravity_sign_out",
            "acp_cursor_auth_status",
            "acp_cursor_list_models",
            "acp_fetch_agent_latest_release",
            "acp_install_uv_tool",
            "acp_list_pi_model_capabilities",
            "acp_load_deepseek_model_catalog",
            "acp_qoder_auth_status",
            "acp_reclaim_leaked_temp",
            "acp_scan_leaked_temp",
            "acp_sync_antigravity_settings",
            "acp_update_claude_model_picker",
            "acp_update_deepseek_model_catalog",
            "acp_update_kimi_code_config",
            "acp_update_system_agent",
        ] {
            assert!(owns(command), "{command} should be owned");
        }
        assert!(!owns("acp_answer_question"));
        assert!(!owns("acp_update_hermes_config"));
        assert!(!owns("acp_scan_leaked"));
        assert!(!owns("ACP_ANTIGRAVITY_LOGIN_START"));
    }

    #[test]
    fn plan_approval_persists_and_is_idempotent() {
        let root = temp_root("plan");
        let echoed = call(
            &root,
            "acp_answer_plan_approval",
            json!({"connectionId": "conn-1", "approvalId": "appr-9", "answer": {"decision": "approve", "feedback": null}}),
        )
        .unwrap();
        assert_eq!(echoed, Value::Null);

        // 重复回答覆盖同一条记录，不产生第二行。
        call(
            &root,
            "acp_answer_plan_approval",
            json!({"connectionId": "conn-1", "approvalId": "appr-9", "answer": {"decision": "request_changes", "feedback": "再改"}}),
        )
        .unwrap();
        let doc = read_store(&data_dir(&root).join("plan-approvals.json"));
        let rows = doc["approvals"].as_array().unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0]["decision"], "request_changes");
        assert_eq!(rows[0]["feedback"], "再改");
    }

    #[test]
    fn plan_approval_rejects_missing_or_invalid_input() {
        let root = temp_root("plan-bad");
        assert!(call(&root, "acp_answer_plan_approval", json!({}))
            .unwrap_err()
            .contains("approvalId"));
        assert!(call(
            &root,
            "acp_answer_plan_approval",
            json!({"approvalId": "a", "answer": {"decision": "maybe"}})
        )
        .unwrap_err()
        .contains("decision 无效"));
        assert!(call(
            &root,
            "acp_answer_plan_approval",
            json!({"approvalId": "a"})
        )
        .unwrap_err()
        .contains("缺少 answer"));
    }

    #[test]
    fn antigravity_start_reports_signed_in_from_saved_state() {
        let root = temp_root("ag-start");
        store_file::write_json(
            &auth_state_path(&root),
            &json!({"tokenPresent": true, "methodId": "oauth", "savedAtMs": 1}),
        )
        .unwrap();
        let view = call(&root, "acp_antigravity_login_start", json!({"methodId": "oauth"})).unwrap();
        assert_eq!(view["alreadySignedIn"], true);
        assert!(view["handle"].is_null());
        assert!(view["authUrl"].is_null());
        assert_eq!(view["methodId"], "oauth");
        assert!(view["expiresInSecs"].as_u64().unwrap() > 0);
    }

    #[test]
    fn antigravity_start_without_credentials_is_limited_and_validates_input() {
        let root = temp_root("ag-start-empty");
        assert!(call(&root, "acp_antigravity_login_start", json!({}))
            .unwrap_err()
            .contains("methodId"));
        let err = call(&root, "acp_antigravity_login_start", json!({"methodId": "oauth"})).unwrap_err();
        assert!(err.contains("未接入"));
    }

    #[test]
    fn antigravity_finish_unknown_session_is_not_retryable() {
        let root = temp_root("ag-finish");
        let outcome = call(
            &root,
            "acp_antigravity_login_finish",
            json!({"handle": "missing", "redirect": "http://127.0.0.1/cb?code=x"}),
        )
        .unwrap();
        assert_eq!(outcome["signedIn"], false);
        assert_eq!(outcome["retryable"], false);
        assert!(outcome["credentialPath"].is_null());
        assert!(outcome["message"].as_str().unwrap().contains("过期"));
    }

    #[test]
    fn antigravity_finish_removes_pending_attempt_and_reports_failure() {
        let root = temp_root("ag-finish-pending");
        let pending = json!({"attempts": [{"handle": "h-1", "methodId": "oauth", "createdAtMs": 1, "expiresInSecs": 900}]});
        store_file::write_json(&pending_path(&root), &pending).unwrap();
        let outcome = call(
            &root,
            "acp_antigravity_login_finish",
            json!({"handle": "h-1", "redirect": "http://127.0.0.1/cb?code=x"}),
        )
        .unwrap();
        assert_eq!(outcome["signedIn"], false);
        assert_eq!(outcome["retryable"], true);
        let doc = read_store(&pending_path(&root));
        assert_eq!(doc["attempts"].as_array().unwrap().len(), 0);
    }

    #[test]
    fn antigravity_finish_rejects_bad_redirect_and_missing_handle() {
        let root = temp_root("ag-finish-bad");
        assert!(call(&root, "acp_antigravity_login_finish", json!({"handle": "h"}))
            .unwrap_err()
            .contains("redirect"));
        assert!(call(
            &root,
            "acp_antigravity_login_finish",
            json!({"handle": "h", "redirect": "ftp://x"})
        )
        .unwrap_err()
        .contains("http(s)"));
    }

    #[test]
    fn antigravity_cancel_clears_pending_attempt_and_is_idempotent() {
        let root = temp_root("ag-cancel");
        let pending = json!({"attempts": [{"handle": "h-1"}, {"handle": "h-2"}]});
        store_file::write_json(&pending_path(&root), &pending).unwrap();
        call(&root, "acp_antigravity_login_cancel", json!({"handle": "h-1"})).unwrap();
        let doc = read_store(&pending_path(&root));
        assert_eq!(doc["attempts"].as_array().unwrap().len(), 1);
        // 未知 handle 也是成功空操作。
        call(&root, "acp_antigravity_login_cancel", json!({"handle": "nope"})).unwrap();
        assert!(call(&root, "acp_antigravity_login_cancel", json!({}))
            .unwrap_err()
            .contains("handle"));
    }

    #[test]
    fn antigravity_sign_out_clears_state_and_vendor_auth_block() {
        let root = temp_root("ag-signout");
        let _guard = OverrideGuard::set("GEMINI_HOME", temp_root("ag-gemini").to_str().unwrap());
        store_file::write_json(
            &auth_state_path(&root),
            &json!({"tokenPresent": true, "methodId": "oauth"}),
        )
        .unwrap();
        let settings = antigravity_settings_path();
        fs::create_dir_all(settings.parent().unwrap()).unwrap();
        fs::write(&settings, json!({"auth": {"type": "oauth", "refresh": "x"}}).to_string()).unwrap();

        let report = call(&root, "acp_antigravity_sign_out", json!({})).unwrap();
        assert_eq!(report["status"], "written");
        assert!(report["path"].as_str().unwrap().contains("settings.json"));
        assert!(!auth_state_path(&root).exists());
        let vendor: Value = serde_json::from_slice(&fs::read(&settings).unwrap()).unwrap();
        assert!(vendor.get("auth").is_none());
    }

    #[test]
    fn sync_antigravity_writes_already_current_or_skips() {
        let root = temp_root("ag-sync");
        let gemini = temp_root("ag-sync-gemini");
        let _guard = OverrideGuard::set("GEMINI_HOME", gemini.to_str().unwrap());
        store_file::write_json(
            &auth_state_path(&root),
            &json!({"tokenPresent": true, "methodId": "oauth"}),
        )
        .unwrap();

        // 首次写入。
        let report = call(&root, "acp_sync_antigravity_settings", json!({})).unwrap();
        assert_eq!(report["status"], "written");
        let settings = antigravity_settings_path();
        let vendor: Value = serde_json::from_slice(&fs::read(&settings).unwrap()).unwrap();
        assert_eq!(vendor["auth"]["type"], "oauth");

        // 再次同步：已是当前值。
        let report = call(&root, "acp_sync_antigravity_settings", json!({})).unwrap();
        assert_eq!(report["status"], "already_current");

        // 根节点不是对象 → skipped。
        fs::write(&settings, "[1,2,3]").unwrap();
        let report = call(&root, "acp_sync_antigravity_settings", json!({})).unwrap();
        assert_eq!(report["status"], "skipped");
        assert!(report["reason"].as_str().unwrap().contains("根节点"));
    }

    #[test]
    fn sync_antigravity_skips_when_no_saved_method() {
        let root = temp_root("ag-sync-empty");
        let gemini = temp_root("ag-sync-empty-gemini");
        let _guard = OverrideGuard::set("GEMINI_HOME", gemini.to_str().unwrap());
        let report = call(&root, "acp_sync_antigravity_settings", json!({})).unwrap();
        assert_eq!(report["status"], "skipped");
        assert!(report["reason"].as_str().unwrap().contains("登录方式"));
    }

    #[test]
    fn cursor_status_and_models_return_limited_empty_shape() {
        let root = temp_root("cursor");
        let status = call(&root, "acp_cursor_auth_status", json!({"apiKey": "sk-secret"})).unwrap();
        assert_eq!(status["installed"], false);
        assert_eq!(status["is_authenticated"], false);
        assert!(status["email"].is_null());
        assert!(status["error"].as_str().unwrap().contains("cursor-agent"));

        let models = call(&root, "acp_cursor_list_models", json!({})).unwrap();
        assert_eq!(models["models"].as_array().unwrap().len(), 0);
        assert!(models["default_model"].is_null());
        assert!(models["error"].is_string());
    }

    #[test]
    fn qoder_status_returns_limited_empty_shape() {
        let root = temp_root("qoder");
        let status = call(&root, "acp_qoder_auth_status", json!({"personalAccessToken": "pat"})).unwrap();
        assert_eq!(status["installed"], false);
        assert_eq!(status["logged_in"], false);
        for field in ["username", "email", "user_type", "version", "allow_byok", "binary_path"] {
            assert!(status[field].is_null(), "{field} should be null");
        }
        assert!(status["error"].as_str().unwrap().contains("qoder"));
    }

    #[test]
    fn fetch_latest_release_parses_version_from_local_stub() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap().to_string();
        std::thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                let mut stream = stream;
                let mut buffer = [0u8; 4096];
                let _ = stream.read(&mut buffer);
                let body = r#"{"dist-tags":{"latest":"2.4.1"}}"#;
                let response = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    body.len(),
                    body
                );
                let _ = stream.write_all(response.as_bytes());
            }
        });
        let _guard = OverrideGuard::set("CODEPLUSE_ACP_RELEASE_URL", &format!("http://{address}/release"));
        let root = temp_root("release");
        let release = call(&root, "acp_fetch_agent_latest_release", json!({"agentType": "claude_code"})).unwrap();
        assert_eq!(release["version"], "2.4.1");
    }

    #[test]
    fn fetch_latest_release_is_null_without_configured_source_and_validates_input() {
        let root = temp_root("release-empty");
        std::env::remove_var("CODEPLUSE_ACP_RELEASE_URL");
        let release = call(&root, "acp_fetch_agent_latest_release", json!({"agentType": "pi"})).unwrap();
        assert!(release.is_null());
        assert!(call(&root, "acp_fetch_agent_latest_release", json!({}))
            .unwrap_err()
            .contains("agentType"));
    }

    #[test]
    fn install_uv_tool_validates_task_id_and_reports_limited_failure() {
        let root = temp_root("uv");
        assert!(call(&root, "acp_install_uv_tool", json!({}))
            .unwrap_err()
            .contains("taskId"));
        let err = call(&root, "acp_install_uv_tool", json!({"taskId": "task-1"})).unwrap_err();
        assert!(err.contains("未接入"));
    }

    #[test]
    fn pi_model_catalog_reports_failed_empty() {
        let root = temp_root("pi");
        let catalog = call(&root, "acp_list_pi_model_capabilities", json!({})).unwrap();
        assert_eq!(catalog["status"], "failed");
        assert_eq!(catalog["models"].as_array().unwrap().len(), 0);
    }

    #[test]
    fn deepseek_load_missing_file_is_inherited_not_error() {
        let root = temp_root("dsh-load");
        let home = temp_root("dsh-home-missing");
        let _guard = OverrideGuard::set("DSH_HOME", home.to_str().unwrap());
        let catalog = call(&root, "acp_load_deepseek_model_catalog", json!({})).unwrap();
        assert_eq!(catalog["exists"], false);
        assert_eq!(catalog["configured"], false);
        assert!(catalog["models"].as_array().unwrap().is_empty());
        assert!(catalog["error"].is_null());
    }

    #[test]
    fn deepseek_load_reads_stored_models_and_flags_invalid() {
        let root = temp_root("dsh-load-ok");
        let home = temp_root("dsh-home-ok");
        let _guard = OverrideGuard::set("DSH_HOME", home.to_str().unwrap());
        fs::create_dir_all(&home).unwrap();
        fs::write(
            home.join("settings.yaml"),
            "llm-deepseek:\n  models:\n    - id: deepseek-chat\n      name: Chat\n      contextWindow: 64000\n",
        )
        .unwrap();
        let catalog = call(&root, "acp_load_deepseek_model_catalog", json!({})).unwrap();
        assert_eq!(catalog["exists"], true);
        assert_eq!(catalog["configured"], true);
        assert_eq!(catalog["models"][0]["id"], "deepseek-chat");
        assert!(catalog["invalid"].is_null());

        // 重复 id → invalid 置位，但文件仍可被理解。
        fs::write(
            home.join("settings.yaml"),
            "llm-deepseek:\n  models:\n    - id: a\n    - id: a\n",
        )
        .unwrap();
        let catalog = call(&root, "acp_load_deepseek_model_catalog", json!({})).unwrap();
        assert!(catalog["invalid"].as_str().unwrap().contains("重复"));
    }

    #[test]
    fn deepseek_load_reports_parse_error() {
        let root = temp_root("dsh-load-bad");
        let home = temp_root("dsh-home-bad");
        let _guard = OverrideGuard::set("DSH_HOME", home.to_str().unwrap());
        fs::create_dir_all(&home).unwrap();
        fs::write(home.join("settings.yaml"), "llm-deepseek: [\n  - :\n").unwrap();
        let catalog = call(&root, "acp_load_deepseek_model_catalog", json!({})).unwrap();
        assert!(catalog["error"].as_str().unwrap().contains("解析失败"));
    }

    #[test]
    fn deepseek_update_writes_catalog_and_removes_on_null() {
        let root = temp_root("dsh-write");
        let home = temp_root("dsh-home-write");
        let _guard = OverrideGuard::set("DSH_HOME", home.to_str().unwrap());
        let models = json!([
            {"id": "deepseek-chat", "name": "Chat", "contextWindow": 64000, "inputModalities": ["text", "image"]},
            {"id": "deepseek-reasoner", "contextWindow": 32000}
        ]);
        call(
            &root,
            "acp_update_deepseek_model_catalog",
            json!({"models": models}),
        )
        .unwrap();
        let catalog = call(&root, "acp_load_deepseek_model_catalog", json!({})).unwrap();
        assert_eq!(catalog["models"].as_array().unwrap().len(), 2);

        // null 移除 models 键，回到内置继承。
        call(&root, "acp_update_deepseek_model_catalog", json!({"models": null})).unwrap();
        let catalog = call(&root, "acp_load_deepseek_model_catalog", json!({})).unwrap();
        assert_eq!(catalog["configured"], false);
    }

    #[test]
    fn deepseek_update_rejects_invalid_entries_before_writing() {
        let root = temp_root("dsh-write-bad");
        let home = temp_root("dsh-home-write-bad");
        let _guard = OverrideGuard::set("DSH_HOME", home.to_str().unwrap());
        assert!(call(
            &root,
            "acp_update_deepseek_model_catalog",
            json!({"models": [{"id": "x", "contextWindow": 0}]})
        )
        .unwrap_err()
        .contains("contextWindow"));
        assert!(call(
            &root,
            "acp_update_deepseek_model_catalog",
            json!({"models": [{"id": "x"}, {"id": "x"}]})
        )
        .unwrap_err()
        .contains("重复"));
        assert!(call(
            &root,
            "acp_update_deepseek_model_catalog",
            json!({"models": "not-an-array"})
        )
        .unwrap_err()
        .contains("数组"));
        assert!(!home.join("settings.yaml").exists(), "非法条目不得写盘");
    }

    #[test]
    fn scan_leaked_temp_lists_only_project_prefixes() {
        let root = temp_root("scan");
        let _guard = OverrideGuard::set("CODEPLUSE_LEAKED_TEMP_ROOT", root.to_str().unwrap());
        let ours_old = root.join("acp-persist-abc123.bin");
        fs::write(&ours_old, vec![0u8; 1024]).unwrap();
        // 回溯到两小时前，越过“太新可能仍在使用”的 1 小时跳过窗口；
        // SetFileTime 需要写访问句柄，否则 Windows 拒绝。
        let backdated = SystemTime::now() - std::time::Duration::from_secs(7200);
        fs::OpenOptions::new()
            .write(true)
            .open(&ours_old)
            .unwrap()
            .set_modified(backdated)
            .unwrap();
        let foreign = root.join("tmp-foreign-xyz");
        fs::create_dir_all(&foreign).unwrap();
        // 太新的本项目目录进 skipped。
        let fresh = root.join("acp-splits-fresh");
        fs::create_dir_all(&fresh).unwrap();

        let view = call(&root, "acp_scan_leaked_temp", json!({})).unwrap();
        assert_eq!(view["root"].as_str().unwrap(), root.to_string_lossy());
        let entries = view["entries"].as_array().unwrap();
        assert_eq!(entries.len(), 1);
        assert!(entries[0]["path"].as_str().unwrap().contains("acp-persist-abc123"));
        assert_eq!(entries[0]["bytes"], 1024);
        assert!(!entries[0]["is_dir"].as_bool().unwrap());
        assert_eq!(view["skipped"].as_u64().unwrap(), 1);
        assert_eq!(view["total_bytes"].as_u64().unwrap(), 1024);
    }

    #[test]
    fn reclaim_leaked_temp_deletes_only_guarded_paths() {
        let root = temp_root("reclaim");
        let _guard = OverrideGuard::set("CODEPLUSE_LEAKED_TEMP_ROOT", root.to_str().unwrap());
        let ours = root.join("acp-storeconv-deadbeef");
        fs::create_dir_all(&ours).unwrap();
        fs::write(ours.join("a.bin"), vec![0u8; 512]).unwrap();
        let foreign = root.join("tmp-foreign-keep");
        fs::create_dir_all(&foreign).unwrap();

        let view = call(
            &root,
            "acp_reclaim_leaked_temp",
            json!({"paths": [
                ours.to_string_lossy().to_string(),
                foreign.to_string_lossy().to_string(),
                "C:\\Windows\\System32\\config",
                42
            ]}),
        )
        .unwrap();
        assert_eq!(view["removed"].as_u64().unwrap(), 1);
        assert_eq!(view["freed_bytes"].as_u64().unwrap(), 512);
        let failed = view["failed"].as_array().unwrap();
        assert_eq!(failed.len(), 3);
        assert!(!ours.exists());
        assert!(foreign.exists(), "未知前缀路径不得删除");
    }

    #[test]
    fn reclaim_leaked_temp_requires_paths_array() {
        let root = temp_root("reclaim-bad");
        assert!(call(&root, "acp_reclaim_leaked_temp", json!({}))
            .unwrap_err()
            .contains("paths"));
    }

    #[test]
    fn claude_picker_normalizes_deduplicates_and_persists() {
        let root = temp_root("picker");
        let count = call(
            &root,
            "acp_update_claude_model_picker",
            json!({"models": ["sonnet", " sonnet ", "opus", ""]}),
        )
        .unwrap();
        assert_eq!(count, json!(0));
        let doc = read_store(&data_dir(&root).join("claude-model-picker.json"));
        let models = doc["models"].as_array().unwrap();
        assert_eq!(models.len(), 2);
        assert_eq!(models[0], "sonnet");
        assert_eq!(models[1], "opus");
    }

    #[test]
    fn claude_picker_rejects_non_string_entries() {
        let root = temp_root("picker-bad");
        assert!(call(
            &root,
            "acp_update_claude_model_picker",
            json!({"models": ["ok", 5]})
        )
        .unwrap_err()
        .contains("非空字符串"));
        assert!(call(&root, "acp_update_claude_model_picker", json!({}))
            .unwrap_err()
            .contains("models"));
    }

    #[test]
    fn kimi_raw_mode_writes_verbatim_and_seeds_gate_token() {
        let root = temp_root("kimi-raw");
        let home = temp_root("kimi-home-raw");
        let _guard = OverrideGuard::set("KIMI_CODE_HOME", home.to_str().unwrap());
        let toml = "[provider]\ntype = \"openai\"\nmodel = \"kimi-k2\"\n";
        call(
            &root,
            "acp_update_kimi_code_config",
            json!({"mode": "raw", "rawConfigToml": toml}),
        )
        .unwrap();
        let written = fs::read_to_string(home.join("config.toml")).unwrap();
        assert!(written.contains("model = \"kimi-k2\""));
        assert!(kimi_gate_token_path().is_file());
    }

    #[test]
    fn kimi_apikey_mode_merges_managed_provider_block() {
        let root = temp_root("kimi-apikey");
        let home = temp_root("kimi-home-apikey");
        let _guard = OverrideGuard::set("KIMI_CODE_HOME", home.to_str().unwrap());
        call(
            &root,
            "acp_update_kimi_code_config",
            json!({
                "mode": "apikey",
                "interfaceType": "openai",
                "authType": "api_key",
                "baseUrl": "https://api.moonshot.cn/v1",
                "apiKey": "sk-kimi",
                "model": "kimi-k2",
                "maxContextSize": 128000,
                "reasoningEnabled": true,
                "supportEfforts": ["low", "high"]
            }),
        )
        .unwrap();
        let doc: toml::Value = toml::from_str(&fs::read_to_string(home.join("config.toml")).unwrap()).unwrap();
        let provider = doc["provider"].as_table().unwrap();
        assert_eq!(provider["type"].as_str().unwrap(), "openai");
        assert_eq!(provider["api_key"].as_str().unwrap(), "sk-kimi");
        assert_eq!(provider["max_context_size"].as_integer().unwrap(), 128000);
        assert_eq!(provider["reasoning_enabled"].as_bool().unwrap(), true);
        assert_eq!(provider["support_efforts"].as_array().unwrap().len(), 2);
        assert!(kimi_gate_token_path().is_file());
    }

    #[test]
    fn kimi_login_mode_clears_managed_block_and_token() {
        let root = temp_root("kimi-login");
        let home = temp_root("kimi-home-login");
        let _guard = OverrideGuard::set("KIMI_CODE_HOME", home.to_str().unwrap());
        call(
            &root,
            "acp_update_kimi_code_config",
            json!({"mode": "apikey", "apiKey": "sk-x", "model": "m"}),
        )
        .unwrap();
        call(&root, "acp_update_kimi_code_config", json!({"mode": "login"})).unwrap();
        let doc: toml::Value = toml::from_str(&fs::read_to_string(home.join("config.toml")).unwrap()).unwrap();
        assert!(doc.get("provider").is_none());
        assert!(!kimi_gate_token_path().exists());
    }

    #[test]
    fn kimi_config_validates_mode_and_toml() {
        let root = temp_root("kimi-bad");
        let home = temp_root("kimi-home-bad");
        let _guard = OverrideGuard::set("KIMI_CODE_HOME", home.to_str().unwrap());
        assert!(call(&root, "acp_update_kimi_code_config", json!({}))
            .unwrap_err()
            .contains("mode"));
        assert!(call(
            &root,
            "acp_update_kimi_code_config",
            json!({"mode": "magic"})
        )
        .unwrap_err()
        .contains("mode 无效"));
        assert!(call(
            &root,
            "acp_update_kimi_code_config",
            json!({"mode": "raw", "rawConfigToml": "= not toml"})
        )
        .unwrap_err()
        .contains("TOML"));
        assert!(call(
            &root,
            "acp_update_kimi_code_config",
            json!({"mode": "raw"})
        )
        .unwrap_err()
        .contains("rawConfigToml"));
    }

    #[test]
    fn system_agent_update_records_request_and_returns_null() {
        let root = temp_root("sys-agent");
        let result = call(
            &root,
            "acp_update_system_agent",
            json!({"agentType": "claude_code", "taskId": "task-7"}),
        )
        .unwrap();
        assert!(result.is_null());
        let doc = read_store(&data_dir(&root).join("agent-updates.json"));
        let updates = doc["updates"].as_array().unwrap();
        assert_eq!(updates.len(), 1);
        assert_eq!(updates[0]["agentType"], "claude_code");
        assert_eq!(updates[0]["taskId"], "task-7");
    }

    #[test]
    fn system_agent_update_validates_input() {
        let root = temp_root("sys-agent-bad");
        assert!(call(&root, "acp_update_system_agent", json!({"taskId": "t"}))
            .unwrap_err()
            .contains("agentType"));
        assert!(call(
            &root,
            "acp_update_system_agent",
            json!({"agentType": "pi"})
        )
        .unwrap_err()
        .contains("taskId"));
    }

    #[test]
    fn unknown_command_falls_through_to_next_module() {
        assert!(handle(&temp_root("fallthrough"), "acp_prompt", &json!({})).is_none());
    }
}
