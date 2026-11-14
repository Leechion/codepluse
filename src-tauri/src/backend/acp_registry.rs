//! ACP registry catalog, ported from `server-go/internal/codeg/custom_agents.go`
//! (`fetchRegistryEntries` + `registryCatalog` + the spec-kind helpers).
//!
//! Ported (1): `acp_fetch_registry_catalog`. The sibling install commands
//! (`acp_prepare_npx_agent`, `acp_download_agent_binary`, …) stay with the ACP
//! runtime (⑤): the download path checks the sidecar's live connection
//! registry before clearing caches.
//!
//! `fetchRegistryEntries` reads the public registry (35s timeout, 8MB cap,
//! UA `codepluse/2.0-go`). `CODEPLUSE_ACP_REGISTRY` overrides the URL for
//! tests, playing the role of Go's package-level indirection.

use std::path::Path;

use serde_json::{json, Value};

use crate::backend::net;
use crate::backend::store_file;

const ACP_REGISTRY_URL: &str =
    "https://cdn.agentclientprotocol.com/registry/v1/latest/registry.json";

pub fn owns(command: &str) -> bool {
    command == "acp_fetch_registry_catalog"
}

pub fn handle(root: &Path, command: &str, _args: &Value) -> Option<Result<Value, String>> {
    if !owns(command) {
        return None;
    }
    Some(registry_catalog_with_url(&registry_url(), root))
}

fn registry_url() -> String {
    std::env::var("CODEPLUSE_ACP_REGISTRY")
        .ok()
        .filter(|v| !v.trim().is_empty())
        .unwrap_or_else(|| ACP_REGISTRY_URL.to_string())
}

/// custom_agents.go `currentACPPlatform`：GOOS-GOARCH 形态。
fn current_acp_platform() -> String {
    let os = match std::env::consts::OS {
        "macos" => "darwin",
        other => other,
    };
    let arch = match std::env::consts::ARCH {
        "x86" => "x86",
        "x86_64" => "x86_64",
        "aarch64" => "aarch64",
        other => other,
    };
    format!("{os}-{arch}")
}

fn custom_kinds(spec: &Value) -> Vec<String> {
    let mut out = Vec::new();
    if spec
        .get("binary")
        .and_then(|v| v.as_object())
        .map(|m| !m.is_empty())
        .unwrap_or(false)
    {
        out.push("binary".into());
    }
    if spec.get("npx").map(Value::is_object).unwrap_or(false) {
        out.push("npx".into());
    }
    if spec.get("uvx").map(Value::is_object).unwrap_or(false) {
        out.push("uvx".into());
    }
    out
}

fn custom_kind_runs_here(spec: &Value, kind: &str) -> bool {
    match kind {
        "binary" => spec
            .pointer(&format!("/binary/{}", current_acp_platform()))
            .and_then(|b| b.get("archive"))
            .and_then(|v| v.as_str())
            .map(|archive| !archive.is_empty())
            .unwrap_or(false),
        "npx" => spec
            .pointer("/npx/package")
            .and_then(|v| v.as_str())
            .map(|package| !package.is_empty())
            .unwrap_or(false),
        "uvx" => spec
            .pointer("/uvx/package")
            .and_then(|v| v.as_str())
            .map(|package| !package.is_empty())
            .unwrap_or(false),
        _ => false,
    }
}

fn default_custom_kind(spec: &Value) -> String {
    for kind in custom_kinds(spec) {
        if custom_kind_runs_here(spec, &kind) {
            return kind;
        }
    }
    String::new()
}

fn builtin_registry_id(id: &str) -> bool {
    matches!(
        id,
        "codex-acp" | "claude-acp" | "claude-code-acp" | "deepseek" | "dsh" | "workbuddy"
    )
}

/// Go 把 spec 解码进 customAgentSpec 再序列化：未知字段丢弃，空串/空数组/缺省
/// map 被 omitempty 丢掉。Rust 透传原始 JSON 会多出 "args":[] 这类键——
/// 这里按 Go 的形状重建。
fn canon_spec(spec: &Value) -> Value {
    fn text<'a>(value: &'a Value, key: &str) -> &'a str {
        value.get(key).and_then(|v| v.as_str()).unwrap_or("")
    }
    let package_spec = |value: Option<&Value>| -> Option<Value> {
        let value = value?;
        if !value.is_object() {
            return None;
        }
        let package = text(value, "package");
        if package.is_empty() {
            return Some(json!({"package": ""}));
        }
        let mut out = json!({"package": package});
        if let Some(obj) = value.as_object() {
            let cmd = obj.get("cmd").and_then(|v| v.as_str()).unwrap_or("");
            if !cmd.is_empty() {
                out["cmd"] = json!(cmd);
            }
            if let Some(args) = obj.get("args").and_then(|v| v.as_array()) {
                if !args.is_empty() {
                    out["args"] = json!(args);
                }
            }
            if let Some(env) = obj.get("env").and_then(|v| v.as_object()) {
                if !env.is_empty() {
                    out["env"] = json!(env);
                }
            }
            for key in ["python", "nodeRequired", "uvRequired"] {
                let text = obj.get(key).and_then(|v| v.as_str()).unwrap_or("");
                if !text.is_empty() {
                    out[key] = json!(text);
                }
            }
        }
        Some(out)
    };
    let mut out = serde_json::Map::new();
    if let Some(npx) = package_spec(spec.get("npx")) {
        out.insert("npx".into(), npx);
    }
    if let Some(uvx) = package_spec(spec.get("uvx")) {
        out.insert("uvx".into(), uvx);
    }
    if let Some(binary) = spec.get("binary").and_then(|v| v.as_object()) {
        if !binary.is_empty() {
            let mut platforms = serde_json::Map::new();
            for (platform, value) in binary {
                let archive = text(value, "archive");
                let cmd = text(value, "cmd");
                let mut row = json!({"archive": archive, "cmd": cmd});
                if let Some(obj) = value.as_object() {
                    if let Some(args) = obj.get("args").and_then(|v| v.as_array()) {
                        if !args.is_empty() {
                            row["args"] = json!(args);
                        }
                    }
                    if let Some(env) = obj.get("env").and_then(|v| v.as_object()) {
                        if !env.is_empty() {
                            row["env"] = json!(env);
                        }
                    }
                    let sha = text(value, "sha256");
                    if !sha.is_empty() {
                        row["sha256"] = json!(sha);
                    }
                }
                platforms.insert(platform.clone(), row);
            }
            out.insert("binary".into(), Value::Object(platforms));
        }
    }
    Value::Object(out)
}

/// custom_agents.go `fetchRegistryEntries`.
/// 供 custom_agents 的 add_registry_agent 复用。
pub(crate) fn fetch_entries() -> Result<Vec<Value>, String> {
    fetch_registry_entries(&registry_url())
}

fn fetch_registry_entries(registry: &str) -> Result<Vec<Value>, String> {
    let response = net::client()
        .get(registry)
        .timeout(std::time::Duration::from_secs(35))
        .header("User-Agent", "codepluse/2.0-go")
        .send()
        .map_err(|e| format!("无法读取 ACP 注册表：{e}"))?;
    let status = response.status().as_u16();
    if status != 200 {
        return Err(format!("无法读取 ACP 注册表：HTTP {status}"));
    }
    let body = response.bytes().map_err(|e| e.to_string())?;
    let payload: Value =
        serde_json::from_slice(&body).map_err(|e| format!("ACP 注册表 JSON 无效：{e}"))?;
    let Some(agents) = payload.get("agents").and_then(|v| v.as_array()) else {
        return Err("ACP 注册表缺少 agents 列表".into());
    };
    Ok(agents.clone())
}

/// custom_agents.go `registryCatalog`：注册表行 + 已安装标记 + 平台适配标记，
/// 按名称（忽略大小写）稳定排序。
fn registry_catalog(root: &Path) -> Result<Value, String> {
    registry_catalog_with_url(&registry_url(), root)
}

fn registry_catalog_with_url(registry: &str, root: &Path) -> Result<Value, String> {
    let entries = fetch_registry_entries(registry)?;
    let installed: Vec<String> =
        store_file::read_json(&root.join(".data").join("codeg-workspace.json"))?
            .and_then(|doc| {
                doc.get("customAgents")
                    .and_then(|v| v.as_object())
                    .map(|map| map.keys().cloned().collect())
            })
            .unwrap_or_default();
    let mut rows: Vec<Value> = Vec::new();
    for entry in &entries {
        let id = entry
            .get("id")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let spec = canon_spec(&entry.get("distribution").cloned().unwrap_or(Value::Null));
        rows.push(json!({
            "registryId": id,
            "name": entry.get("name").cloned().unwrap_or(Value::Null),
            "description": entry.get("description").cloned().unwrap_or(Value::Null),
            "version": entry.get("version").cloned().unwrap_or(Value::Null),
            "iconUrl": entry.get("icon").cloned().unwrap_or(Value::Null),
            "website": entry.get("website").cloned().unwrap_or(Value::Null),
            "repository": entry.get("repository").cloned().unwrap_or(Value::Null),
            "license": entry.get("license").cloned().unwrap_or(Value::Null),
            "distributionKinds": custom_kinds(&spec),
            "builtin": builtin_registry_id(&id),
            "installed": installed.iter().any(|key| *key == id),
            "supportedOnPlatform": !default_custom_kind(&spec).is_empty(),
            "spec": spec,
        }));
    }
    rows.sort_by(|a, b| {
        let name = |row: &Value| {
            row.get("name")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_lowercase()
        };
        name(a).cmp(&name(b))
    });
    Ok(Value::Array(rows))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::io::{Read, Write};
    use std::net::TcpListener;

    fn serve_registry(payload: &'static str) -> String {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap().to_string();
        std::thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                let mut stream = stream;
                let mut buffer = [0u8; 4096];
                let _ = stream.read(&mut buffer);
                let response = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    payload.len(),
                    payload
                );
                let _ = stream.write_all(response.as_bytes());
            }
        });
        format!("http://{address}/registry.json")
    }

    fn temp_root(tag: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "codepluse-acp-registry-{tag}-{}-{}",
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
    fn catalog_marks_builtin_installed_and_platform_support() {
        let payload = r#"{"agents":[
            {"id":"claude-acp","name":"B Claude","description":"d","distribution":{"npx":{"package":"@x/claude-acp","cmd":"claude-acp"}}},
            {"id":"custom-one","name":"A Custom","description":"d2","version":"1.2.3","distribution":{"binary":{"windows-x86_64":{"archive":"https://dl/x.zip","cmd":"x"}}}},
            {"id":"linux-only","name":"C Linux","distribution":{"binary":{"linux-aarch64":{"archive":"https://dl/l.tar.gz","cmd":"l"}}}}
        ]}"#;
        let url = serve_registry(payload);
        let root = temp_root("catalog");
        std::fs::write(
            root.join(".data").join("codeg-workspace.json"),
            json!({"next": 1, "customAgents": {"claude-acp": {"registryId": "claude-acp"}}})
                .to_string(),
        )
        .unwrap();
        let rows = registry_catalog_with_url(&url, &root).unwrap();
        let rows = rows.as_array().unwrap();
        assert_eq!(rows.len(), 3);
        // 名称忽略大小写排序：A Custom、B Claude、C Linux。
        assert_eq!(rows[0]["registryId"], "custom-one");
        assert_eq!(rows[1]["registryId"], "claude-acp");
        assert_eq!(rows[2]["registryId"], "linux-only");
        assert_eq!(rows[1]["installed"], true, "customAgents 里的视为已安装");
        assert_eq!(rows[0]["installed"], false);
        assert_eq!(rows[1]["builtin"], true);
        assert_eq!(rows[0]["builtin"], false);
        assert_eq!(
            rows[0]["supportedOnPlatform"], true,
            "本机是 windows-x86_64"
        );
        assert_eq!(
            rows[2]["supportedOnPlatform"], false,
            "linux-aarch64 不适配"
        );
        assert_eq!(rows[0]["distributionKinds"], json!(["binary"]));
        assert_eq!(rows[1]["distributionKinds"], json!(["npx"]));
        assert_eq!(rows[0]["version"], "1.2.3");
        assert_eq!(rows[1]["version"], Value::Null);
    }

    #[test]
    fn missing_agents_list_is_an_error() {
        let url = serve_registry(r#"{"agents": null}"#);
        let root = temp_root("empty");
        let err = registry_catalog_with_url(&url, &root).unwrap_err();
        assert_eq!(err, "ACP 注册表缺少 agents 列表");
    }

    #[test]
    fn platform_strings_match_go() {
        // 本机断言形状；GOOS 映射由 current_acp_platform 处理。
        let platform = current_acp_platform();
        assert!(
            platform.starts_with("windows-")
                || platform.starts_with("linux-")
                || platform.starts_with("darwin-")
        );
        assert!(platform.contains("x86_64") || platform.contains("aarch64"));
    }
}
