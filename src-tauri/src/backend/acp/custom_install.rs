//! Custom-agent install/launch primitives, ported from `codeg/
//! {custom_launch,custom_agents,custom_archive}.go`. The three install
//! commands (`acp_prepare_npx_agent`, `acp_uninstall_agent`,
//! `acp_clear_binary_cache`) run through `install_command` from the command
//! layer; everything here is the reusable machinery underneath.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde_json::{json, Value};

use crate::backend::acp::agents::{home_dir, Agent};
use crate::backend::acp::launch::Launch;
use crate::backend::store_file;

/// Mirror of Go's `customAgentDef` (custom_agents.go:45), as persisted under
/// `Store.CustomAgents` in `codeg-workspace.json`.
#[derive(Clone, Debug, Default)]
pub struct CustomAgentDef {
    pub registry_id: String,
    pub name: String,
    pub description: String,
    pub version: String,
    pub kind: String,
    pub spec: Value,
    pub icon_url: Option<String>,
    pub skills_shared_store: bool,
    pub skills_dir: Option<String>,
    pub source: String,
    pub version_probe: Option<String>,
    pub supports_mcp: bool,
}

/// Go `customLaunch` (custom_launch.go:23).
#[derive(Clone, Debug, Default, serde::Serialize, serde::Deserialize)]
pub struct CustomLaunch {
    #[serde(default)]
    pub command: String,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub version: String,
}

/// Go `currentACPPlatform` (custom_agents.go:71).
pub fn current_acp_platform() -> String {
    let goarch = if cfg!(target_arch = "x86_64") {
        "amd64"
    } else if cfg!(target_arch = "aarch64") {
        "arm64"
    } else if cfg!(target_arch = "x86") {
        "386"
    } else {
        "unknown"
    };
    let arch = match goarch {
        "amd64" => "x86_64",
        "arm64" => "aarch64",
        "386" => "x86",
        other => other,
    };
    format!("{}-{}", std::env::consts::OS, arch)
}

pub fn builtin_registry_id(id: &str) -> bool {
    matches!(
        id,
        "codex-acp" | "claude-acp" | "claude-code-acp" | "deepseek" | "dsh" | "workbuddy"
    )
}

/// Go `customIDPattern` (custom_agents.go:111).
fn valid_custom_id(id: &str) -> bool {
    let bytes = id.as_bytes();
    if bytes.is_empty() || bytes.len() > 128 {
        return false;
    }
    let first = bytes[0];
    if !(first.is_ascii_alphanumeric()) {
        return false;
    }
    bytes[1..]
        .iter()
        .all(|b| b.is_ascii_alphanumeric() || *b == b'_' || *b == b'.' || *b == b'-')
}

/// Go `customDef` (custom_agents.go:117): resolve the definition behind a
/// `custom:` wire name from `Store.CustomAgents`.
pub fn custom_def(root: &Path, wire: &str) -> Option<CustomAgentDef> {
    let rid = wire.strip_prefix("custom:")?;
    let doc = store_file::read_json(&root.join(".data").join("codeg-workspace.json")).ok()??;
    let raw = doc.get("customAgents")?.get(rid)?.clone();
    Some(def_from_json(&raw))
}

pub fn def_from_json(raw: &Value) -> CustomAgentDef {
    let get = |k: &str| raw.get(k).and_then(Value::as_str).unwrap_or("").to_string();
    CustomAgentDef {
        registry_id: get("registryId"),
        name: get("name"),
        description: get("description"),
        version: get("version"),
        kind: get("distributionKind"),
        spec: raw.get("spec").cloned().unwrap_or(Value::Null),
        icon_url: raw
            .get("iconUrl")
            .filter(|v| !v.is_null())
            .and_then(Value::as_str)
            .map(String::from),
        skills_shared_store: raw
            .get("skillsSharedStore")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        skills_dir: raw
            .get("skillsDir")
            .filter(|v| !v.is_null())
            .and_then(Value::as_str)
            .map(String::from),
        source: get("source"),
        version_probe: raw
            .get("versionProbe")
            .filter(|v| !v.is_null())
            .and_then(Value::as_str)
            .map(String::from),
        supports_mcp: raw
            .get("supportsMcp")
            .and_then(Value::as_bool)
            .unwrap_or(false),
    }
}

fn spec_part<'a>(d: &'a CustomAgentDef, kind: &str) -> Option<&'a Value> {
    match kind {
        "npx" => d.spec.get("npx").filter(|v| !v.is_null()),
        "uvx" => d.spec.get("uvx").filter(|v| !v.is_null()),
        _ => None,
    }
}

/// Go `customKindRunsHere` (custom_agents.go:91).
pub fn custom_kind_runs_here(spec: &Value, kind: &str) -> bool {
    match kind {
        "binary" => spec
            .get("binary")
            .and_then(|b| b.get(current_acp_platform()))
            .and_then(|p| p.get("archive"))
            .and_then(Value::as_str)
            .map(|a| !a.is_empty())
            .unwrap_or(false),
        "npx" => spec
            .get("npx")
            .and_then(|p| p.get("package"))
            .and_then(Value::as_str)
            .map(|p| !p.is_empty())
            .unwrap_or(false),
        "uvx" => spec
            .get("uvx")
            .and_then(|p| p.get("package"))
            .and_then(Value::as_str)
            .map(|p| !p.is_empty())
            .unwrap_or(false),
        _ => false,
    }
}

pub fn custom_kinds(spec: &Value) -> Vec<String> {
    let mut out = Vec::new();
    if spec
        .get("binary")
        .and_then(Value::as_object)
        .map(|m| !m.is_empty())
        .unwrap_or(false)
    {
        out.push("binary".into());
    }
    if spec.get("npx").filter(|v| !v.is_null()).is_some() {
        out.push("npx".into());
    }
    if spec.get("uvx").filter(|v| !v.is_null()).is_some() {
        out.push("uvx".into());
    }
    out
}

pub fn default_custom_kind(spec: &Value) -> String {
    custom_kinds(spec)
        .into_iter()
        .find(|kind| custom_kind_runs_here(spec, kind))
        .unwrap_or_default()
}

/// Go `validateCustomDef` (custom_agents.go:134).
pub fn validate_custom_def(d: &CustomAgentDef) -> Result<(), String> {
    if !valid_custom_id(&d.registry_id) || builtin_registry_id(&d.registry_id) {
        return Err("自定义智能体 ID 无效或与内置执行端冲突".into());
    }
    if !custom_kind_runs_here(&d.spec, &d.kind) {
        return Err(format!(
            "{} 未提供适用于 {} 的 {} 发行版",
            d.registry_id,
            current_acp_platform(),
            d.kind
        ));
    }
    if d.kind == "binary" {
        let b = d
            .spec
            .get("binary")
            .and_then(|b| b.get(current_acp_platform()))
            .cloned()
            .unwrap_or(Value::Null);
        let archive = b.get("archive").and_then(Value::as_str).unwrap_or("");
        let u = url::Url::parse(archive).ok();
        let ok_host = u
            .as_ref()
            .map(|u| !u.host_str().unwrap_or("").is_empty())
            .unwrap_or(false);
        let ok_scheme = u
            .as_ref()
            .map(|u| u.scheme() == "https" || u.scheme() == "http")
            .unwrap_or(false);
        if !ok_host || !ok_scheme {
            return Err("归档下载地址必须为 HTTP(S) URL".into());
        }
        safe_custom_path(
            Path::new("."),
            b.get("cmd").and_then(Value::as_str).unwrap_or(""),
        )
        .map_err(|e| format!("归档启动路径无效: {e}"))?;
        if let Some(sha) = b
            .get("sha256")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
        {
            if sha.len() != 64 || !sha.bytes().all(|b| b.is_ascii_hexdigit()) {
                return Err("SHA-256 校验值必须是 64 位十六进制字符串".into());
            }
        }
    } else {
        let p = spec_part(d, &d.kind).cloned().unwrap_or(Value::Null);
        let package = p.get("package").and_then(Value::as_str).unwrap_or("");
        if package.starts_with('-') || package.bytes().any(|b| b == b'\r' || b == b'\n' || b == 0) {
            return Err("包名称无效".into());
        }
    }
    Ok(())
}

/// Go `customCacheDir` (custom_launch.go:32).
pub fn custom_cache_dir(root: &Path, d: &CustomAgentDef) -> PathBuf {
    use sha2::{Digest, Sha256};
    let payload = json!({
        "spec": d.spec,
        "kind": d.kind,
        "version": d.version,
        "platform": current_acp_platform(),
    });
    let canonical = crate::backend::goformat::canon(&payload).to_string();
    let mut hasher = Sha256::new();
    hasher.update(canonical.as_bytes());
    let hash = hasher.finalize();
    root.join(".data")
        .join("acp-custom")
        .join(&d.registry_id)
        .join(
            format!("{:x}", u128::from_be_bytes(hash[..16].try_into().unwrap()))[..16]
                .to_string()
                .get(..16)
                .unwrap()
                .to_string(),
        )
}

/// Go `cachedCustomLaunch` (custom_launch.go:36): a cached launch is valid when
/// the command and its absolute args still exist.
pub fn cached_custom_launch(root: &Path, d: &CustomAgentDef) -> Option<CustomLaunch> {
    let dir = custom_cache_dir(root, d);
    let data = std::fs::read(dir.join("launch.json")).ok()?;
    let launch: CustomLaunch = serde_json::from_slice(&data).ok()?;
    if launch.command.is_empty() || !Path::new(&launch.command).is_file() {
        return None;
    }
    for arg in &launch.args {
        if Path::new(arg).is_absolute() && !Path::new(arg).exists() {
            return None;
        }
    }
    Some(launch)
}

/// Go `customInstalledVersion` (custom_launch.go:54): version, "local", or null.
pub fn custom_installed_version(root: &Path, d: &CustomAgentDef) -> Value {
    match cached_custom_launch(root, d) {
        Some(l) => {
            if !l.version.is_empty() {
                json!(l.version)
            } else {
                json!("local")
            }
        }
        None => Value::Null,
    }
}

/// Go `npmCLI` (custom_launch.go:63): node + npm-cli.js pair.
pub fn npm_cli() -> Result<(String, String), String> {
    let node = crate::backend::acp::launch::which("node")
        .map_err(|_| "未找到 Node.js，请先安装 Node.js 与 npm".to_string())?;
    let mut paths: Vec<PathBuf> = Vec::new();
    if let Some(dir) = Path::new(&node).parent() {
        paths.push(
            dir.join("node_modules")
                .join("npm")
                .join("bin")
                .join("npm-cli.js"),
        );
    }
    for name in ["npm", "npm.cmd"] {
        if let Ok(found) = crate::backend::acp::launch::which(name) {
            if let Ok(real) = std::fs::canonicalize(&found) {
                if real.to_string_lossy().ends_with("npm-cli.js") {
                    paths.push(real);
                }
            }
            if let Some(dir) = Path::new(&found).parent() {
                paths.push(
                    dir.join("node_modules")
                        .join("npm")
                        .join("bin")
                        .join("npm-cli.js"),
                );
            }
        }
    }
    for p in paths {
        if let Ok(meta) = std::fs::metadata(&p) {
            if !meta.is_dir() {
                return Ok((node, p.to_string_lossy().to_string()));
            }
        }
    }
    Err("未找到 npm-cli.js，请修复 Node.js/npm 安装".into())
}

/// Go `npmPackageName` (custom_launch.go:84).
pub fn npm_package_name(spec: &str) -> String {
    let mut name = spec;
    if let Some(at) = name.rfind('@') {
        if at > 0 {
            name = &name[..at];
        }
    }
    name.to_string()
}

/// Go `pythonCommandName` (custom_launch.go:91).
pub fn python_command_name(spec: &str) -> String {
    spec.split(|c: char| "[=<>!~@ ".contains(c))
        .find(|s| !s.is_empty())
        .unwrap_or("")
        .trim()
        .to_string()
}

/// Go `customEnv` (custom_launch.go:98): merge env lists with the extras
/// winning, output sorted by key.
pub fn custom_env(base: &[String], extra: &BTreeMap<String, String>) -> Vec<String> {
    let mut values: BTreeMap<String, String> = BTreeMap::new();
    for item in base {
        if let Some((key, val)) = item.split_once('=') {
            values.insert(key.to_string(), val.to_string());
        }
    }
    for (k, v) in extra {
        values.insert(k.clone(), v.clone());
    }
    values
        .into_iter()
        .map(|(k, v)| format!("{k}={v}"))
        .collect()
}

/// Go `runCustomInstaller` (custom_launch.go:119): 5-minute window, combined
/// output tail 6000 chars on failure.
pub fn run_custom_installer(
    command: &str,
    args: &[String],
    env: &[String],
    dir: &Path,
) -> Result<(), String> {
    use std::process::Command;
    let mut cmd = Command::new(command);
    cmd.args(args).current_dir(dir).env_clear();
    for item in env {
        if let Some((k, v)) = item.split_once('=') {
            cmd.env(k, v);
        }
    }
    let output = cmd
        .output()
        .map_err(|e| format!("安装自定义 ACP 智能体失败：{e}"))?;
    if !output.status.success() {
        let mut message = String::from_utf8_lossy(&output.stdout).to_string();
        message.push_str(&String::from_utf8_lossy(&output.stderr));
        let message = message.trim().to_string();
        let message = if message.chars().count() > 6000 {
            message
                .chars()
                .skip(message.chars().count() - 6000)
                .collect()
        } else {
            message
        };
        return Err(format!(
            "安装自定义 ACP 智能体失败：{}\n{message}",
            exit_code(&output.status)
        ));
    }
    Ok(())
}

fn exit_code(status: &std::process::ExitStatus) -> String {
    status
        .code()
        .map(|c| format!("exit status {c}"))
        .unwrap_or_else(|| "exit status unknown".into())
}

/// Go `customNotInstalled` (custom_launch.go:143): the English substring is an
/// INVARIANT the frontend routes on (`message.includes("is not installed")`).
pub fn custom_not_installed(name: &str, detail: Option<&str>) -> String {
    match detail {
        None => format!(
            "{name} is not installed. Please install it in Agent Settings.（该自定义智能体尚未安装或启动入口缺失，请在「设置 → 智能体」中重新安装）"
        ),
        Some(detail) => format!(
            "{name} is not installed. Please install it in Agent Settings.（该自定义智能体尚未安装或启动入口缺失，请在「设置 → 智能体」中重新安装：{detail}）"
        ),
    }
}

/// Go `safeCustomPath` (custom_archive.go:21): registry entries are untrusted.
pub fn safe_custom_path(root: &Path, entry: &str) -> Result<PathBuf, String> {
    let entry = entry.replace('\\', "/");
    if entry.is_empty() || entry.contains(':') || entry.contains('\0') || entry.starts_with('/') {
        return Err(format!("不安全的归档路径：{entry:?}"));
    }
    for part in entry.split('/') {
        if part == ".." || part.ends_with(' ') || (part != "." && part.ends_with('.')) {
            return Err(format!("不安全的归档路径：{entry:?}"));
        }
    }
    let cleaned = PathBuf::from(&entry);
    let text = cleaned.to_string_lossy();
    if text == "." || text == ".." || text.starts_with("..\\") || text.starts_with("../") {
        return Err(format!("归档路径越界：{entry:?}"));
    }
    Ok(root.join(cleaned))
}

fn write_custom_entry(
    root: &Path,
    name: &str,
    executable: bool,
    mut reader: impl std::io::Read,
) -> Result<(), String> {
    let dest = safe_custom_path(root, name)?;
    if let Some(dir) = dest.parent() {
        std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    }
    let mut file = std::fs::File::create(&dest).map_err(|e| e.to_string())?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = if executable { 0o755 } else { 0o600 };
        std::fs::set_permissions(&dest, std::fs::Permissions::from_mode(mode)).ok();
    }
    std::io::copy(&mut reader, &mut file).map_err(|e| e.to_string())?;
    Ok(())
}

/// Go `extractCustomArchive` (custom_archive.go:62): zip / tar / tar.gz /
/// tar.bz2 by URL path suffix; links and special files are rejected.
pub fn extract_custom_archive(root: &Path, archive: &Path, address: &str) -> Result<(), String> {
    let name = url::Url::parse(address)
        .map(|u| u.path().to_lowercase())
        .unwrap_or_else(|_| address.to_lowercase());
    if name.ends_with(".zip") {
        let file = std::fs::File::open(archive).map_err(|e| e.to_string())?;
        let mut zip = zip::ZipArchive::new(file).map_err(|e| e.to_string())?;
        for i in 0..zip.len() {
            let mut entry = zip.by_index(i).map_err(|e| e.to_string())?;
            if entry.is_dir() {
                continue;
            }
            if entry.is_symlink() {
                return Err(format!("不支持归档中的链接或特殊文件：{}", entry.name()));
            }
            let executable = entry.unix_mode().map(|m| m & 0o111 != 0).unwrap_or(false);
            let name = entry.name().to_string();
            write_custom_entry(root, &name, executable, &mut entry)?;
        }
        return Ok(());
    }
    let file = std::fs::File::open(archive).map_err(|e| e.to_string())?;
    let reader: Box<dyn std::io::Read> = if name.ends_with(".tar.gz") || name.ends_with(".tgz") {
        Box::new(flate2::read::GzDecoder::new(file))
    } else if name.ends_with(".tar.bz2") {
        Box::new(bzip2::read::BzDecoder::new(file))
    } else if name.ends_with(".tar") {
        Box::new(file)
    } else {
        return Err(format!(
            "不支持归档格式：{name}（支持 zip、tar、tar.gz、tar.bz2）"
        ));
    };
    let mut tar = tar::Archive::new(reader);
    for entry in tar.entries().map_err(|e| e.to_string())? {
        let mut entry = entry.map_err(|e| e.to_string())?;
        if entry.header().entry_type() == tar::EntryType::Directory {
            continue;
        }
        if !entry.header().entry_type().is_file() {
            return Err(format!(
                "不支持归档中的链接或特殊文件：{}",
                entry
                    .path()
                    .map(|p| p.to_string_lossy().to_string())
                    .unwrap_or_default()
            ));
        }
        let name = entry
            .path()
            .map_err(|e| e.to_string())?
            .to_string_lossy()
            .to_string();
        let executable = entry
            .header()
            .mode()
            .map(|m| m & 0o111 != 0)
            .unwrap_or(false);
        write_custom_entry(root, &name, executable, &mut entry)?;
    }
    Ok(())
}

/// Go `installCustomArchive` (custom_archive.go:109): download (5-minute
/// window), verify SHA-256, extract into a temp dir, promote to `payload/`.
pub fn install_custom_archive(root: &Path, dir: &Path, b: &Value) -> Result<String, String> {
    use sha2::{Digest, Sha256};
    let archive_url = b.get("archive").and_then(Value::as_str).unwrap_or("");
    let response = crate::backend::net::get(archive_url, std::time::Duration::from_secs(5 * 60))
        .map_err(|e| format!("下载智能体失败：{e}"))?;
    if response.status().as_u16() != 200 {
        return Err(format!(
            "下载智能体失败：HTTP {}",
            response.status().as_u16()
        ));
    }
    let body = response
        .bytes()
        .map_err(|e| format!("下载智能体失败：{e}"))?;
    let mut hasher = Sha256::new();
    hasher.update(&body);
    let hash = format!("{:x}", hasher.finalize());
    let expect = b.get("sha256").and_then(Value::as_str).unwrap_or("");
    if !expect.is_empty() && !hash.eq_ignore_ascii_case(expect) {
        return Err("智能体归档 SHA-256 校验失败".into());
    }
    std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    let archive_path = dir.join(format!("archive-{}", crate::backend::goformat::hex_id()));
    std::fs::write(&archive_path, &body).map_err(|e| e.to_string())?;
    let extracted = dir.join(format!("extract-{}", crate::backend::goformat::hex_id()));
    std::fs::create_dir_all(&extracted).map_err(|e| e.to_string())?;
    let outcome = extract_custom_archive(&extracted, &archive_path, archive_url);
    let _ = std::fs::remove_file(&archive_path);
    outcome?;
    let command = b.get("cmd").and_then(Value::as_str).unwrap_or("");
    let entry = safe_custom_path(&extracted, command)?;
    match std::fs::metadata(&entry) {
        Ok(meta) if meta.is_file() => {}
        _ => return Err(format!("归档内没有启动文件：{command}")),
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&entry, std::fs::Permissions::from_mode(0o755)).ok();
    }
    let payload = dir.join("payload");
    if payload.exists() {
        std::fs::remove_dir_all(&payload).map_err(|e| e.to_string())?;
    }
    std::fs::rename(&extracted, &payload).map_err(|e| e.to_string())?;
    safe_custom_path(&payload, command).map(|p| p.to_string_lossy().to_string())
}

/// Go `prepareCustom` (custom_launch.go:150): cache-first install for one
/// definition. Blocking (npm/uv installers run synchronously).
pub fn prepare_custom(root: &Path, d: &CustomAgentDef) -> Result<CustomLaunch, String> {
    validate_custom_def(d)?;
    let dir = custom_cache_dir(root, d);
    if let Some(launch) = cached_custom_launch(root, d) {
        return Ok(launch);
    }
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let mut launch = CustomLaunch {
        version: d.version.clone(),
        args: Vec::new(),
        command: String::new(),
    };
    match d.kind.as_str() {
        "binary" => {
            let b = d
                .spec
                .get("binary")
                .and_then(|b| b.get(current_acp_platform()))
                .cloned()
                .unwrap_or(Value::Null);
            let command = install_custom_archive(root, &dir, &b)?;
            launch.command = command;
        }
        "npx" => {
            let p = spec_part(d, "npx").cloned().unwrap_or(Value::Null);
            let (node, npm) = npm_cli()?;
            let package = p.get("package").and_then(Value::as_str).unwrap_or("");
            let package_name = npm_package_name(package);
            if package_name.is_empty()
                || !(package_name.starts_with('@')
                    || package_name
                        .bytes()
                        .next()
                        .map(|b| b.is_ascii_alphanumeric() || b == b'_')
                        .unwrap_or(false))
            {
                return Err(format!("npm 包名称无效：{package_name}"));
            }
            run_custom_installer(
                &node,
                &[
                    npm,
                    "install".into(),
                    "--prefix".into(),
                    dir.to_string_lossy().to_string(),
                    "--no-audit".into(),
                    "--no-fund".into(),
                    "--omit=dev".into(),
                    "--".into(),
                    package.into(),
                ],
                &std::env::vars()
                    .map(|(k, v)| format!("{k}={v}"))
                    .collect::<Vec<String>>(),
                &dir,
            )?;
            let package_dir = dir
                .join("node_modules")
                .join(package_name.replace('/', std::path::MAIN_SEPARATOR_STR));
            let data = match std::fs::read(package_dir.join("package.json")) {
                // 包没装上（或目录被清掉）：按原版归入 SdkNotInstalled。
                Err(e) => return Err(custom_not_installed(&d.name, Some(&e.to_string()))),
                Ok(data) => data,
            };
            let pkg: Value = serde_json::from_slice(&data).map_err(|e| e.to_string())?;
            let bin_raw = pkg.get("bin").cloned().unwrap_or(Value::Null);
            let mut bin = String::new();
            if let Some(s) = bin_raw.as_str() {
                bin = s.to_string();
            }
            if bin.is_empty() {
                let mut bins: BTreeMap<String, String> = BTreeMap::new();
                if let Some(map) = bin_raw.as_object() {
                    for (k, v) in map {
                        if let Some(s) = v.as_str() {
                            bins.insert(k.clone(), s.to_string());
                        }
                    }
                }
                let command = p.get("cmd").and_then(Value::as_str).unwrap_or("");
                if !command.is_empty() {
                    bin = bins.get(command).cloned().unwrap_or_default();
                } else if bins.len() == 1 {
                    bin = bins.values().next().cloned().unwrap_or_default();
                } else {
                    bin = bins.get(&package_name).cloned().unwrap_or_default();
                }
            }
            if bin.is_empty() {
                return Err(format!(
                    "{package} 没有唯一的启动入口，请在手动配置的 cmd 中指定 package.json 中的 bin 名称"
                ));
            }
            let entry = safe_custom_path(&package_dir, &bin)?;
            if !entry.exists() {
                return Err(custom_not_installed(&d.name, None));
            }
            launch.version = pkg
                .get("version")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            let mut prefix = [0u8; 128];
            let n = std::fs::File::open(&entry)
                .and_then(|mut f| std::io::Read::read(&mut f, &mut prefix))
                .unwrap_or(0);
            let first_line = String::from_utf8_lossy(&prefix[..n]);
            let first_line = first_line.split('\n').next().unwrap_or("");
            let entry_text = entry.to_string_lossy().to_string();
            let looks_js = entry_text.ends_with(".js")
                || entry_text.ends_with(".cjs")
                || entry_text.ends_with(".mjs")
                || (first_line.starts_with("#!") && first_line.contains("node"));
            if looks_js {
                launch.command = node;
                launch.args = vec![entry_text];
            } else {
                launch.command = entry_text;
            }
        }
        "uvx" => {
            let p = spec_part(d, "uvx").cloned().unwrap_or(Value::Null);
            let uv = crate::backend::acp::launch::which("uv")
                .map_err(|_| "未找到 uv，请先安装 uv/uvx 运行环境".to_string())?;
            let mut args: Vec<String> = vec!["tool".into(), "install".into(), "--force".into()];
            let python = p.get("python").and_then(Value::as_str).unwrap_or("");
            if !python.is_empty() {
                args.push("--python".into());
                args.push(python.into());
            }
            args.push("--".into());
            args.push(
                p.get("package")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .into(),
            );
            let mut extra = BTreeMap::new();
            extra.insert(
                "UV_TOOL_DIR".into(),
                dir.join("tools").to_string_lossy().to_string(),
            );
            extra.insert(
                "UV_TOOL_BIN_DIR".into(),
                dir.join("bin").to_string_lossy().to_string(),
            );
            let process_env = custom_env(
                &std::env::vars()
                    .map(|(k, v)| format!("{k}={v}"))
                    .collect::<Vec<String>>(),
                &extra,
            );
            run_custom_installer(&uv, &args, &process_env, &dir)?;
            let mut name = p
                .get("cmd")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            if name.is_empty() {
                name = python_command_name(p.get("package").and_then(Value::as_str).unwrap_or(""));
            }
            if cfg!(windows) && !name.ends_with(".exe") {
                name.push_str(".exe");
            }
            let entry = safe_custom_path(&dir.join("bin"), &name)?;
            if !entry.exists() {
                return Err(custom_not_installed(
                    &d.name,
                    Some(&format!(
                        "找不到 uv 工具入口 {name}，请在手动配置中填写 cmd"
                    )),
                ));
            }
            launch.command = entry.to_string_lossy().to_string();
        }
        _ => {}
    }
    let body = serde_json::to_vec(&launch).map_err(|e| e.to_string())?;
    std::fs::write(dir.join("launch.json"), body).map_err(|e| e.to_string())?;
    Ok(launch)
}

/// Go `CustomAgentAvailable` verdict: `Some(false)` for an invalid definition,
/// otherwise the launchability probe. Callers pass the def; `None` means the
/// agent has no custom definition at all.
pub fn available_verdict(root: &Path, d: &CustomAgentDef) -> Option<bool> {
    if validate_custom_def(d).is_err() {
        return Some(false);
    }
    if cached_custom_launch(root, d).is_some() {
        return Some(true);
    }
    match d.kind.as_str() {
        "npx" => Some(npm_cli().is_ok()),
        "uvx" => Some(crate::backend::acp::launch::which("uv").is_ok()),
        _ => Some(true),
    }
}

/// Go `CustomAgentLaunch` (custom_launch.go:297): the launch for a custom
/// definition, or `None` when the agent isn't one.
pub fn custom_agent_launch(root: &Path, agent: &Agent) -> Result<Option<Launch>, String> {
    let mut wire = crate::backend::acp::agents::wire_agent(agent);
    if !agent.base_agent_type.is_empty() {
        wire = agent.base_agent_type.clone();
    }
    let Some(d) = custom_def(root, &wire) else {
        // 判定"注册丢失"只看 hub 记录自己的 Wire：registerCustomAgent 一定显式
        // 写 `custom:<registryId>`，而 wireAgent 的兜底会把 Wire 为空的 harness
        // 实例也算成 `custom:<ID>`。用兜底值判断会让 instance 起来的会话永远
        // 走不到 customAcp 路径。
        if agent.wire.starts_with("custom:") {
            let name = agent.wire.trim_start_matches("custom:").to_string();
            return Err(format!(
                "The custom agent {name:?} is no longer registered. Re-add it in Settings → Agents.（该自定义智能体已不在注册表中，请在「设置 → 智能体」重新添加）"
            ));
        }
        return Ok(None);
    };
    let launch = prepare_custom(root, &d)?;
    let args: Vec<String> = match d.kind.as_str() {
        "binary" => {
            let b = d
                .spec
                .get("binary")
                .and_then(|b| b.get(current_acp_platform()))
                .cloned()
                .unwrap_or(Value::Null);
            string_list(b.get("args"))
        }
        "npx" => string_list(spec_part(&d, "npx").and_then(|p| p.get("args"))),
        "uvx" => string_list(spec_part(&d, "uvx").and_then(|p| p.get("args"))),
        _ => Vec::new(),
    };
    let mut extra: BTreeMap<String, String> = BTreeMap::new();
    let env_spec = match d.kind.as_str() {
        "binary" => d
            .spec
            .get("binary")
            .and_then(|b| b.get(current_acp_platform()))
            .and_then(|b| b.get("env"))
            .cloned()
            .unwrap_or(Value::Null),
        "npx" => spec_part(&d, "npx")
            .and_then(|p| p.get("env"))
            .cloned()
            .unwrap_or(Value::Null),
        "uvx" => spec_part(&d, "uvx")
            .and_then(|p| p.get("env"))
            .cloned()
            .unwrap_or(Value::Null),
        _ => Value::Null,
    };
    if let Some(map) = env_spec.as_object() {
        for (k, v) in map {
            let value = match v {
                Value::String(s) => s.clone(),
                other => other.to_string(),
            };
            extra.insert(k.clone(), value);
        }
    }
    let mut process_env = custom_env(
        &std::env::vars()
            .map(|(k, v)| format!("{k}={v}"))
            .collect::<Vec<String>>(),
        &extra,
    );
    // Go LaunchOverlay: the settings-page env overlay merged on top.
    let overlay = overlay_env_map(root, agent);
    process_env = custom_env(&process_env, &overlay);
    if agent.auth_mode == "subscription" {
        process_env = crate::backend::acp::providers_core::clear_api_environment(&process_env);
    }
    let mut all_args = launch.args.clone();
    all_args.extend(args);
    Ok(Some(Launch {
        command: launch.command,
        args: all_args,
        env: Some(process_env),
    }))
}

fn overlay_env_map(root: &Path, agent: &Agent) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    let Ok(Some(doc)) = store_file::read_json(&root.join(".data").join("codeg-workspace.json"))
    else {
        return out;
    };
    let wire = crate::backend::acp::agents::wire_agent(agent);
    let Some(ov) = doc.get("agents").and_then(|a| a.get(&wire)) else {
        return out;
    };
    if let Some(env) = ov.get("env").and_then(Value::as_object) {
        for (k, v) in env {
            let value = match v {
                Value::String(s) => s.clone(),
                other => other.to_string(),
            };
            out.insert(k.clone(), value);
        }
    }
    out
}

fn string_list(value: Option<&Value>) -> Vec<String> {
    value
        .and_then(Value::as_array)
        .map(|list| {
            list.iter()
                .filter_map(Value::as_str)
                .map(String::from)
                .collect()
        })
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn temp_root(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "acp-install-{tag}-{}",
            crate::backend::goformat::hex_id()
        ));
        fs::create_dir_all(dir.join(".data")).unwrap();
        dir
    }

    #[test]
    fn safe_custom_path_rejects_traversal_and_win32_tricks() {
        let root = Path::new("/tmp/x");
        assert!(safe_custom_path(root, "bin/run.js").is_ok());
        assert!(safe_custom_path(root, "..\\evil").is_err());
        assert!(safe_custom_path(root, "../evil").is_err());
        // Go rejects any `..` component before cleaning, interior ones included.
        assert!(safe_custom_path(root, "a/../b").is_err());
        assert!(safe_custom_path(root, "C:/evil").is_err());
        assert!(safe_custom_path(root, "trailing. ").is_err());
        assert!(safe_custom_path(root, "trailing.").is_err());
        assert!(safe_custom_path(root, "").is_err());
        assert!(safe_custom_path(root, "/abs").is_err());
    }

    #[test]
    fn package_name_and_python_command_parse() {
        assert_eq!(npm_package_name("@org/pkg@1.2.3"), "@org/pkg");
        assert_eq!(npm_package_name("pkg@2"), "pkg");
        assert_eq!(npm_package_name("pkg"), "pkg");
        assert_eq!(python_command_name("mypkg[extra]==1.0"), "mypkg");
        assert_eq!(python_command_name("  pkg~=2 "), "pkg");
        assert_eq!(python_command_name("[=<>"), "");
    }

    #[test]
    fn custom_env_merges_sorted_with_extras_winning() {
        let base = vec!["B=2".to_string(), "A=1".to_string()];
        let mut extra = BTreeMap::new();
        extra.insert("B".to_string(), "override".to_string());
        extra.insert("C".to_string(), "3".to_string());
        assert_eq!(
            custom_env(&base, &extra),
            vec![
                "A=1".to_string(),
                "B=override".to_string(),
                "C=3".to_string()
            ]
        );
    }

    #[test]
    fn validate_custom_def_enforces_platform_and_id() {
        let mut d = CustomAgentDef::default();
        d.registry_id = "codex-acp".into();
        d.kind = "npx".into();
        d.spec = json!({"npx": {"package": "@x/y"}});
        assert!(validate_custom_def(&d).is_err(), "builtin id rejected");
        d.registry_id = "my-agent".into();
        assert!(validate_custom_def(&d).is_ok());
        d.kind = "binary".into();
        assert!(validate_custom_def(&d).is_err(), "missing platform archive");
        // Go: runtime.GOOS + "-" + mapped GOARCH → "windows-x86_64".
        let platform = current_acp_platform();
        d.spec = json!({"binary": {platform.clone(): {"archive": "ftp://x", "cmd": "a"}}});
        assert!(validate_custom_def(&d).is_err(), "non-http archive");
        d.spec = json!({"binary": {platform: {"archive": "https://x/y.zip", "cmd": "a"}}});
        assert!(validate_custom_def(&d).is_ok());
    }

    #[test]
    fn not_installed_message_keeps_invariant_substring() {
        let message = custom_not_installed("MyAgent", None);
        assert!(message.contains("is not installed. Please install it in Agent Settings."));
        assert!(message.contains("设置 → 智能体"));
    }

    #[test]
    fn custom_def_reads_store_and_launch_roundtrip() {
        let root = temp_root("def");
        std::fs::write(
            root.join(".data").join("codeg-workspace.json"),
            json!({
                "customAgents": {
                    "hello": {"registryId": "hello", "name": "Hello", "description": "",
                              "version": "0.1.0", "distributionKind": "npx",
                              "spec": {"npx": {"package": "@x/hello", "args": ["--x"], "env": {"E": "1"}}},
                              "iconUrl": null, "skillsSharedStore": false, "skillsDir": null,
                              "source": "manual", "versionProbe": null, "supportsMcp": true}
                },
                "agents": {}, "next": 1
            })
            .to_string(),
        )
        .unwrap();
        let d = custom_def(&root, "custom:hello").expect("def resolves");
        assert_eq!(d.name, "Hello");
        assert_eq!(d.kind, "npx");
        assert_eq!(custom_installed_version(&root, &d), Value::Null);
        assert!(available_verdict(&root, &d).is_some());
        fs::remove_dir_all(root).ok();
    }
}
