//! 自定义智能体的只读面：`server-go/internal/codeg/custom_agents.go` 里
//! 不碰 hub 注册表的 2 条命令。
//!
//! - `acp_current_platform`：平台标识（GOOS-GOARCH）。
//! - `acp_list_custom_agents`：读 Store.CustomAgents 并做可启动性校验。
//!
//! `acp_fetch_registry_catalog` 已由 acp_registry.rs 翻转（B 组）。
//!
//! 写命令（收尾批）：`acp_save_custom_agent` / `acp_delete_custom_agent` /
//! `acp_add_registry_agent`。Go 侧同步的是内存 Hub 注册表与热身连接池；Rust
//! 的注册表按 `.data/codeg-workspace.json` 的 customAgents 每次无状态重建
//! （acp::agents），所以写路径只需落盘 + 事件。
//!
//! 校验逻辑按 validateCustomDef 逐条复刻（ID 正则手写展开，不引 regex 依赖）。

use std::path::Path;

use serde_json::{json, Value};

use crate::backend::store_file;

pub fn owns(command: &str) -> bool {
    matches!(
        command,
        "acp_current_platform"
            | "acp_list_custom_agents"
            | "acp_save_custom_agent"
            | "acp_delete_custom_agent"
            | "acp_add_registry_agent"
    )
}

pub fn handle(root: &Path, command: &str, args: &Value) -> Option<Result<Value, String>> {
    if !owns(command) {
        return None;
    }
    Some(run(root, command, args))
}

/// Go `currentACPPlatform`。
fn current_platform() -> String {
    let arch = match std::env::consts::ARCH {
        "x86_64" => "x86_64",
        "aarch64" => "aarch64",
        "x86" => "x86",
        other => other,
    };
    format!("{}-{}", std::env::consts::OS, arch)
}

const BUILTIN_REGISTRY_IDS: [&str; 6] =
    ["codex-acp", "claude-acp", "claude-code-acp", "deepseek", "dsh", "workbuddy"];

fn builtin_registry_id(id: &str) -> bool {
    BUILTIN_REGISTRY_IDS.contains(&id)
}

/// Go `customKindRunsHere`。
fn kind_runs_here(spec: &Value, kind: &str, platform: &str) -> bool {
    match kind {
        "binary" => spec
            .get("binary")
            .and_then(|b| b.get(platform))
            .and_then(|b| b.get("archive"))
            .and_then(|a| a.as_str())
            .map(|a| !a.is_empty())
            .unwrap_or(false),
        "npx" => spec
            .get("npx")
            .and_then(|n| n.get("package"))
            .and_then(|p| p.as_str())
            .map(|p| !p.is_empty())
            .unwrap_or(false),
        "uvx" => spec
            .get("uvx")
            .and_then(|n| n.get("package"))
            .and_then(|p| p.as_str())
            .map(|p| !p.is_empty())
            .unwrap_or(false),
        _ => false,
    }
}

/// Go `validateCustomDef`：返回 None 表示可启动，Some 是 problem 文案。
pub(crate) fn validate_custom_def(def: &Value, platform: &str) -> Option<String> {
    let id = def.get("registryId").and_then(|v| v.as_str()).unwrap_or("");
    let valid_id = !id.is_empty()
        && id.as_bytes()[0].is_ascii_alphanumeric()
        && id.len() <= 128
        && id
            .bytes()
            .skip(1)
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'.' || b == b'-');
    if !valid_id || builtin_registry_id(id) {
        return Some("自定义智能体 ID 无效或与内置执行端冲突".into());
    }
    let kind = def.get("distributionKind").and_then(|v| v.as_str()).unwrap_or("");
    let spec = def.get("spec").cloned().unwrap_or(json!({}));
    if !kind_runs_here(&spec, kind, platform) {
        return Some(format!("{id} 未提供适用于 {platform} 的 {kind} 发行版"));
    }
    if kind == "binary" {
        let binary = spec
            .get("binary")
            .and_then(|b| b.get(platform))
            .cloned()
            .unwrap_or(json!({}));
        let archive = binary.get("archive").and_then(|a| a.as_str()).unwrap_or("");
        if !valid_http_url(archive) {
            return Some("归档下载地址必须为 HTTP(S) URL".into());
        }
        let command = binary.get("cmd").and_then(|c| c.as_str()).unwrap_or("");
        if let Err(err) = safe_custom_path(command) {
            return Some(format!("归档启动路径无效: {err}"));
        }
        let sha256 = binary.get("sha256").and_then(|s| s.as_str()).unwrap_or("");
        if !sha256.is_empty() && !(sha256.len() == 64 && sha256.bytes().all(|b| b.is_ascii_hexdigit()))
        {
            return Some("SHA-256 校验值必须是 64 位十六进制字符串".into());
        }
    } else {
        let package = match kind {
            "uvx" => spec.get("uvx").and_then(|u| u.get("package")),
            _ => spec.get("npx").and_then(|u| u.get("package")),
        }
        .and_then(|p| p.as_str())
        .unwrap_or("");
        if package.starts_with('-')
            || package.contains('\r')
            || package.contains('\n')
            || package.contains('\0')
        {
            return Some("包名称无效".into());
        }
    }
    None
}

/// Go `safeCustomPath` 的校验部分（只需要 error 语义）。
fn safe_custom_path(entry: &str) -> Result<(), String> {
    let entry = entry.replace('\\', "/");
    if entry.is_empty() || entry.contains(':') || entry.contains('\0') || entry.starts_with('/') {
        return Err(format!("不安全的归档路径：{entry:?}"));
    }
    for part in entry.split('/') {
        if part == ".." || part.ends_with(' ') || (part != "." && part.ends_with('.')) {
            return Err(format!("不安全的归档路径：{entry:?}"));
        }
    }
    // filepath.Clean(FromSlash(entry))：去 "./"，合并 "//"。
    let mut out: Vec<&str> = Vec::new();
    for part in entry.split('/').filter(|p| !p.is_empty() && *p != ".") {
        if part == ".." {
            if out.pop().is_none() {
                return Err(format!("归档路径越界：{entry:?}"));
            }
        } else {
            out.push(part);
        }
    }
    if out.is_empty() {
        return Err(format!("归档路径越界：{entry:?}"));
    }
    Ok(())
}

/// Go `url.Parse` 的形状检查：scheme http/https 且 host 非空。
fn valid_http_url(raw: &str) -> bool {
    if raw.contains(|c: char| c.is_control()) {
        return false;
    }
    let Some((scheme, rest)) = raw.split_once("://") else {
        return false;
    };
    if scheme != "https" && scheme != "http" {
        return false;
    }
    let host = rest.split(['/', '?', '#']).next().unwrap_or("");
    !host.is_empty()
}

fn custom_agents_doc(root: &Path) -> Value {
    store_file::read_json(&root.join(".data").join("codeg-workspace.json"))
        .ok()
        .flatten()
        .and_then(|doc| doc.get("customAgents").cloned())
        .unwrap_or(json!({}))
}

/// Go `customAgentSpec` 的 marshal 形状（未知键丢弃；空 map 键省略）。
fn normalize_spec(v: &Value) -> Value {
    // Go 的 customPackageSpec 每个可选字段都带 omitempty：空串/空数组/空 map
    // 不落盘。Rust 透传原样会把 `"args":[]` 这类空值写进文件。
    let non_empty = |item: &Value| match item {
        Value::String(text) => !text.is_empty(),
        Value::Array(list) => !list.is_empty(),
        Value::Object(map) => !map.is_empty(),
        Value::Null => false,
        _ => true,
    };
    let package = |raw: &Value| {
        let mut out = serde_json::Map::new();
        out.insert("package".into(), raw.get("package").cloned().unwrap_or(json!("")));
        for key in ["cmd", "args", "env", "python", "nodeRequired", "uvRequired"] {
            if let Some(item) = raw.get(key).filter(|i| non_empty(i)) {
                out.insert(key.into(), item.clone());
            }
        }
        Value::Object(out)
    };
    let mut out = serde_json::Map::new();
    if let Some(npx) = v.get("npx").filter(|x| x.is_object()) {
        out.insert("npx".into(), package(npx));
    }
    if let Some(uvx) = v.get("uvx").filter(|x| x.is_object()) {
        out.insert("uvx".into(), package(uvx));
    }
    if let Some(binary) = v.get("binary").and_then(|b| b.as_object()) {
        if !binary.is_empty() {
            let mut platforms = serde_json::Map::new();
            for (platform, raw) in binary {
                let mut entry = serde_json::Map::new();
                entry.insert("archive".into(), raw.get("archive").cloned().unwrap_or(json!("")));
                entry.insert("cmd".into(), raw.get("cmd").cloned().unwrap_or(json!("")));
                for key in ["args", "env", "sha256"] {
                    if let Some(item) = raw.get(key).filter(|i| non_empty(i)) {
                        entry.insert(key.into(), item.clone());
                    }
                }
                platforms.insert(platform.clone(), Value::Object(entry));
            }
            out.insert("binary".into(), Value::Object(platforms));
        }
    }
    Value::Object(out)
}

/// Go customAgentDef 经 `json.Marshal` → `json.Unmarshal` 进 Row 后的键集：
/// 无 omitempty 的字段永远存在（指针缺失 → null，字符串缺失 → ""）。
fn def_row(map_key: &str, def: &Value) -> Value {
    let s = |key: &str| def.get(key).and_then(|v| v.as_str()).unwrap_or("").to_string();
    let nullable = |key: &str| def.get(key).cloned().unwrap_or(Value::Null);
    let _ = map_key; // Go 的 registryId/agentType 取结构体字段，不用 map 键。
    json!({
        "registryId": s("registryId"),
        "name": s("name"),
        "description": s("description"),
        "version": s("version"),
        "distributionKind": s("distributionKind"),
        "spec": normalize_spec(def.get("spec").unwrap_or(&json!({}))),
        "iconUrl": nullable("iconUrl"),
        "skillsSharedStore": def.get("skillsSharedStore").and_then(|v| v.as_bool()).unwrap_or(false),
        "skillsDir": nullable("skillsDir"),
        "source": s("source"),
        "versionProbe": nullable("versionProbe"),
        "supportsMcp": def.get("supportsMcp").and_then(|v| v.as_bool()).unwrap_or(false),
        "agentType": format!("custom:{}", s("registryId")),
    })
}

fn run(root: &Path, command: &str, args: &Value) -> Result<Value, String> {
    match command {
        "acp_current_platform" => Ok(json!(current_platform())),
        "acp_list_custom_agents" => {
            let installed = custom_agents_doc(root);
            let platform = current_platform();
            let mut rows: Vec<Value> = Vec::new();
            if let Some(map) = installed.as_object() {
                for (id, def) in map {
                    // Go：json.Marshal(customAgentDef) → Unmarshal → Row。结构体
                    // 字段没有 omitempty（除 spec 内层），所以缺省键要按 Go 的
                    // 零值语义补齐，再叠 agentType / launchable / problem。
                    let mut row = def_row(id, def);
                    if let Some(obj) = row.as_object_mut() {
                        match validate_custom_def(def, &platform) {
                            None => {
                                obj.insert("launchable".into(), json!(true));
                                obj.insert("problem".into(), Value::Null);
                            }
                            Some(problem) => {
                                obj.insert("launchable".into(), json!(false));
                                obj.insert("problem".into(), json!(problem));
                            }
                        }
                    }
                    rows.push(row);
                }
            }
            rows.sort_by(|a, b| {
                let ka = a.get("registryId").and_then(|v| v.as_str()).unwrap_or("");
                let kb = b.get("registryId").and_then(|v| v.as_str()).unwrap_or("");
                ka.cmp(kb)
            });
            Ok(Value::Array(rows))
        }
        "acp_save_custom_agent" => {
            let params = args
                .get("params")
                .filter(|v| v.is_object())
                .ok_or_else(|| "智能体配置无效: 缺少 params".to_string())?;
            save_custom_agent(root, params)
        }
        "acp_delete_custom_agent" => delete_custom_agent(root, args),
        "acp_add_registry_agent" => add_registry_agent(root, args),
        _ => Err(format!("未知自定义智能体命令：{command}")),
    }
}

// ── 写命令（custom_agents.go saveCustomAgent / delete / add_registry）──────

fn workspace_path(root: &Path) -> std::path::PathBuf {
    root.join(".data").join("codeg-workspace.json")
}

fn read_doc(root: &Path) -> Result<Value, String> {
    Ok(match store_file::read_json(&workspace_path(root))? {
        Some(doc) if doc.is_object() => doc,
        _ => json!({"next": 1}),
    })
}

fn persist_doc(root: &Path, doc: &Value) -> Result<(), String> {
    let bytes = crate::backend::goformat::marshal_store(doc);
    let path = workspace_path(root);
    let _guard = store_file::lock(&path)?;
    store_file::write_bytes(&path, &bytes)
}

/// customAgentDef 的 Go 结构体序（无 omitempty 的字段也要补齐零值）。
fn def_in_go_shape(source: &Value) -> Value {
    let text = |key: &str| {
        source
            .get(key)
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string()
    };
    json!({
        "registryId": text("registryId"),
        "name": text("name"),
        "description": text("description"),
        "version": text("version"),
        "distributionKind": text("distributionKind"),
        "spec": normalize_spec(source.get("spec").unwrap_or(&Value::Null)),
        "iconUrl": source.get("iconUrl").cloned().unwrap_or(Value::Null),
        "skillsSharedStore": source
            .get("skillsSharedStore")
            .and_then(|v| v.as_bool())
            .unwrap_or(false),
        "skillsDir": source.get("skillsDir").cloned().unwrap_or(Value::Null),
        "source": text("source"),
        "versionProbe": source.get("versionProbe").cloned().unwrap_or(Value::Null),
        "supportsMcp": source
            .get("supportsMcp")
            .and_then(|v| v.as_bool())
            .unwrap_or(false),
    })
}

/// Go saveCustomAgent：校验 → 技能目录归一 → 来源/supportsMcp 继承 → 落盘。
fn save_custom_agent(root: &Path, params: &Value) -> Result<Value, String> {
    let mut doc = read_doc(root)?;
    let custom = doc
        .pointer("/customAgents")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    let platform = current_platform();
    let mut def = def_in_go_shape(params);
    if def["distributionKind"].as_str().unwrap_or("").is_empty() {
        def["distributionKind"] = json!(default_custom_kind(&def["spec"], &platform));
    }
    // RegistryID / Name 去空白；Name 空则用 RegistryID。
    let registry_id = def["registryId"].as_str().unwrap_or("").trim().to_string();
    def["registryId"] = json!(registry_id);
    let mut name = def["name"].as_str().unwrap_or("").trim().to_string();
    if name.is_empty() {
        name = registry_id.clone();
    }
    def["name"] = json!(name);
    if let Some(problem) = validate_custom_def(&def, &platform) {
        return Err(problem);
    }
    let old = custom.get(&registry_id).cloned();
    // skillsDir：~/ 展开，空置 null，相对路径拒绝。
    if let Some(dir) = def.get("skillsDir").and_then(Value::as_str).map(str::to_string) {
        let mut dir = dir.trim().to_string();
        if dir.starts_with("~/") || dir.starts_with("~\\") {
            let home = std::env::var("USERPROFILE")
                .or_else(|_| std::env::var("HOME"))
                .unwrap_or_default();
            dir = std::path::Path::new(&home)
                .join(&dir[2..])
                .to_string_lossy()
                .to_string();
        }
        if dir.is_empty() {
            def["skillsDir"] = Value::Null;
        } else if !std::path::Path::new(&dir).is_absolute() {
            return Err("技能目录必须为绝对路径".into());
        } else {
            def["skillsDir"] = json!(dir);
        }
    }
    // supportsMcp：请求里没有该键时默认 true（已有配置沿用旧值）。
    if params.get("supportsMcp").is_none() {
        let inherited = old
            .as_ref()
            .and_then(|o| o.get("supportsMcp"))
            .cloned()
            .unwrap_or(json!(true));
        def["supportsMcp"] = inherited;
    }
    // source：空 → manual（已有配置沿用旧值）。
    if def["source"].as_str().unwrap_or("").is_empty() {
        let inherited = old
            .as_ref()
            .and_then(|o| o.get("source"))
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .unwrap_or("manual")
            .to_string();
        def["source"] = json!(inherited);
    }
    let source = def["source"].as_str().unwrap_or("");
    if source != "manual" && source != "registry" {
        return Err("智能体来源无效".into());
    }
    let custom_agents = doc
        .as_object_mut()
        .unwrap()
        .entry("customAgents")
        .or_insert_with(|| json!({}));
    if !custom_agents.is_object() {
        *custom_agents = json!({});
    }
    custom_agents
        .as_object_mut()
        .unwrap()
        .insert(registry_id.clone(), def);
    persist_doc(root, &doc)?;
    crate::backend::emit_event("app://acp-agents-updated", json!({}));
    Ok(Value::Null)
}

fn delete_custom_agent(root: &Path, args: &Value) -> Result<Value, String> {
    if args.get("deleteTranscripts") == Some(&Value::Bool(true)) {
        return Err(
            "Go 版暂不支持清除自定义智能体的历史记录；取消勾选后可移除定义并保留会话".into(),
        );
    }
    let registry_id = args
        .get("registryId")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    let mut doc = read_doc(root)?;
    let Some(custom) = doc
        .pointer_mut("/customAgents")
        .and_then(Value::as_object_mut)
    else {
        return Err("自定义智能体不存在".into());
    };
    if custom.remove(&registry_id).is_none() {
        return Err("自定义智能体不存在".into());
    }
    persist_doc(root, &doc)?;
    crate::backend::emit_event("app://acp-agents-updated", json!({}));
    Ok(Value::Null)
}

fn add_registry_agent(root: &Path, args: &Value) -> Result<Value, String> {
    let registry_id = args
        .get("registryId")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    let entries = crate::backend::acp_registry::fetch_entries()?;
    for entry in entries {
        let id = entry.get("id").and_then(Value::as_str).unwrap_or("");
        if id != registry_id {
            continue;
        }
        let exists = read_doc(root)?
            .pointer("/customAgents")
            .and_then(Value::as_object)
            .map(|map| map.contains_key(id))
            .unwrap_or(false);
        if exists {
            return Err(format!("智能体已添加：{id}"));
        }
        let spec = entry.get("distribution").cloned().unwrap_or(Value::Null);
        let mut kind = args
            .get("distributionKind")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        if kind.is_empty() {
            kind = default_custom_kind(&spec, &current_platform());
        }
        let params = json!({
            "registryId": id,
            "name": entry.get("name").cloned().unwrap_or(Value::Null),
            "description": entry.get("description").cloned().unwrap_or(Value::Null),
            "version": entry.get("version").cloned().unwrap_or(Value::Null),
            "distributionKind": kind,
            "spec": spec,
            "iconUrl": entry.get("icon").cloned().unwrap_or(Value::Null),
            "source": "registry",
            "supportsMcp": true,
        });
        return save_custom_agent(root, &params);
    }
    Err(format!("ACP 注册表中未找到 {registry_id}"))
}

/// Go defaultCustomKind：第一个能在本机跑的发行方式。
fn default_custom_kind(spec: &Value, platform: &str) -> String {
    let mut kinds: Vec<&str> = Vec::new();
    if spec
        .get("binary")
        .and_then(Value::as_object)
        .map(|m| !m.is_empty())
        .unwrap_or(false)
    {
        kinds.push("binary");
    }
    if spec.get("npx").map(Value::is_object).unwrap_or(false) {
        kinds.push("npx");
    }
    if spec.get("uvx").map(Value::is_object).unwrap_or(false) {
        kinds.push("uvx");
    }
    for kind in kinds {
        if kind_runs_here(spec, kind, platform) {
            return kind.to_string();
        }
    }
    String::new()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_root(tag: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "codepluse-custom-agents-{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(dir.join(".data")).unwrap();
        dir
    }

    #[test]
    fn platform_matches_go_mapping() {
        // GOOS windows + GOARCH amd64 → windows-x86_64。
        assert_eq!(current_platform(), format!("{}-{}", std::env::consts::OS, "x86_64"));
    }

    #[test]
    fn validation_mirrors_go() {
        let platform = current_platform();
        // 与内置 ID 冲突。
        let builtin = json!({"registryId": "dsh", "distributionKind": "npx",
            "spec": {"npx": {"package": "foo"}}});
        assert_eq!(
            validate_custom_def(&builtin, &platform).as_deref(),
            Some("自定义智能体 ID 无效或与内置执行端冲突")
        );
        // 平台缺失发行版。
        let missing = json!({"registryId": "myagent", "distributionKind": "binary",
            "spec": {"binary": {}}});
        assert!(validate_custom_def(&missing, &platform)
            .unwrap_or_default()
            .contains("未提供适用于"));
        // 合法 npx 定义。
        let ok = json!({"registryId": "myagent", "distributionKind": "npx",
            "spec": {"npx": {"package": "@x/y"}}});
        assert_eq!(validate_custom_def(&ok, &platform), None);
        // 包名以 - 开头。
        let bad = json!({"registryId": "myagent", "distributionKind": "npx",
            "spec": {"npx": {"package": "-x"}}});
        assert_eq!(validate_custom_def(&bad, &platform).as_deref(), Some("包名称无效"));
    }

    #[test]
    fn id_pattern_matches_go_regexp() {
        let platform = current_platform();
        let mk = |id: &str| json!({"registryId": id, "distributionKind": "npx", "spec": {"npx": {"package": "p"}}});
        assert_eq!(validate_custom_def(&mk("a_b.c-1"), &platform), None);
        // 首字符必须字母数字。
        assert!(validate_custom_def(&mk("_x"), &platform).is_some());
        // 非 ASCII 不匹配 Go 的 ^[a-zA-Z0-9…]$。
        assert!(validate_custom_def(&mk("智能体"), &platform).is_some());
    }

    #[test]
    fn url_check_needs_scheme_and_host() {
        assert!(valid_http_url("https://example.com/a.zip"));
        assert!(valid_http_url("http://example.com"));
        assert!(!valid_http_url("example.com/a.zip"));
        assert!(!valid_http_url("ftp://example.com"));
        assert!(!valid_http_url("https://"));
    }
    #[test]
    fn save_delete_round_trip_in_go_shape() {
        let root = temp_root("save");
        // npx 发行（本机一定能跑 npx 分支的判定只看 package 非空）。
        let params = json!({
            "registryId": "demo-agent",
            "name": "  Demo  ",
            "description": "d",
            "version": "1.0.0",
            "spec": {"npx": {"package": "@demo/agent", "args": []}},
            "iconUrl": "https://x/i.png",
            "skillsSharedStore": true,
            "source": "manual",
            "supportsMcp": true,
        });
        handle(&root, "acp_save_custom_agent", &json!({"params": params}))
            .unwrap()
            .unwrap();
        let doc = store_file::read_json(&root.join(".data").join("codeg-workspace.json"))
            .unwrap()
            .unwrap();
        let def = &doc["customAgents"]["demo-agent"];
        assert_eq!(def["name"], "Demo", "名称去空白");
        assert_eq!(def["distributionKind"], "npx", "缺省 kind 由 spec 推断");
        assert_eq!(def["source"], "manual");
        assert_eq!(def["skillsDir"], Value::Null);
        assert_eq!(def["versionProbe"], Value::Null);
        // 空 args 被 omitempty 丢弃（Go customAgentSpec）。
        assert!(def["spec"]["npx"].get("args").is_none(), "{def}");

        // list 能看到且 launchable。
        let rows = handle(&root, "acp_list_custom_agents", &json!({}))
            .unwrap()
            .unwrap();
        let row = &rows.as_array().unwrap()[0];
        assert_eq!(row["registryId"], "demo-agent");
        assert_eq!(row["launchable"], true);

        // 删除。
        handle(&root, "acp_delete_custom_agent", &json!({"registryId": "demo-agent"}))
            .unwrap()
            .unwrap();
        let doc = store_file::read_json(&root.join(".data").join("codeg-workspace.json"))
            .unwrap()
            .unwrap();
        assert!(doc["customAgents"].get("demo-agent").is_none());
        assert_eq!(
            handle(&root, "acp_delete_custom_agent", &json!({"registryId": "demo-agent"}))
                .unwrap()
                .unwrap_err(),
            "自定义智能体不存在"
        );
    }

    #[test]
    fn save_validates_and_inherits() {
        let root = temp_root("save-rules");
        // 非法 ID。
        let err = handle(
            &root,
            "acp_save_custom_agent",
            &json!({"params": {"registryId": "../bad", "spec": {"npx": {"package": "p"}}}}),
        )
        .unwrap()
        .unwrap_err();
        assert_eq!(err, "自定义智能体 ID 无效或与内置执行端冲突");
        // 与内置冲突。
        let err = handle(
            &root,
            "acp_save_custom_agent",
            &json!({"params": {"registryId": "codex-acp", "spec": {"npx": {"package": "p"}}}}),
        )
        .unwrap()
        .unwrap_err();
        assert_eq!(err, "自定义智能体 ID 无效或与内置执行端冲突");
        // 相对技能目录。
        let err = handle(
            &root,
            "acp_save_custom_agent",
            &json!({"params": {"registryId": "ok-agent", "spec": {"npx": {"package": "p"}},
                               "skillsDir": "relative/dir"}}),
        )
        .unwrap()
        .unwrap_err();
        assert_eq!(err, "技能目录必须为绝对路径");
        // 非法来源。
        let err = handle(
            &root,
            "acp_save_custom_agent",
            &json!({"params": {"registryId": "ok-agent", "spec": {"npx": {"package": "p"}},
                               "source": "weird"}}),
        )
        .unwrap()
        .unwrap_err();
        assert_eq!(err, "智能体来源无效");

        // 首次保存：source 默认 manual、supportsMcp 默认 true。
        handle(
            &root,
            "acp_save_custom_agent",
            &json!({"params": {"registryId": "ok-agent", "spec": {"npx": {"package": "p"}}}}),
        )
        .unwrap()
        .unwrap();
        let doc = store_file::read_json(&root.join(".data").join("codeg-workspace.json"))
            .unwrap()
            .unwrap();
        assert_eq!(doc["customAgents"]["ok-agent"]["source"], "manual");
        assert_eq!(doc["customAgents"]["ok-agent"]["supportsMcp"], true);
        // 二次保存未带 supportsMcp：沿用旧值（先改成 false 再保存）。
        let mut doc = doc;
        doc["customAgents"]["ok-agent"]["supportsMcp"] = json!(false);
        store_file::write_bytes(
            &root.join(".data").join("codeg-workspace.json"),
            &crate::backend::goformat::marshal_store(&doc),
        )
        .unwrap();
        handle(
            &root,
            "acp_save_custom_agent",
            &json!({"params": {"registryId": "ok-agent", "spec": {"npx": {"package": "p"}}}}),
        )
        .unwrap()
        .unwrap();
        let doc = store_file::read_json(&root.join(".data").join("codeg-workspace.json"))
            .unwrap()
            .unwrap();
        assert_eq!(doc["customAgents"]["ok-agent"]["supportsMcp"], false, "继承旧值");
    }

    #[test]
    fn delete_rejects_transcript_purge() {
        let root = temp_root("purge");
        assert_eq!(
            handle(
                &root,
                "acp_delete_custom_agent",
                &json!({"registryId": "x", "deleteTranscripts": true})
            )
            .unwrap()
            .unwrap_err(),
            "Go 版暂不支持清除自定义智能体的历史记录；取消勾选后可移除定义并保留会话"
        );
    }
}
