//! The ACP adapter launch chain, ported from `cmd/serve/{wiring,cli,
//! provider_profiles,official_accounts,dsh_model,anthropic_base}.go`.
//!
//! The launch decides *which process spawns and with which environment*, so
//! every probe order and env key here is load-bearing: the adapter npm entry,
//! the vendor CLI resolution, the isolated `CODEX_HOME` copy rules, the
//! provider env injection and the subscription overlays. Byte differences in
//! hashed patch file names (`profile-<hash>.yml`) are documented divergences —
//! they only name files, and drivers live wholly on one side of the port at a
//! time (all acp commands are Rust-served once this module lands).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde_json::{json, Value};

use crate::backend::acp::agents::{home_dir, Agent};
use crate::backend::acp::custom_install;
use crate::backend::acp::providers_core::{self, ProviderDef};
use crate::backend::store_file;

/// Go `acp.Launch` (runtime.go:31). `env == None` inherits the parent env,
/// matching Go's nil.
#[derive(Clone, Debug, Default)]
pub struct Launch {
    pub command: String,
    pub args: Vec<String>,
    pub env: Option<Vec<String>>,
}

/// How the runtime obtains a launch for one agent. Production resolves via
/// `launch_fn`; tests point at the node fixture.
pub trait LaunchResolver: Send + Sync {
    fn launch(&self, agent: &Agent, permission: &str, cwd: &str) -> Result<Launch, String>;
    fn version(&self, agent: &Agent) -> String;
}

/// Production resolver rooted at a workspace (Go wiring.go:523-524).
pub struct ProductionResolver {
    pub root: PathBuf,
}

impl LaunchResolver for ProductionResolver {
    fn launch(&self, agent: &Agent, permission: &str, cwd: &str) -> Result<Launch, String> {
        launch_fn(&self.root, agent, permission, cwd)
    }
    fn version(&self, agent: &Agent) -> String {
        provider_version(&self.root, agent)
    }
}

/// Fixed launch for tests (the protocol fixture).
pub struct StaticLaunchResolver {
    pub launch: Launch,
}

impl LaunchResolver for StaticLaunchResolver {
    fn launch(&self, _agent: &Agent, _permission: &str, _cwd: &str) -> Result<Launch, String> {
        Ok(self.launch.clone())
    }
    fn version(&self, _agent: &Agent) -> String {
        String::new()
    }
}

// ----------------------------- CLI locating -----------------------------

fn npm_global() -> PathBuf {
    home_dir()
        .join("AppData")
        .join("Roaming")
        .join("npm")
        .join("node_modules")
}

fn acp_package_candidates(tool: &str) -> Vec<PathBuf> {
    match tool {
        "codex" => vec![
            npm_global()
                .join("@agentclientprotocol")
                .join("codex-acp")
                .join("dist")
                .join("index.js"),
            PathBuf::from("node_modules")
                .join("@agentclientprotocol")
                .join("codex-acp")
                .join("dist")
                .join("index.js"),
        ],
        "claude" => vec![
            npm_global()
                .join("@agentclientprotocol")
                .join("claude-agent-acp")
                .join("dist")
                .join("index.js"),
            PathBuf::from("node_modules")
                .join("@agentclientprotocol")
                .join("claude-agent-acp")
                .join("dist")
                .join("index.js"),
        ],
        "dsh" => vec![
            npm_global()
                .join("@deepseek-ai")
                .join("dsh")
                .join("lib")
                .join("bin.js"),
            PathBuf::from("node_modules")
                .join("@deepseek-ai")
                .join("dsh")
                .join("lib")
                .join("bin.js"),
        ],
        _ => Vec::new(),
    }
}

/// Go `configuredHarnessPath` (cli.go:125): `.data/harness-paths.json`.
pub fn configured_harness_path(root: &Path, tool: &str) -> String {
    let Ok(Some(data)) = store_file::read_json(&root.join(".data").join("harness-paths.json"))
    else {
        return String::new();
    };
    let p = data.get(tool).and_then(Value::as_str).unwrap_or("");
    if p.is_empty() || !Path::new(p).exists() {
        return String::new();
    }
    p.to_string()
}

/// Go `workBuddyEntry` (cli.go:92): prefer the desktop bundle's engine.
pub fn work_buddy_entry(root: &Path) -> String {
    let configured = configured_harness_path(root, "workbuddy");
    if !configured.is_empty() {
        return configured;
    }
    if let Ok(p) = std::env::var("HARNESS_HUB_WORKBUDDY_BIN") {
        if !p.is_empty() && Path::new(&p).exists() {
            return p;
        }
    }
    let mut roots: Vec<PathBuf> = Vec::new();
    if let Ok(home) = std::env::var("HARNESS_HUB_WORKBUDDY_HOME") {
        roots.push(PathBuf::from(home));
    }
    if let Ok(local) = std::env::var("LOCALAPPDATA") {
        let local = PathBuf::from(local);
        roots.push(local.join("Programs").join("WorkBuddy"));
        roots.push(local.join("WorkBuddy"));
    }
    if let Ok(found) = which("WorkBuddy.exe") {
        if let Some(dir) = Path::new(&found).parent() {
            roots.push(dir.to_path_buf());
        }
    }
    // Windows App Paths supports installations on a custom drive.
    if let Some(dir) = registered_workbuddy_directory() {
        roots.push(dir);
    }
    for root_dir in roots {
        let p = root_dir
            .join("resources")
            .join("app.asar.unpacked")
            .join("cli")
            .join("bin")
            .join("codebuddy");
        if p.exists() {
            return p.to_string_lossy().to_string();
        }
    }
    String::new()
}

#[cfg(windows)]
fn registered_workbuddy_directory() -> Option<PathBuf> {
    use windows::core::PCWSTR;
    use windows::Win32::System::Registry::{RegGetValueW, HKEY_CURRENT_USER, RRF_RT_REG_SZ};
    let key: Vec<u16> = r"Software\Microsoft\Windows\CurrentVersion\App Paths\WorkBuddy.exe"
        .encode_utf16()
        .chain(Some(0))
        .collect();
    let mut bytes = 0u32;
    unsafe {
        if RegGetValueW(
            HKEY_CURRENT_USER,
            PCWSTR(key.as_ptr()),
            PCWSTR::null(),
            RRF_RT_REG_SZ,
            None,
            None,
            Some(&mut bytes),
        )
        .0 != 0
        {
            return None;
        }
        let mut value = vec![0u16; bytes as usize / 2 + 1];
        if RegGetValueW(
            HKEY_CURRENT_USER,
            PCWSTR(key.as_ptr()),
            PCWSTR::null(),
            RRF_RT_REG_SZ,
            None,
            Some(value.as_mut_ptr().cast()),
            Some(&mut bytes),
        )
        .0 != 0
        {
            return None;
        }
        let end = value.iter().position(|c| *c == 0).unwrap_or(value.len());
        let path = String::from_utf16_lossy(&value[..end]);
        Path::new(path.trim().trim_matches('"'))
            .parent()
            .map(Path::to_path_buf)
    }
}

#[cfg(not(windows))]
fn registered_workbuddy_directory() -> Option<PathBuf> {
    None
}

/// `exec.LookPath` equivalent for one name (PATH lookup).
pub fn which(name: &str) -> Result<String, ()> {
    let candidate = Path::new(name);
    if candidate.is_file() {
        return Ok(name.to_string());
    }
    let path = std::env::var("PATH").unwrap_or_default();
    for dir in std::env::split_paths(&path) {
        let full = dir.join(name);
        if full.is_file() {
            return Ok(full.to_string_lossy().to_string());
        }
        // Go's LookPath also resolves extensioned names from bare names via
        // PATHEXT on Windows; cover the common .cmd/.exe cases.
        for ext in [".cmd", ".exe", ".bat"] {
            let with_ext = dir.join(format!("{name}{ext}"));
            if with_ext.is_file() {
                return Ok(with_ext.to_string_lossy().to_string());
            }
        }
    }
    let _ = candidate;
    Err(())
}

fn bin_candidates(tool: &str) -> Vec<PathBuf> {
    match tool {
        "codex" => vec![
            npm_global()
                .join("@openai")
                .join("codex")
                .join("bin")
                .join("codex.js"),
            home_dir()
                .join(".npm-global")
                .join("lib")
                .join("node_modules")
                .join("@openai")
                .join("codex")
                .join("bin")
                .join("codex.js"),
            PathBuf::from("/usr/local/lib/node_modules/@openai/codex/bin/codex.js"),
        ],
        "claude" => vec![
            npm_global()
                .join("@anthropic-ai")
                .join("claude-code")
                .join("bin")
                .join("claude.exe"),
            npm_global()
                .join("@anthropic-ai")
                .join("claude-code")
                .join("bin")
                .join("claude"),
            home_dir()
                .join(".npm-global")
                .join("lib")
                .join("node_modules")
                .join("@anthropic-ai")
                .join("claude-code")
                .join("bin")
                .join("claude"),
            PathBuf::from("/usr/local/lib/node_modules/@anthropic-ai/claude-code/bin/claude"),
        ],
        _ => Vec::new(),
    }
}

fn bin_env(tool: &str) -> &'static str {
    match tool {
        "codex" => "HARNESS_HUB_CODEX_BIN",
        "claude" => "HARNESS_HUB_CLAUDE_BIN",
        _ => "",
    }
}

/// Go `runtimeBin` (cli.go:196): npm installs whose global prefix is not
/// `%APPDATA%\npm` — the shim sits in the prefix root, the package under
/// `<prefix>\node_modules`.
fn runtime_bin(tool: &str) -> String {
    let Ok(shim) = which(&format!("{tool}.cmd")) else {
        return String::new();
    };
    let Some(prefix) = Path::new(&shim).parent() else {
        return String::new();
    };
    let pkg = match tool {
        "codex" => PathBuf::from("@openai")
            .join("codex")
            .join("bin")
            .join("codex.js"),
        "claude" => {
            let exe = PathBuf::from("@anthropic-ai")
                .join("claude-code")
                .join("bin")
                .join("claude.exe");
            if prefix.join("node_modules").join(&exe).exists() {
                exe
            } else {
                PathBuf::from("@anthropic-ai")
                    .join("claude-code")
                    .join("bin")
                    .join("claude")
            }
        }
        _ => return String::new(),
    };
    let p = prefix.join("node_modules").join(pkg);
    if !p.exists() {
        return String::new();
    }
    p.to_string_lossy().to_string()
}

/// Go `resolveBin` (cli.go:178).
pub fn resolve_bin(tool: &str) -> String {
    if let Ok(p) = std::env::var(bin_env(tool)) {
        if !p.is_empty() && Path::new(&p).exists() {
            return p;
        }
    }
    for p in bin_candidates(tool) {
        if p.exists() {
            return p.to_string_lossy().to_string();
        }
    }
    runtime_bin(tool)
}

/// Go `adapterVersion` (cli.go:144): walk up to four dirs for a package.json.
pub fn adapter_version(entry: &str) -> String {
    let mut dir = match Path::new(entry).parent() {
        Some(p) => p.to_path_buf(),
        None => return "local".into(),
    };
    for _ in 0..4 {
        if dir.as_os_str() == "." || dir.as_os_str().is_empty() {
            break;
        }
        if let Ok(body) = std::fs::read(dir.join("package.json")) {
            if let Ok(pkg) = serde_json::from_slice::<Value>(&body) {
                if let Some(v) = pkg.get("version").and_then(Value::as_str) {
                    if !v.is_empty() {
                        return v.to_string();
                    }
                }
            }
        }
        match dir.parent() {
            Some(parent) => dir = parent.to_path_buf(),
            None => break,
        }
    }
    "local".into()
}

/// Go `acpInstalled` (cli.go:221): the adapter entry this tool would run.
pub fn acp_installed(tool: &str) -> String {
    if tool == "workbuddy" {
        // The entry is resolved against the workspace root by the caller in
        // the seeded path; env-only probing here covers the direct callers.
        return work_buddy_entry(Path::new("."));
    }
    if tool == "dsh" {
        if let Ok(p) = std::env::var("HARNESS_HUB_DSH_BIN") {
            if !p.is_empty() && Path::new(&p).exists() {
                return p;
            }
        }
    }
    for p in acp_package_candidates(tool) {
        if p.exists() {
            return p.to_string_lossy().to_string();
        }
    }
    String::new()
}

/// Root-aware variant of `acp_installed` (dsh honours harness-paths.json).
pub fn acp_installed_rooted(root: &Path, tool: &str) -> String {
    if let Some(rest) = tool.strip_prefix("account:") {
        // 账号智能体的 ACP 适配器在仓库内（上游 wiring.go）。
        let file = crate::backend::account_channels::acp_adapter(root);
        return if file.is_file() {
            file.to_string_lossy().to_string()
        } else {
            String::new()
        };
    }
    if tool == "workbuddy" {
        return work_buddy_entry(root);
    }
    if tool == "dsh" {
        let configured = configured_harness_path(root, tool);
        if !configured.is_empty() {
            return configured;
        }
        if let Ok(p) = std::env::var("HARNESS_HUB_DSH_BIN") {
            if !p.is_empty() && Path::new(&p).exists() {
                return p;
            }
        }
    }
    let mut candidates = acp_package_candidates(tool);
    // The ROOT/node_modules candidates resolve against the workspace root.
    for c in candidates.iter_mut() {
        if c.starts_with("node_modules") {
            let rest = c.strip_prefix("node_modules").unwrap_or(&c).to_path_buf();
            *c = root.join("node_modules").join(rest);
        }
    }
    for p in candidates {
        if p.exists() {
            return p.to_string_lossy().to_string();
        }
    }
    String::new()
}

// ----------------------------- env plumbing -----------------------------

/// Go `dropEnvKey` (cli.go:295): a later append becomes the single value.
/// Go `ClearAPIEnvironment`（providers/auth.go:101）：账号智能体不继承任何
/// API 供应商的凭据 —— 它只用自己的协议进程与账号。
pub fn clear_api_environment(env: &[String]) -> Vec<String> {
    const BLOCKED: [&str; 8] = [
        "CLAUDE_CODE_SUBAGENT_MODEL",
        "OPENAI_API_KEY",
        "OPENAI_BASE_URL",
        "CODEX_API_KEY",
        "CODEPLUSE_CODEX_API_KEY",
        "CODEPLUSE_DSH_API_KEY",
        "CODEBUDDY_API_KEY",
        "CODEBUDDY_BASE_URL",
    ];
    env.iter()
        .filter(|entry| {
            let key = entry.split('=').next().unwrap_or_default().to_uppercase();
            !key.starts_with("ANTHROPIC_") && !BLOCKED.contains(&key.as_str())
        })
        .cloned()
        .collect()
}

pub fn drop_env_key(env: &[String], key: &str) -> Vec<String> {
    let prefix = format!("{key}=");
    env.iter()
        .filter(|e| !e.starts_with(&prefix))
        .cloned()
        .collect()
}

/// Go `anthropicBaseURL` (anthropic_base.go): trim full-endpoint spellings
/// down to the base the SDK expects.
pub fn anthropic_base_url(raw: &str) -> String {
    let mut u = raw.trim().to_string();
    if u.is_empty() {
        return u;
    }
    u = u.trim_end_matches('/').to_string();
    for suffix in ["/v1/messages", "/messages"] {
        if u.to_lowercase().ends_with(suffix) {
            u.truncate(u.len() - suffix.len());
            u = u.trim_end_matches('/').to_string();
        }
    }
    u
}

/// Go `codexHome` (cli.go:256): Codex's own settings in a writable workspace
/// copy; auth.json only refreshes when the source is newer.
pub fn codex_home(root: &Path, name: &str) -> Result<PathBuf, String> {
    let dir = if name.is_empty() {
        root.join(".data").join("codex-native-home")
    } else {
        root.join(".data").join("codex-homes").join(name)
    };
    std::fs::create_dir_all(&dir).map_err(|e| format!("无法创建 Codex 配置目录: {e}"))?;
    for file in ["auth.json", "config.toml"] {
        let src = Path::new(&providers_core::native_home("codex")).join(file);
        let data = match std::fs::read(&src) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            Err(e) => return Err(format!("读取 Codex {file} 失败: {e}")),
            Ok(data) => data,
        };
        let dst = dir.join(file);
        if file == "auth.json" {
            if let (Ok(dst_meta), Ok(src_meta)) = (std::fs::metadata(&dst), std::fs::metadata(&src))
            {
                // A source newer than the copy means the user logged in again
                // with the real CLI; otherwise keep the copy (Codex refreshes
                // tokens inside the isolated CODEX_HOME).
                if let (Ok(src_time), Ok(dst_time)) = (src_meta.modified(), dst_meta.modified()) {
                    if src_time <= dst_time {
                        continue;
                    }
                }
            }
        }
        std::fs::write(&dst, &data).map_err(|e| format!("写入 Codex {file} 失败: {e}"))?;
    }
    Ok(dir)
}

/// Go `buildEnv` (cli.go:305).
pub fn build_env(root: &Path, agent: &Agent) -> Result<Vec<String>, String> {
    let tool = agent.real_tool.as_str();
    let mut env: Vec<String> = std::env::vars().map(|(k, v)| format!("{k}={v}")).collect();
    env = drop_env_key(&env, "NODE_EXTRA_CA_CERTS");
    env.push("NO_COLOR=1".into());
    env.push("CI=1".into());
    if tool == "dsh" {
        if std::env::var("DSH_HOME")
            .map(|v| v.is_empty())
            .unwrap_or(true)
        {
            let local_home = root.join(".data").join("dsh-home");
            // Keep ACP startup isolated from the user's global ~/.dsh. The CLI
            // creates profile files (including profiles/acp/cordis.yml) during
            // initialize, so merely pointing at this directory when it already
            // exists makes first-run startup fall back to ~/.dsh and fail with
            // EPERM on locked-down Windows profiles.
            std::fs::create_dir_all(&local_home)
                .map_err(|e| format!("创建 DeepSeek Harness 工作目录失败: {e}"))?;
            env.push(format!("DSH_HOME={}", local_home.to_string_lossy()));
        }
    }
    if tool == "workbuddy" {
        env.push("CODEBUDDY_FORCE_LITE_WB_BUNDLE=1".into());
        if std::env::var("CODEBUDDY_CONFIG_DIR")
            .map(|v| v.is_empty())
            .unwrap_or(true)
        {
            env.push(format!(
                "CODEBUDDY_CONFIG_DIR={}",
                home_dir().join(".workbuddy").to_string_lossy()
            ));
        }
    }
    if tool == "claude" {
        let settings = home_dir().join(".claude").join("settings.json");
        if let Ok(data) = std::fs::read(&settings) {
            if let Ok(cfg) = serde_json::from_slice::<Value>(&data) {
                if let Some(env_map) = cfg.get("env").and_then(Value::as_object) {
                    for (k, v) in env_map {
                        if !k.starts_with("ANTHROPIC_") {
                            continue;
                        }
                        let already = std::env::var(k).map(|v| !v.is_empty()).unwrap_or(false);
                        if already {
                            continue;
                        }
                        let mut value = v.as_str().unwrap_or("").to_string();
                        if k == "ANTHROPIC_BASE_URL" {
                            value = anthropic_base_url(&value);
                        }
                        env.push(format!("{k}={value}"));
                    }
                }
            }
        }
        env.push(format!("CLAUDE_CODE_EXECUTABLE={}", resolve_bin("claude")));
    }
    if tool == "codex" {
        let isolate_off = std::env::var("HARNESS_HUB_CODEX_ISOLATE")
            .map(|v| v == "0")
            .unwrap_or(false);
        if !isolate_off {
            let dir = codex_home(root, &agent.wire)?;
            env = drop_env_key(&env, "CODEX_HOME");
            env.push(format!("CODEX_HOME={}", dir.to_string_lossy()));
        }
    }
    Ok(env)
}

// ----------------------------- provider resolution -----------------------------

/// Go `resolveProvider` (cli.go:52).
pub fn resolve_provider(root: &Path, agent: &Agent) -> Result<(ProviderDef, bool), String> {
    if agent.auth_mode == "subscription" {
        return Ok((ProviderDef::default(), false));
    }
    if !agent.provider_id.is_empty() {
        let p = providers_core::find(root, &agent.provider_id)?;
        let disabled = p.enabled.map(|e| !e).unwrap_or(false);
        if p.agent != agent.real_tool || !providers_core::is_api(&p) || disabled {
            return Err("固定的 API 供应商无效，请重新配置".into());
        }
        return Ok((p, true));
    }
    providers_core::active(root, &agent.real_tool)
}

/// Go `providerVersion` (cli.go:21): what the driver env is baked from.
pub fn provider_version(root: &Path, agent: &Agent) -> String {
    let tool = agent.real_tool.as_str();
    if agent.auth_mode == "subscription" {
        return format!(
            "subscription:{}:{}",
            providers_core::account_version(tool),
            agent.default_model
        );
    }
    if tool == "dsh" && agent.provider_id.is_empty() {
        return dsh_model_version();
    }
    if tool != "claude" && agent.provider_id.is_empty() {
        return "default".into();
    }
    let p = match resolve_provider(root, agent) {
        Err(_) => return "error".into(),
        Ok((_, false)) => return "default".into(),
        Ok((p, true)) => p,
    };
    // Deterministic fingerprint of the baked environment. The hash bytes
    // differ from Go's (different JSON encoder), which only renames pooled
    // drivers — one side owns the pool at a time.
    let mut hasher_input = Vec::new();
    for field in [
        &p.id,
        &p.name,
        &p.agent,
        &p.url,
        &p.model,
        &p.key,
        &p.source,
        &p.protocol,
    ] {
        hasher_input.extend_from_slice(field.as_bytes());
        hasher_input.push(0);
    }
    for (k, v) in &p.env {
        hasher_input.extend_from_slice(format!("{k}={v}").as_bytes());
        hasher_input.push(0);
    }
    for (id, name) in &p.models {
        hasher_input.extend_from_slice(format!("{id}\u{1}{name}").as_bytes());
        hasher_input.push(0);
    }
    if let Some(enabled) = p.enabled {
        hasher_input.extend_from_slice(format!("enabled={enabled}").as_bytes());
    }
    hasher_input.extend_from_slice(agent.default_model.as_bytes());
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(&hasher_input);
    format!("{:x}", hasher.finalize())
}

/// Go `applyCodexProvider` (provider_profiles.go:15): bind one provider into
/// the isolated CODEX_HOME config.
pub fn apply_codex_provider(
    root: &Path,
    agent: &Agent,
    env: Vec<String>,
) -> Result<Vec<String>, String> {
    let (p, _) = resolve_provider(root, agent)?;
    let mut dir = if agent.wire.is_empty() {
        root.join(".data").join("codex-native-home")
    } else {
        root.join(".data").join("codex-homes").join(&agent.wire)
    };
    // LaunchOverlay may already have written an explicit per-agent config.
    let prepared = env
        .iter()
        .any(|v| v.strip_prefix("CODEX_HOME=") == Some(dir.to_string_lossy().as_ref()));
    if !prepared {
        dir = codex_home(root, &agent.wire)?;
    }
    let file = dir.join("config.toml");
    let mut cfg: Value = match std::fs::read(&file) {
        Ok(data) => {
            let text = std::str::from_utf8(&data).map_err(|_| "Codex 配置格式无效".to_string())?;
            let parsed: toml::Value =
                toml::from_str(text).map_err(|_| "Codex 配置格式无效".to_string())?;
            providers_core::convert_toml(parsed)
        }
        Err(_) => json!({}),
    };
    let mut settings: BTreeMap<String, Value> = BTreeMap::new();
    if let Some(native) = p.native.as_object() {
        for (k, v) in native {
            settings.insert(k.clone(), v.clone());
        }
    }
    settings.insert("name".into(), json!(p.name));
    settings.insert("base_url".into(), json!(p.url));
    settings.insert("env_key".into(), json!("CODEPLUSE_CODEX_API_KEY"));
    settings.insert("requires_openai_auth".into(), json!(false));
    if !settings.contains_key("wire_api") {
        settings.insert("wire_api".into(), json!("responses"));
    }
    cfg["model_provider"] = json!("codepluse");
    let model = if agent.default_model.is_empty() {
        p.model.clone()
    } else {
        agent.default_model.clone()
    };
    cfg["model"] = json!(model);
    let mut configured: BTreeMap<String, Value> = BTreeMap::new();
    if let Some(existing) = cfg.get("model_providers").and_then(Value::as_object) {
        for (k, v) in existing {
            configured.insert(k.clone(), v.clone());
        }
    }
    configured.insert(
        "codepluse".into(),
        Value::Object(settings.into_iter().collect()),
    );
    cfg["model_providers"] = Value::Object(configured.into_iter().collect());
    let toml_body = toml::to_string_pretty(&toml_from_json(&cfg)?).map_err(|e| e.to_string())?;
    std::fs::write(&file, toml_body).map_err(|e| e.to_string())?;
    // API profiles must not inherit the native ChatGPT login copied by codexHome.
    let auth = json!({"OPENAI_API_KEY": p.key, "auth_mode": "apikey"});
    std::fs::write(dir.join("auth.json"), auth.to_string()).map_err(|e| e.to_string())?;
    let env = drop_env_key(&env, "CODEX_HOME");
    let env = drop_env_key(&env, "CODEPLUSE_CODEX_API_KEY");
    let mut env = env;
    env.push(format!("CODEX_HOME={}", dir.to_string_lossy()));
    env.push(format!("CODEPLUSE_CODEX_API_KEY={}", p.key));
    Ok(env)
}

/// Re-encode a JSON tree as TOML, preserving nested tables like go-toml does.
fn toml_from_json(value: &Value) -> Result<toml::Value, String> {
    fn convert(value: &Value) -> Result<toml::Value, String> {
        Ok(match value {
            Value::Null => toml::Value::String(String::new()),
            Value::Bool(b) => toml::Value::Boolean(*b),
            Value::Number(n) => {
                if let Some(i) = n.as_i64() {
                    toml::Value::Integer(i)
                } else {
                    toml::Value::Float(n.as_f64().unwrap_or(0.0))
                }
            }
            Value::String(s) => toml::Value::String(s.clone()),
            Value::Array(items) => {
                // TOML cannot mix tables and scalars in one array; Go's
                // marshaller would error the same way, so propagate the panic
                // path as a message.
                let mut out = Vec::new();
                for item in items {
                    out.push(convert(item)?);
                }
                toml::Value::Array(out)
            }
            Value::Object(map) => {
                // TOML wants plain values before tables (go-toml reorders
                // automatically; keep insertion order by building two maps).
                let mut scalars = toml::map::Map::new();
                let mut tables = toml::map::Map::new();
                for (k, v) in map {
                    if v.is_object() {
                        tables.insert(k.clone(), convert(v)?);
                    } else if v.is_array()
                        && v.as_array()
                            .map(|a| a.iter().any(Value::is_object))
                            .unwrap_or(false)
                    {
                        tables.insert(k.clone(), convert(v)?);
                    } else {
                        scalars.insert(k.clone(), convert(v)?);
                    }
                }
                let mut out = toml::map::Map::new();
                out.extend(scalars);
                out.extend(tables);
                toml::Value::Table(out)
            }
        })
    }
    convert(value)
}

fn yaml_from_json(value: &Value) -> serde_yaml::Value {
    match value {
        Value::Null => serde_yaml::Value::Null,
        Value::Bool(b) => serde_yaml::Value::Bool(*b),
        Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                serde_yaml::Value::Number(i.into())
            } else if let Some(f) = n.as_f64() {
                serde_yaml::Value::Number(f.into())
            } else {
                serde_yaml::Value::Null
            }
        }
        Value::String(s) => serde_yaml::Value::String(s.clone()),
        Value::Array(items) => {
            serde_yaml::Value::Sequence(items.iter().map(yaml_from_json).collect())
        }
        Value::Object(map) => {
            let mut out = serde_yaml::Mapping::new();
            for (k, v) in map {
                out.insert(serde_yaml::Value::String(k.clone()), yaml_from_json(v));
            }
            serde_yaml::Value::Mapping(out)
        }
    }
}

/// Go `dshProviderPatch` (provider_profiles.go:84): one pinned provider as a
/// DSH patch file + its API key env entry.
pub fn dsh_provider_patch(
    root: &Path,
    agent: &Agent,
    env: Vec<String>,
) -> Result<(String, Vec<String>), String> {
    let (p, _) = resolve_provider(root, agent)?;
    let mut cfg: BTreeMap<String, Value> = BTreeMap::new();
    if let Some(native) = p.native.as_object() {
        for (k, v) in native {
            cfg.insert(k.clone(), v.clone());
        }
    }
    cfg.insert("baseURL".into(), json!(p.url));
    cfg.insert("apiKeyEnv".into(), json!("CODEPLUSE_DSH_API_KEY"));
    cfg.remove("apiKey");
    if !cfg.contains_key("api") {
        cfg.insert(
            "api".into(),
            json!(match p.protocol.as_str() {
                "openai-responses" => "openai-responses",
                "anthropic" => "anthropic-messages",
                _ => "openai-completions",
            }),
        );
    }
    let model = if agent.default_model.is_empty() {
        p.model.clone()
    } else {
        agent.default_model.clone()
    };
    if !cfg.contains_key("models") {
        cfg.insert("models".into(), json!([{"id": model, "name": model}]));
    }
    let mut name = "codepluse".to_string();
    if let Some(rest) = p.id.strip_prefix("native:dsh:") {
        if let Some(last) = rest.split(':').next_back() {
            name = last.to_string();
        }
    }
    let provider_entry = {
        let mut providers = BTreeMap::new();
        providers.insert(
            name.clone(),
            Value::Object(cfg.clone().into_iter().collect()),
        );
        json!({"id": "llm-pi-ai", "config": {"providers": providers}})
    };
    let config = vec![
        provider_entry,
        json!({"id": "agent-default-model", "config": {"provider": name, "model": model}}),
        json!({"id": "acp", "config": {"provider": name, "model": model}}),
    ];
    let data = serde_yaml::to_string(&yaml_from_json(&json!(config))).map_err(|e| e.to_string())?;
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(data.as_bytes());
    let hash = hasher.finalize();
    let dir = root.join(".data").join("dsh-model-patches");
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    // Go names the file with the first 12 hex chars of the sha256.
    let dest = dir.join(format!("profile-{}.yml", hex_of(&hash[..6])));
    std::fs::write(&dest, &data).map_err(|e| e.to_string())?;
    let env = drop_env_key(&env, "CODEPLUSE_DSH_API_KEY");
    let mut env = env;
    env.push(format!("CODEPLUSE_DSH_API_KEY={}", p.key));
    Ok((dest.to_string_lossy().to_string(), env))
}

fn hex_of(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn dsh_original_home() -> PathBuf {
    if let Ok(v) = std::env::var("DSH_HOME") {
        if !v.is_empty() {
            return PathBuf::from(v);
        }
    }
    home_dir().join(".dsh")
}

/// Go `dshModelVersion` (dsh_model.go:17).
pub fn dsh_model_version() -> String {
    use sha2::{Digest, Sha256};
    let original = dsh_original_home();
    let mut hasher = Sha256::new();
    for p in [
        original
            .join("profiles")
            .join("desktop")
            .join("cordis.patch.yml"),
        original.join(".credentials.yaml"),
    ] {
        match std::fs::read(&p) {
            Ok(body) => hasher.update(&body),
            Err(e) => hasher.update(e.to_string().as_bytes()),
        }
    }
    format!("{:x}", hasher.finalize())
}

/// Go `dshDesktopPatch` (dsh_model.go:29): the desktop profile's llm plugins
/// plus an `acp` entry pinned to the same selection.
pub fn dsh_desktop_patch(root: &Path) -> Result<String, String> {
    let original = dsh_original_home();
    let file = original
        .join("profiles")
        .join("desktop")
        .join("cordis.patch.yml");
    let body = match std::fs::read(&file) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(String::new()),
        Err(_) => return Err("读取 DeepSeek Harness 模型配置失败".into()),
        Ok(body) => body,
    };
    let doc: serde_yaml::Value = serde_yaml::from_slice(&body)
        .map_err(|_| "解析 DeepSeek Harness 模型配置失败".to_string())?;
    let Some(items) = doc.as_sequence() else {
        return Err("DeepSeek Harness 配置格式无效".into());
    };
    let mut selected: Vec<serde_yaml::Value> = Vec::new();
    let mut selection_provider = String::new();
    let mut selection_model = String::new();
    for item in items {
        if !item.is_mapping() {
            continue;
        }
        let json_item = providers_core::convert_yaml(item.clone());
        let id = json_item.get("id").and_then(Value::as_str).unwrap_or("");
        if matches!(id, "llm-pi-ai" | "llm-deepseek" | "agent-default-model") {
            selected.push(item.clone());
        }
        if id == "agent-default-model" {
            let config = json_item.get("config").cloned().unwrap_or(Value::Null);
            selection_provider = config
                .get("provider")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            selection_model = config
                .get("model")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
        }
    }
    if selection_provider.is_empty() || selection_model.is_empty() {
        return Ok(String::new());
    }
    selected.push(
        serde_yaml::from_str(&format!(
            "id: acp\nconfig:\n  provider: {selection_provider}\n  model: {selection_model}\n"
        ))
        .map_err(|e| e.to_string())?,
    );
    let out =
        serde_yaml::to_string(&serde_yaml::Value::Sequence(selected)).map_err(|e| e.to_string())?;
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(out.as_bytes());
    let hash = hasher.finalize();
    let dir = root.join(".data").join("dsh-model-patches");
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let dest = dir.join(format!("desktop-{}.yml", hex_of(&hash[..4])));
    std::fs::write(&dest, &out).map_err(|e| e.to_string())?;
    // The isolated ACP profile needs the same managed credentials as the desktop.
    if std::env::var("DSH_HOME")
        .map(|v| v.is_empty())
        .unwrap_or(true)
    {
        let local = root.join(".data").join("dsh-home");
        if local.is_dir() {
            let source = original.join(".credentials.yaml");
            match std::fs::read(&source) {
                Ok(secret) => {
                    std::fs::write(local.join(".credentials.yaml"), secret)
                        .map_err(|e| format!("同步 DeepSeek Harness 凭据失败: {e}"))?;
                }
                Err(e) if e.kind() != std::io::ErrorKind::NotFound => {
                    return Err("读取 DeepSeek Harness 凭据失败".into());
                }
                Err(_) => {}
            }
        }
    }
    Ok(dest.to_string_lossy().to_string())
}

/// Go `applyOfficialAccount` (official_accounts.go:16): subscription profiles
/// bypass both the gateway and stored API credentials.
pub fn apply_official_account(
    root: &Path,
    agent: &Agent,
    env: Vec<String>,
) -> Result<Vec<String>, String> {
    let env = providers_core::clear_api_environment(&env);
    match agent.real_tool.as_str() {
        "codex" => {
            let source = providers_core::native_home("codex");
            let data = std::fs::read(Path::new(&source).join("auth.json")).map_err(|_| {
                "未读取到 Codex 官网登录，请先在 Codex 中执行 codex login".to_string()
            })?;
            let mut auth: Value =
                serde_json::from_slice(&data).map_err(|_| "Codex 登录文件格式无效".to_string())?;
            let tokens = auth.get("tokens").cloned().unwrap_or(Value::Null);
            let access = tokens
                .get("access_token")
                .and_then(Value::as_str)
                .unwrap_or("");
            if access.is_empty() {
                return Err("Codex 当前没有官网账号登录凭据；API Key 不会作为官网订阅使用".into());
            }
            if let Some(obj) = auth.as_object_mut() {
                obj.remove("OPENAI_API_KEY");
                obj.insert("auth_mode".into(), json!("chatgpt"));
            }
            let dir = codex_home(root, &agent.wire)?;
            // Keep a token refreshed by an earlier isolated session when newer.
            if let Ok(body) = std::fs::read(dir.join("auth.json")) {
                if let Ok(copied) = serde_json::from_slice::<Value>(&body) {
                    let copy_tokens = copied.get("tokens").cloned().unwrap_or(Value::Null);
                    let copy_access = copy_tokens
                        .get("access_token")
                        .and_then(Value::as_str)
                        .unwrap_or("");
                    if !copy_access.is_empty() {
                        let mut copied = copied;
                        if let Some(obj) = copied.as_object_mut() {
                            obj.remove("OPENAI_API_KEY");
                            obj.insert("auth_mode".into(), json!("chatgpt"));
                        }
                        auth = copied;
                    }
                }
            }
            std::fs::write(dir.join("auth.json"), auth.to_string()).map_err(|e| e.to_string())?;
            let mut cfg: Value = match std::fs::read(dir.join("config.toml")) {
                Ok(body) => {
                    let text = std::str::from_utf8(&body).map_err(|e| e.to_string())?;
                    let parsed: toml::Value = toml::from_str(text).map_err(|e| e.to_string())?;
                    providers_core::convert_toml(parsed)
                }
                Err(_) => json!({}),
            };
            cfg["model_provider"] = json!("openai");
            if let Some(obj) = cfg.as_object_mut() {
                for key in [
                    "profile",
                    "forced_login_method",
                    "openai_base_url",
                    "model_catalog_json",
                ] {
                    obj.remove(key);
                }
                if let Some(ps) = obj
                    .get_mut("model_providers")
                    .and_then(Value::as_object_mut)
                {
                    ps.remove("openai");
                }
            }
            if !agent.default_model.is_empty() {
                cfg["model"] = json!(agent.default_model);
            } else if let Some(obj) = cfg.as_object_mut() {
                obj.remove("model");
            }
            let body = toml::to_string_pretty(&toml_from_json(&cfg)?).map_err(|e| e.to_string())?;
            std::fs::write(dir.join("config.toml"), body).map_err(|e| e.to_string())?;
            let env = drop_env_key(&env, "CODEX_HOME");
            let mut env = env;
            env.push(format!("CODEX_HOME={}", dir.to_string_lossy()));
            Ok(env)
        }
        "claude" => {
            let source = providers_core::native_home("claude");
            let account = providers_core::account("claude");
            if !account
                .get("configured")
                .and_then(Value::as_bool)
                .unwrap_or(false)
            {
                return Err(
                    "未读取到 Claude 官网登录，请先在 Claude Code 中执行 claude auth login".into(),
                );
            }
            let dir = root.join(".data").join("claude-homes").join(&agent.wire);
            std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
            for name in [".credentials.json", "settings.json"] {
                let data = match std::fs::read(Path::new(&source).join(name)) {
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
                    Err(e) => return Err(e.to_string()),
                    Ok(data) => data,
                };
                let mut body = data.clone();
                if name == ".credentials.json" {
                    if let (Ok(src_time), Ok(dst_time)) = (
                        std::fs::metadata(Path::new(&source).join(name)).and_then(|m| m.modified()),
                        std::fs::metadata(dir.join(name)).and_then(|m| m.modified()),
                    ) {
                        if src_time <= dst_time {
                            continue;
                        }
                    }
                }
                if name == "settings.json" {
                    let mut cfg: Value = serde_json::from_slice(&data)
                        .map_err(|_| "Claude 设置格式无效".to_string())?;
                    if let Some(env_map) = cfg.get_mut("env").and_then(Value::as_object_mut) {
                        let keys: Vec<String> = env_map.keys().cloned().collect();
                        for key in keys {
                            let probe = vec![format!("{key}=value")];
                            if providers_core::clear_api_environment(&probe).is_empty() {
                                env_map.remove(&key);
                            }
                        }
                    }
                    if let Some(obj) = cfg.as_object_mut() {
                        for key in ["model", "apiKeyHelper", "forceLoginMethod"] {
                            obj.remove(key);
                        }
                    }
                    body = serde_json::to_vec(&cfg).unwrap_or_default();
                }
                std::fs::write(dir.join(name), &body).map_err(|e| e.to_string())?;
            }
            let env = drop_env_key(&env, "CLAUDE_CONFIG_DIR");
            let mut env = env;
            env.push(format!("CLAUDE_CONFIG_DIR={}", dir.to_string_lossy()));
            Ok(env)
        }
        _ => Ok(env),
    }
}

// ----------------------------- launch_fn -----------------------------

/// Go `launchFn` (wiring.go:218): the full launch decision for one agent.
pub fn launch_fn(
    root: &Path,
    agent: &Agent,
    permission: &str,
    cwd: &str,
) -> Result<Launch, String> {
    if let Some(launch) = custom_install::custom_agent_launch(root, agent)? {
        return Ok(launch);
    }
    if let Some(custom) = agents_acp_entry(root, &agent.id) {
        return Ok(Launch {
            command: custom.command,
            args: custom.args,
            env: Some(std::env::vars().map(|(k, v)| format!("{k}={v}")).collect()),
        });
    }
    let tool = agent.real_tool.clone();
    let pkg = acp_installed_rooted(root, &tool);
    if pkg.is_empty() {
        return Ok(Launch::default());
    }
    // 账号智能体：模型必须属于该渠道，且凭据只通过协议进程传递。
    if let Some(channel_id) = tool.strip_prefix("account:") {
        let channel_id = channel_id.to_string();
        let (channels, client) = crate::backend::account_channels::list(root)?;
        let Some(channel) = channels.iter().find(|item| item.id == channel_id) else {
            return Err("账号智能体不存在".into());
        };
        if !channel.configured || channel.models.is_empty() {
            return Err(format!("请先在智能体设置中登录 {}", channel.name));
        }
        let mut model = agent.default_model.clone();
        if model.is_empty() {
            model = channel.models[0].id.clone();
        }
        if !channel.models.iter().any(|item| item.id == model) {
            return Err(format!("{} 不提供模型 {}", channel.name, model));
        }
        let node = which("node").map_err(|_| "未找到 node，无法启动 ACP 适配器".to_string())?;
        let current: Vec<String> = std::env::vars()
            .map(|(key, value)| format!("{key}={value}"))
            .collect();
        let mut env = clear_api_environment(&current);
        env.push(format!("CODEPLUSE_CHANNEL_URL={}", client.url));
        env.push(format!("CODEPLUSE_CHANNEL_KEY={}", client.key));
        env.push(format!("CODEPLUSE_CHANNEL_MODEL={model}"));
        env.push(format!("CODEPLUSE_CHANNEL_PERMISSION={permission}"));
        return Ok(Launch {
            command: node,
            args: vec![
                pkg,
                channel_id,
                crate::backend::account_channels::data_dir(root)
                    .join("sessions")
                    .to_string_lossy()
                    .to_string(),
            ],
            env: Some(env),
        });
    }
    let node = which("node").map_err(|_| "未找到 node，无法启动 ACP 适配器".to_string())?;
    let mut env = build_env(root, agent)?;
    // Layer the settings-page overlay onto the launch environment and
    // materialize codex's config.toml/auth.json into its isolated CODEX_HOME.
    let overlay = overlay_for(root, agent);
    for (k, v) in &overlay {
        env = drop_env_key(&env, k);
        env.push(format!("{k}={v}"));
    }
    if tool == "codex" {
        let codex_toml = overlay_codex_toml(root, agent);
        let codex_auth = overlay_codex_auth(root, agent);
        if !codex_toml.is_empty() || !codex_auth.is_empty() {
            let dir = codex_home(root, &agent.wire)?;
            if !codex_toml.is_empty() {
                let _ = std::fs::write(dir.join("config.toml"), codex_toml.as_bytes());
            }
            if !codex_auth.is_empty() {
                let _ = std::fs::write(dir.join("auth.json"), codex_auth.as_bytes());
            }
        }
    }
    let mut args: Vec<String> = vec![pkg];
    if tool == "codex" {
        let mode = if permission == "workspace" {
            "workspace-write"
        } else {
            "read-only"
        };
        env.push(format!("INITIAL_AGENT_MODE={mode}"));
    }
    if tool == "claude" {
        env.push(format!("CLAUDE_CODE_EXECUTABLE={}", resolve_bin("claude")));
    }
    if tool == "claude" {
        let (provider, configured) =
            resolve_provider(root, agent).map_err(|e| format!("读取 API 供应商失败: {e}"))?;
        if configured {
            env = drop_env_key(&env, "CLAUDE_CODE_OAUTH_TOKEN");
            if !provider.env.is_empty() {
                let filtered: Vec<String> = env
                    .into_iter()
                    .filter(|item| {
                        let upper = item.to_uppercase();
                        !upper.starts_with("ANTHROPIC_")
                            && !upper.starts_with("CLAUDE_CODE_SUBAGENT_MODEL=")
                    })
                    .collect();
                env = filtered;
                for (key, value) in &provider.env {
                    let mut value = value.clone();
                    if key.eq_ignore_ascii_case("ANTHROPIC_BASE_URL") {
                        value = anthropic_base_url(&value);
                    }
                    env.push(format!("{key}={value}"));
                }
                if !provider.model.is_empty() && !provider.env.contains_key("ANTHROPIC_MODEL") {
                    env.push(format!("ANTHROPIC_MODEL={}", provider.model));
                }
            } else {
                for key in [
                    "ANTHROPIC_BASE_URL",
                    "ANTHROPIC_AUTH_TOKEN",
                    "ANTHROPIC_API_KEY",
                    "ANTHROPIC_MODEL",
                ] {
                    env = drop_env_key(&env, key);
                }
                env.push(format!(
                    "ANTHROPIC_BASE_URL={}",
                    anthropic_base_url(&provider.url)
                ));
                env.push(format!("ANTHROPIC_AUTH_TOKEN={}", provider.key));
                env.push(format!("ANTHROPIC_API_KEY={}", provider.key));
                env.push(format!("ANTHROPIC_MODEL={}", provider.model));
            }
            if !agent.default_model.is_empty() {
                env = drop_env_key(&env, "ANTHROPIC_MODEL");
                env.push(format!("ANTHROPIC_MODEL={}", agent.default_model));
            }
        }
    }
    if tool == "codex" && !agent.provider_id.is_empty() {
        env = apply_codex_provider(root, agent, env)?;
    }
    if agent.auth_mode == "subscription" {
        env = apply_official_account(root, agent, env)?;
    }
    if tool == "dsh" {
        if permission != "workspace" {
            return Err("DSH 不提供只读会话模式，请使用其原生工作区策略".into());
        }
        args.push("--profile".into());
        args.push("acp".into());
        if !agent.provider_id.is_empty() {
            let (patch, new_env) = dsh_provider_patch(root, agent, env)?;
            env = new_env;
            if !patch.is_empty() {
                args.push("--patch".into());
                args.push(patch);
            }
        } else {
            let patch = dsh_desktop_patch(root)?;
            if !patch.is_empty() {
                args.push("--patch".into());
                args.push(patch);
            }
        }
    }
    if tool == "workbuddy" {
        // The desktop lite bundle registers ACP through its server strategy.
        args.extend([
            "--serve".into(),
            "--acp".into(),
            "--host".into(),
            "127.0.0.1".into(),
        ]);
    }
    Ok(Launch {
        command: node,
        args,
        env: Some(env),
    })
}

/// Go `LaunchOverlay` (agentconfig.go:126): the settings-page env overlay plus
/// the codex config/auth text for one agent, read from `Store.Agents`.
fn overlay_for(root: &Path, agent: &Agent) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    let Ok(Some(doc)) = store_file::read_json(&root.join(".data").join("codeg-workspace.json"))
    else {
        return out;
    };
    let Some(ov) = doc
        .get("agents")
        .and_then(|a| a.get(&crate::backend::acp::agents::wire_agent(agent)))
    else {
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

fn overlay_codex_toml(root: &Path, agent: &Agent) -> String {
    let Ok(Some(doc)) = store_file::read_json(&root.join(".data").join("codeg-workspace.json"))
    else {
        return String::new();
    };
    doc.get("agents")
        .and_then(|a| a.get(&crate::backend::acp::agents::wire_agent(agent)))
        .and_then(|ov| ov.get("codex_config_toml"))
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string()
}

fn overlay_codex_auth(root: &Path, agent: &Agent) -> String {
    let Ok(Some(doc)) = store_file::read_json(&root.join(".data").join("codeg-workspace.json"))
    else {
        return String::new();
    };
    doc.get("agents")
        .and_then(|a| a.get(&crate::backend::acp::agents::wire_agent(agent)))
        .and_then(|ov| ov.get("codex_auth_json"))
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string()
}

fn agents_acp_entry(root: &Path, id: &str) -> Option<crate::backend::acp::agents::CustomAcpEntry> {
    crate::backend::acp::agents::custom_acp_launch_entry(root, id)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn temp_root(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "acp-launch-{tag}-{}",
            crate::backend::goformat::hex_id()
        ));
        fs::create_dir_all(dir.join(".data")).unwrap();
        dir
    }

    #[test]
    fn anthropic_base_url_trims_endpoint_suffixes() {
        assert_eq!(
            anthropic_base_url("https://a.com/v1/messages"),
            "https://a.com"
        );
        assert_eq!(
            anthropic_base_url("https://a.com/messages"),
            "https://a.com"
        );
        assert_eq!(anthropic_base_url("https://a.com/"), "https://a.com");
        assert_eq!(anthropic_base_url("https://a.com/v1"), "https://a.com/v1");
        assert_eq!(anthropic_base_url("  "), "");
    }

    #[test]
    fn drop_env_key_removes_all_occurrences() {
        let env = vec!["K=1".into(), "A=2".into(), "K=3".into()];
        assert_eq!(drop_env_key(&env, "K"), vec!["A=2".to_string()]);
    }

    #[test]
    fn provider_version_branches() {
        let root = temp_root("version");
        let mut a = Agent::default();
        a.real_tool = "codex".into();
        assert_eq!(provider_version(&root, &a), "default");
        a.real_tool = "dsh".into();
        // DSH hashes its home files; deterministic for a fixed home
        let v1 = provider_version(&root, &a);
        let v2 = provider_version(&root, &a);
        assert_eq!(v1, v2);
        assert_eq!(v1.len(), 64, "sha256 hex");
        a.auth_mode = "subscription".into();
        assert!(provider_version(&root, &a).starts_with("subscription:"));
        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn resolve_bin_and_installed_shape() {
        // On a machine without the CLIs these return "" without panicking;
        // with them installed the path must point at an existing file.
        let claude = resolve_bin("claude");
        if !claude.is_empty() {
            assert!(Path::new(&claude).exists());
        }
        let codex_pkg = acp_package_candidates("codex");
        assert_eq!(codex_pkg.len(), 2);
    }

    #[test]
    fn account_launch_needs_the_adapter_inside_the_repo() {
        let root = temp_root("account-launch");
        assert!(acp_installed_rooted(&root, "account:workbuddy").is_empty());
        let dir = root.join("bridges").join("account-channels");
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("acp-agent.mjs"), "//").unwrap();
        assert!(acp_installed_rooted(&root, "account:workbuddy").ends_with("acp-agent.mjs"));
        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn clear_api_environment_drops_provider_credentials() {
        let env = vec![
            "PATH=C:\\bin".to_string(),
            "ANTHROPIC_API_KEY=secret".to_string(),
            "ANTHROPIC_BASE_URL=https://x".to_string(),
            "CLAUDE_CODE_SUBAGENT_MODEL=haiku".to_string(),
            "OPENAI_API_KEY=secret".to_string(),
            "OPENAI_BASE_URL=https://y".to_string(),
            "CODEX_API_KEY=secret".to_string(),
            "CODEPLUSE_CODEX_API_KEY=secret".to_string(),
            "CODEPLUSE_DSH_API_KEY=secret".to_string(),
            "CODEBUDDY_API_KEY=secret".to_string(),
            "CODEBUDDY_BASE_URL=https://z".to_string(),
            "HOME=C:\\Users\\me".to_string(),
        ];
        let kept = clear_api_environment(&env);
        assert!(kept.contains(&"PATH=C:\\bin".to_string()));
        assert!(kept.contains(&"HOME=C:\\Users\\me".to_string()));
        for dropped in [
            "ANTHROPIC_API_KEY=secret",
            "ANTHROPIC_BASE_URL=https://x",
            "CLAUDE_CODE_SUBAGENT_MODEL=haiku",
            "OPENAI_API_KEY=secret",
            "OPENAI_BASE_URL=https://y",
            "CODEX_API_KEY=secret",
            "CODEPLUSE_CODEX_API_KEY=secret",
            "CODEPLUSE_DSH_API_KEY=secret",
            "CODEBUDDY_API_KEY=secret",
            "CODEBUDDY_BASE_URL=https://z",
        ] {
            assert!(!kept.contains(&dropped.to_string()), "{dropped} 未被清除");
        }
    }
}
