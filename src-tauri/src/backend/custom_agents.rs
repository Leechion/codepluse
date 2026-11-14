//! 自定义智能体的只读面：`server-go/internal/codeg/custom_agents.go` 里
//! 不碰 hub 注册表的 2 条命令。
//!
//! - `acp_current_platform`：平台标识（GOOS-GOARCH）。
//! - `acp_list_custom_agents`：读 Store.CustomAgents 并做可启动性校验。
//!
//! `acp_fetch_registry_catalog` 已由 acp_registry.rs 翻转（B 组）。
//! `acp_save_custom_agent` / `acp_add_registry_agent` / `acp_delete_custom_agent`
//! 仍留在 Go：它们要同步 hub 代理注册表（registerCustomAgent）和热身连接
//! 池（retireAgentConnections），hub 移到装配层时一起翻转。
//!
//! 校验逻辑按 validateCustomDef 逐条复刻（ID 正则手写展开，不引 regex 依赖）。

use std::path::Path;

use serde_json::{json, Value};

use crate::backend::store_file;

pub fn owns(command: &str) -> bool {
    matches!(command, "acp_current_platform" | "acp_list_custom_agents")
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
    let package = |raw: &Value| {
        let mut out = serde_json::Map::new();
        out.insert("package".into(), raw.get("package").cloned().unwrap_or(json!("")));
        for key in ["cmd", "args", "env", "python", "nodeRequired", "uvRequired"] {
            if let Some(item) = raw.get(key) {
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
                    if let Some(item) = raw.get(key) {
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

fn run(root: &Path, command: &str, _args: &Value) -> Result<Value, String> {
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
        _ => Err(format!("未知自定义智能体命令：{command}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
