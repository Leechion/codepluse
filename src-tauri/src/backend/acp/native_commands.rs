//! Native ACP settings and helper commands that do not belong to a live session.

use std::fs;
use std::io::{BufRead, BufReader, Read};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use serde_json::{json, Value};

use super::agents::home_dir;

struct HermesProvider { id: &'static str, key_env_var: &'static str, needs_base_url: bool, base_url_env_var: &'static str }
const HERMES_PROVIDERS: &[HermesProvider] = &[
    HermesProvider { id: "openrouter", key_env_var: "OPENROUTER_API_KEY", needs_base_url: false, base_url_env_var: "" },
    HermesProvider { id: "openai-api", key_env_var: "OPENAI_API_KEY", needs_base_url: true, base_url_env_var: "OPENAI_BASE_URL" },
    HermesProvider { id: "custom", key_env_var: "", needs_base_url: true, base_url_env_var: "" },
    HermesProvider { id: "anthropic", key_env_var: "ANTHROPIC_API_KEY", needs_base_url: false, base_url_env_var: "" },
    HermesProvider { id: "gemini", key_env_var: "GOOGLE_API_KEY", needs_base_url: false, base_url_env_var: "GEMINI_BASE_URL" },
    HermesProvider { id: "deepseek", key_env_var: "DEEPSEEK_API_KEY", needs_base_url: false, base_url_env_var: "DEEPSEEK_BASE_URL" },
    HermesProvider { id: "xai", key_env_var: "XAI_API_KEY", needs_base_url: false, base_url_env_var: "XAI_BASE_URL" },
    HermesProvider { id: "zai", key_env_var: "GLM_API_KEY", needs_base_url: false, base_url_env_var: "GLM_BASE_URL" },
    HermesProvider { id: "minimax", key_env_var: "MINIMAX_API_KEY", needs_base_url: false, base_url_env_var: "MINIMAX_BASE_URL" },
    HermesProvider { id: "minimax-cn", key_env_var: "MINIMAX_CN_API_KEY", needs_base_url: false, base_url_env_var: "MINIMAX_CN_BASE_URL" },
    HermesProvider { id: "kimi-coding", key_env_var: "KIMI_API_KEY", needs_base_url: false, base_url_env_var: "KIMI_BASE_URL" },
    HermesProvider { id: "kimi-coding-cn", key_env_var: "KIMI_CN_API_KEY", needs_base_url: false, base_url_env_var: "" },
    HermesProvider { id: "nvidia", key_env_var: "NVIDIA_API_KEY", needs_base_url: false, base_url_env_var: "NVIDIA_BASE_URL" },
    HermesProvider { id: "alibaba", key_env_var: "DASHSCOPE_API_KEY", needs_base_url: false, base_url_env_var: "DASHSCOPE_BASE_URL" },
    HermesProvider { id: "alibaba-coding-plan", key_env_var: "ALIBABA_CODING_PLAN_API_KEY", needs_base_url: false, base_url_env_var: "ALIBABA_CODING_PLAN_BASE_URL" },
    HermesProvider { id: "copilot", key_env_var: "GITHUB_TOKEN", needs_base_url: false, base_url_env_var: "" },
    HermesProvider { id: "lmstudio", key_env_var: "LM_API_KEY", needs_base_url: true, base_url_env_var: "LM_BASE_URL" },
    HermesProvider { id: "azure-foundry", key_env_var: "AZURE_FOUNDRY_API_KEY", needs_base_url: true, base_url_env_var: "AZURE_FOUNDRY_BASE_URL" },
    HermesProvider { id: "stepfun", key_env_var: "STEPFUN_API_KEY", needs_base_url: false, base_url_env_var: "STEPFUN_BASE_URL" },
    HermesProvider { id: "arcee", key_env_var: "ARCEEAI_API_KEY", needs_base_url: false, base_url_env_var: "ARCEE_BASE_URL" },
    HermesProvider { id: "gmi", key_env_var: "GMI_API_KEY", needs_base_url: false, base_url_env_var: "GMI_BASE_URL" },
    HermesProvider { id: "huggingface", key_env_var: "HF_TOKEN", needs_base_url: false, base_url_env_var: "HF_BASE_URL" },
    HermesProvider { id: "kilocode", key_env_var: "KILOCODE_API_KEY", needs_base_url: false, base_url_env_var: "KILOCODE_BASE_URL" },
    HermesProvider { id: "opencode-zen", key_env_var: "OPENCODE_ZEN_API_KEY", needs_base_url: false, base_url_env_var: "OPENCODE_ZEN_BASE_URL" },
    HermesProvider { id: "opencode-go", key_env_var: "OPENCODE_GO_API_KEY", needs_base_url: false, base_url_env_var: "OPENCODE_GO_BASE_URL" },
    HermesProvider { id: "xiaomi", key_env_var: "XIAOMI_API_KEY", needs_base_url: false, base_url_env_var: "XIAOMI_BASE_URL" },
    HermesProvider { id: "tencent-tokenhub", key_env_var: "TOKENHUB_API_KEY", needs_base_url: false, base_url_env_var: "TOKENHUB_BASE_URL" },
    HermesProvider { id: "ollama-cloud", key_env_var: "OLLAMA_API_KEY", needs_base_url: false, base_url_env_var: "OLLAMA_BASE_URL" },
    HermesProvider { id: "novita", key_env_var: "NOVITA_API_KEY", needs_base_url: false, base_url_env_var: "" },
    HermesProvider { id: "ai-gateway", key_env_var: "AI_GATEWAY_API_KEY", needs_base_url: false, base_url_env_var: "AI_GATEWAY_BASE_URL" },
    HermesProvider { id: "nous", key_env_var: "", needs_base_url: false, base_url_env_var: "" },
    HermesProvider { id: "openai-codex", key_env_var: "", needs_base_url: false, base_url_env_var: "" },
    HermesProvider { id: "minimax-oauth", key_env_var: "", needs_base_url: false, base_url_env_var: "" },
    HermesProvider { id: "xai-oauth", key_env_var: "", needs_base_url: false, base_url_env_var: "" },
    HermesProvider { id: "qwen-oauth", key_env_var: "", needs_base_url: false, base_url_env_var: "" },
    HermesProvider { id: "google-gemini-cli", key_env_var: "", needs_base_url: false, base_url_env_var: "" },
    HermesProvider { id: "copilot-acp", key_env_var: "", needs_base_url: false, base_url_env_var: "" },
    HermesProvider { id: "bedrock", key_env_var: "", needs_base_url: false, base_url_env_var: "" },
    HermesProvider { id: "vertex", key_env_var: "", needs_base_url: false, base_url_env_var: "" },
];

pub fn owns(command: &str) -> bool {
    matches!(
        command,
        "acp_update_hermes_config"
            | "acp_open_hermes_setup_terminal"
            | "acp_reveal_hermes_home"
            | "acp_install_pi_binary"
            | "acp_uninstall_pi_binary"
    )
}

pub fn handle(_root: &Path, command: &str, args: &Value) -> Option<Result<Value, String>> {
    if !owns(command) {
        return None;
    }
    Some(match command {
        "acp_update_hermes_config" => update_hermes_config(args),
        "acp_open_hermes_setup_terminal" => open_hermes_terminal(args),
        "acp_reveal_hermes_home" => reveal_hermes_home(),
        "acp_install_pi_binary" | "acp_uninstall_pi_binary" => install_pi(command, args),
        _ => unreachable!(),
    })
}

fn install_pi(command: &str, args: &Value) -> Result<Value, String> {
    let task_id = text(args, "taskId").unwrap_or("").trim();
    if task_id.is_empty() || task_id.len() > 256 {
        return Err("安装任务编号无效".into());
    }
    let installing = command == "acp_install_pi_binary";
    let action = if installing { "install" } else { "uninstall" };
    crate::backend::emit_event(
        "app://agent-install",
        json!({"task_id": task_id, "agent_type": "pi", "kind": "started", "payload": if installing { "正在通过 npm 安装 Pi" } else { "正在通过 npm 卸载 Pi" }}),
    );
    #[cfg(windows)]
    let mut command_builder = {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        let mut command = Command::new("cmd.exe");
        command
            .args(["/D", "/S", "/C", if installing {
                "npm install --global @earendil-works/pi-coding-agent"
            } else {
                "npm uninstall --global @earendil-works/pi-coding-agent"
            }])
            .creation_flags(CREATE_NO_WINDOW);
        command
    };
    #[cfg(not(windows))]
    let mut command_builder = {
        let mut command = Command::new("npm");
        command.args([if installing { "install" } else { "uninstall" }, "--global", "@earendil-works/pi-coding-agent"]);
        command
    };
    let result = run_pi_npm(&mut command_builder, task_id, action);
    let (kind, payload) = match &result {
        Ok(_) => ("completed", if installing { "Pi 已安装".to_string() } else { "Pi 已卸载".to_string() }),
        Err(error) => ("failed", error.clone()),
    };
    crate::backend::emit_event(
        "app://agent-install",
        json!({"task_id": task_id, "agent_type": "pi", "kind": kind, "payload": payload}),
    );
    result
}

fn run_pi_npm(command: &mut Command, task_id: &str, action: &str) -> Result<Value, String> {
    let mut child = command
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("无法启动 npm：{e}"))?;
    let mut readers = Vec::new();
    if let Some(stdout) = child.stdout.take() {
        readers.push(stream_install_logs(stdout, task_id.to_string()));
    }
    if let Some(stderr) = child.stderr.take() {
        readers.push(stream_install_logs(stderr, task_id.to_string()));
    }
    let started = Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) => {}
            Err(error) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(format!("读取 npm 状态失败：{error}"));
            }
        }
        if started.elapsed() > Duration::from_secs(9 * 60) {
            let _ = child.kill();
            let _ = child.wait();
            return Err(format!("npm {action} Pi 超过 9 分钟，已终止安装进程"));
        }
        thread::sleep(Duration::from_millis(200));
    };
    for reader in readers {
        let _ = reader.join();
    }
    if status.success() {
        Ok(Value::Null)
    } else {
        Err(format!("npm {action} Pi 失败（退出码 {:?}）", status.code()))
    }
}

fn stream_install_logs<R: Read + Send + 'static>(stream: R, task_id: String) -> thread::JoinHandle<()> {
    thread::spawn(move || {
        for line in BufReader::new(stream).lines().map_while(Result::ok) {
            crate::backend::emit_event(
                "app://agent-install",
                json!({"task_id": task_id, "agent_type": "pi", "kind": "log", "payload": line}),
            );
        }
    })
}

fn hermes_home() -> PathBuf {
    home_dir().join(".hermes")
}

fn text<'a>(args: &'a Value, key: &str) -> Option<&'a str> {
    args.get(key).and_then(Value::as_str)
}

fn update_hermes_config(args: &Value) -> Result<Value, String> {
    let provider = text(args, "provider").unwrap_or("").trim();
    if !valid_provider(provider) {
        return Err("不支持的 Hermes 模型提供方".into());
    }
    if needs_base_url(provider) {
        let base = text(args, "baseUrl").unwrap_or("").trim();
        if base.is_empty() || url::Url::parse(base).map(|u| !matches!(u.scheme(), "http" | "https")).unwrap_or(true) {
            return Err("此 Hermes 提供方需要有效的 HTTP(S) API 地址".into());
        }
    }
    let dir = hermes_home();
    fs::create_dir_all(&dir).map_err(|e| format!("创建 Hermes 配置目录失败：{e}"))?;
    let config_path = dir.join("config.yaml");
    if let Some(raw) = text(args, "rawConfigYaml") {
        if raw.len() > 2 * 1024 * 1024 {
            return Err("Hermes 配置超过 2 MiB".into());
        }
        serde_yaml::from_str::<serde_yaml::Value>(raw)
            .map_err(|e| format!("Hermes YAML 格式无效：{e}"))?;
        atomic_write(&config_path, raw.as_bytes())?;
        return Ok(Value::Null);
    }

    let mut config: serde_yaml::Value = match fs::read_to_string(&config_path) {
        Ok(raw) => serde_yaml::from_str(&raw).map_err(|e| format!("Hermes YAML 格式无效：{e}"))?,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => serde_yaml::Value::Mapping(Default::default()),
        Err(e) => return Err(format!("读取 Hermes 配置失败：{e}")),
    };
    if !config.is_mapping() {
        return Err("Hermes 配置根节点必须是 YAML 对象".into());
    }
    let model = mapping_child(&mut config, "model")?;
    model.insert(yaml_key("provider"), yaml_string(provider));
    if let Some(value) = text(args, "model") {
        set_optional(model, "model", value)?;
    }
    if let Some(value) = text(args, "baseUrl") {
        set_optional(model, "base_url", value)?;
    }
    if provider == "custom" {
        if let Some(value) = text(args, "apiKey") {
            set_optional(model, "api_key", value)?;
        }
    }
    let raw = serde_yaml::to_string(&config).map_err(|e| format!("序列化 Hermes 配置失败：{e}"))?;
    atomic_write(&config_path, raw.as_bytes())?;

    if provider != "custom" {
        if let Some(api_key) = text(args, "apiKey") {
            let env_key = provider_env(provider).ok_or("此 Hermes 提供方使用登录授权，不能保存 API Key")?;
            update_env_file(&dir.join(".env"), env_key, api_key)?;
        }
    }
    Ok(Value::Null)
}

fn needs_base_url(provider: &str) -> bool {
    HERMES_PROVIDERS.iter().find(|row| row.id == provider).is_some_and(|row| row.needs_base_url)
}

fn valid_provider(provider: &str) -> bool {
    HERMES_PROVIDERS.iter().any(|row| row.id == provider)
}

fn provider_env(provider: &str) -> Option<&'static str> {
    HERMES_PROVIDERS.iter().find(|row| row.id == provider).map(|row| row.key_env_var).filter(|key| !key.is_empty())
}

fn update_env_file(path: &Path, key: &str, value: &str) -> Result<(), String> {
    if value.len() > 64 * 1024 || value.contains(['\r', '\n', '\0']) {
        return Err("API Key 格式无效".into());
    }
    let old = match fs::read_to_string(path) {
        Ok(s) => s,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(e) => return Err(format!("读取 Hermes 环境变量失败：{e}")),
    };
    let mut out = String::new();
    let mut replaced = false;
    for line in old.lines() {
        let candidate = line.trim_start().strip_prefix("export ").unwrap_or(line.trim_start());
        if candidate.split_once('=').map(|(name, _)| name.trim()) == Some(key) {
            if !replaced {
                out.push_str(&format!("{key}={}\n", env_quote(value)));
                replaced = true;
            }
        } else {
            out.push_str(line);
            out.push('\n');
        }
    }
    if !replaced {
        out.push_str(&format!("{key}={}\n", env_quote(value)));
    }
    atomic_write(path, out.as_bytes())
}

fn env_quote(value: &str) -> String {
    if value.bytes().all(|b| b.is_ascii_alphanumeric() || b"_./:-".contains(&b)) {
        value.to_string()
    } else {
        format!("\"{}\"", value.replace('\\', "\\\\").replace('"', "\\\""))
    }
}

fn mapping_child<'a>(root: &'a mut serde_yaml::Value, key: &str) -> Result<&'a mut serde_yaml::Mapping, String> {
    let map = root.as_mapping_mut().ok_or("Hermes 配置根节点必须是 YAML 对象")?;
    let value = map.entry(yaml_key(key)).or_insert_with(|| serde_yaml::Value::Mapping(Default::default()));
    value.as_mapping_mut().ok_or_else(|| format!("Hermes 配置 {key} 必须是 YAML 对象"))
}

fn set_optional(map: &mut serde_yaml::Mapping, key: &str, value: &str) -> Result<(), String> {
    if value.len() > 64 * 1024 || value.contains(['\r', '\n', '\0']) {
        return Err(format!("{key} 格式无效"));
    }
    if value.is_empty() {
        map.remove(yaml_key(key));
    } else {
        map.insert(yaml_key(key), yaml_string(value));
    }
    Ok(())
}

fn yaml_key(key: &str) -> serde_yaml::Value { serde_yaml::Value::String(key.into()) }
fn yaml_string(value: &str) -> serde_yaml::Value { serde_yaml::Value::String(value.into()) }

fn atomic_write(path: &Path, bytes: &[u8]) -> Result<(), String> {
    let dir = path.parent().unwrap_or_else(|| Path::new("."));
    let temp = dir.join(format!(".codepluse-hermes-{}.tmp", std::process::id()));
    fs::write(&temp, bytes).map_err(|e| format!("写入配置临时文件失败：{e}"))?;
    #[cfg(windows)]
    let replaced = {
        use std::os::windows::ffi::OsStrExt;
        use windows::core::PCWSTR;
        use windows::Win32::Storage::FileSystem::{MoveFileExW, MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH};
        let from: Vec<u16> = temp.as_os_str().encode_wide().chain(Some(0)).collect();
        let to: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
        unsafe { MoveFileExW(PCWSTR::from_raw(from.as_ptr()), PCWSTR::from_raw(to.as_ptr()), MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH).is_ok() }
    };
    #[cfg(not(windows))]
    let replaced = fs::rename(&temp, path).is_ok();
    if !replaced {
        let error = std::io::Error::last_os_error();
        let _ = fs::remove_file(&temp);
        return Err(format!("保存配置失败：{error}"));
    }
    Ok(())
}

fn open_hermes_terminal(args: &Value) -> Result<Value, String> {
    let kind = text(args, "kind").unwrap_or("");
    let subcommand = match kind { "setup" => "setup", "model" => "model", _ => return Err("不支持的 Hermes 终端操作".into()) };
    let dir = hermes_home();
    fs::create_dir_all(&dir).map_err(|e| format!("创建 Hermes 目录失败：{e}"))?;
    #[cfg(windows)]
    {
        let mut command = Command::new("cmd.exe");
        command.args(["/K", "hermes", subcommand]).current_dir(&dir);
        use std::os::windows::process::CommandExt;
        const CREATE_NEW_CONSOLE: u32 = 0x0000_0010;
        command.creation_flags(CREATE_NEW_CONSOLE);
        command.spawn().map_err(|e| format!("启动 Hermes 终端失败：{e}"))?;
    }
    #[cfg(target_os = "macos")]
    {
        let script = format!("tell application \"Terminal\" to do script \"cd '{}' && hermes {}\"", dir.display().to_string().replace('\'', "'\\''"), subcommand);
        Command::new("osascript").args(["-e", &script]).spawn().map_err(|e| format!("启动 Hermes 终端失败：{e}"))?;
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        let result = Command::new("x-terminal-emulator").args(["-e", "hermes", subcommand]).current_dir(&dir).spawn();
        if result.is_err() { return Err("找不到可用的图形终端".into()); }
    }
    Ok(Value::Null)
}

fn reveal_hermes_home() -> Result<Value, String> {
    let dir = hermes_home();
    fs::create_dir_all(&dir).map_err(|e| format!("创建 Hermes 目录失败：{e}"))?;
    #[cfg(windows)]
    Command::new("explorer.exe").arg(&dir).spawn().map_err(|e| format!("打开 Hermes 目录失败：{e}"))?;
    #[cfg(target_os = "macos")]
    Command::new("open").arg(&dir).spawn().map_err(|e| format!("打开 Hermes 目录失败：{e}"))?;
    #[cfg(all(unix, not(target_os = "macos")))]
    Command::new("xdg-open").arg(&dir).spawn().map_err(|e| format!("打开 Hermes 目录失败：{e}"))?;
    Ok(json!(null))
}
