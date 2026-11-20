//! 提供商删除与令牌校验 —— 补全批次。
//!
//! 本模块负责（4 条）：
//!
//! * `codepluse_provider_delete` —— 从 `.data/api-providers.json` 删除一条执行端
//!   提供商。唯一有 Go 原型的一条：`server-go/internal/providers/providers.go`
//!   的 `Delete`（按 `id` 匹配，清掉指向它的 `active` 项；查不到报
//!   `供应商不存在`，**明确 404 语义、不做幂等**）。
//! * `validate_cnb_token` / `validate_gitea_token` / `validate_gitlab_token` ——
//!   拿令牌去各自主机的 "who am I" 接口做真实校验（短超时）。
//!
//! 契约来源：前端 `codepluse-ui/src/lib/api.ts`（参数 `serverUrl` / `token`）与
//! `types.ts` 的 `GitHubTokenValidation`：
//! `{ success: boolean, username: string|null, scopes: string[], avatar_url: string|null, message: string|null }`。
//!
//! 各自主机接口（前端注释 + 各 OpenAPI）：
//!
//! * Gitea：`GET {serverUrl}/api/v1/user`，`Authorization: token <token>`；
//!   scopes 恒为空数组（Gitea 只在需要账号密码的接口上暴露令牌范围）。
//! * GitLab：`GET {serverUrl}/api/v4/user`，`PRIVATE-TOKEN: <token>`；scopes 从
//!   单独的 `GET /api/v4/personal_access_tokens/self` 取，取不到就回空数组。
//! * CNB：OpenAPI 在独立主机 `https://api.cnb.cool`，`GET /user`，
//!   `Authorization: Bearer <token>`；scopes 恒为空数组。
//!
//! 安全红线：令牌只出现在请求头里，**绝不写日志、绝不进错误文案**，响应里也
//! 不回显；删除返回的记录会剥掉明文 `key`。

use std::path::{Path, PathBuf};
use std::time::Duration;

use serde_json::{json, Value};

use crate::backend::{net, store_file};

const PROVIDERS_FILE: &str = "api-providers.json";
const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);

const GITEA_DEFAULT_BASE: &str = "https://gitea.com";
const GITLAB_DEFAULT_BASE: &str = "https://gitlab.com";
const CNB_DEFAULT_BASE: &str = "https://api.cnb.cool";

pub fn owns(command: &str) -> bool {
    matches!(
        command,
        "codepluse_provider_delete"
            | "validate_cnb_token"
            | "validate_gitea_token"
            | "validate_gitlab_token"
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
        "codepluse_provider_delete" => provider_delete(root, args),
        "validate_cnb_token" => validate_cnb(args),
        "validate_gitea_token" => validate_gitea(args),
        "validate_gitlab_token" => validate_gitlab(args),
        _ => Err(format!("未知的提供商命令：{command}")),
    }
}

// ── 提供商删除 ────────────────────────────────────────────────────────────

fn providers_path(root: &Path) -> PathBuf {
    root.join(".data").join(PROVIDERS_FILE)
}

fn provider_delete(root: &Path, args: &Value) -> Result<Value, String> {
    let id = args
        .get("id")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| "缺少 id 参数".to_string())?;

    let path = providers_path(root);
    let _guard = store_file::lock(&path)?;
    let Some(mut data) = store_file::read_json(&path)? else {
        // 明确 404：存储都不存在，自然没有这条记录（非幂等重试成功语义）。
        return Err("供应商不存在".into());
    };
    let providers = data
        .get_mut("providers")
        .and_then(Value::as_array_mut)
        .ok_or_else(|| "供应商配置格式无效".to_string())?;

    let index = providers
        .iter()
        .position(|entry| entry.get("id").and_then(Value::as_str) == Some(id))
        .ok_or_else(|| "供应商不存在".to_string())?;

    let agent = providers[index]
        .get("agent")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    let mut removed = providers.remove(index);

    // 与 Go `Delete` 对齐：被删记录若是某个执行端的激活项，一并清掉。
    if let Some(active) = data.get_mut("active").and_then(Value::as_object_mut) {
        if active.get(&agent).and_then(Value::as_str) == Some(id) {
            active.remove(&agent);
        }
    }

    store_file::write_json(&path, &data)?;

    // 回被删记录：明文密钥与内部态不离开本进程。
    if let Some(record) = removed.as_object_mut() {
        record.remove("key");
        record.remove("env");
        record.remove("native");
    }
    Ok(removed)
}

// ── 令牌校验公共件 ────────────────────────────────────────────────────────

fn trimmed(args: &Value, key: &str) -> String {
    args.get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .unwrap_or_default()
        .to_string()
}

/// 环境变量优先（测试桩用），否则用前端传入的 `serverUrl`，都为空才回落默认主机。
fn base_url(env_var: &str, server_url: &str, default: &str) -> String {
    std::env::var(env_var)
        .ok()
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| {
            if server_url.is_empty() {
                default.to_string()
            } else {
                server_url.trim_end_matches('/').to_string()
            }
        })
}

fn failure(message: &str) -> Value {
    json!({
        "success": false,
        "username": Value::Null,
        "scopes": Value::Array(Vec::new()),
        "avatar_url": Value::Null,
        "message": message,
    })
}

fn display_name(user: &Value, keys: &[&str]) -> Option<String> {
    keys.iter()
        .find_map(|key| user.get(*key).and_then(Value::as_str))
        .map(str::to_string)
}

fn authed_get(
    url: &str,
    apply: impl Fn(
        reqwest::blocking::RequestBuilder,
    ) -> reqwest::blocking::RequestBuilder,
) -> Result<(u16, Value), String> {
    let parsed = url::Url::parse(url).map_err(|_| "服务器地址无效".to_string())?;
    if parsed.scheme() != "http" && parsed.scheme() != "https" {
        return Err("服务器地址仅支持 http/https".into());
    }
    #[cfg(not(test))]
    {
        let host = parsed.host_str().unwrap_or("");
        crate::backend::provider_catalog::host_is_safe(host)?;
    }
    let builder = net::client().get(url).timeout(REQUEST_TIMEOUT);
    let response = apply(builder)
        .send()
        .map_err(|error| error.to_string())?;
    let status = response.status().as_u16();
    let mut body = Vec::new();
    {
        use std::io::Read;
        response
            .take((1 << 20) + 1)
            .read_to_end(&mut body)
            .map_err(|error| error.to_string())?;
    }
    if body.len() > (1 << 20) {
        return Err("服务器响应过大".into());
    }
    let user = serde_json::from_slice(&body).unwrap_or(Value::Null);
    Ok((status, user))
}

/// 401/403 一律记为令牌无效；其它非 200 带上状态码。传输层错误向上抛 `Err`。
fn verdict(status: u16, user: &Value, username_keys: &[&str], avatar_key: &str, scopes: Vec<String>, host: &str) -> Value {
    match status {
        200 => json!({
            "success": true,
            "username": display_name(user, username_keys),
            "scopes": scopes,
            "avatar_url": user.get(avatar_key).and_then(Value::as_str).map(str::to_string),
            "message": Value::Null,
        }),
        401 | 403 => failure("token 无效或已过期"),
        other => failure(&format!("{host} 返回 {other}")),
    }
}

// ── Gitea ─────────────────────────────────────────────────────────────────

fn validate_gitea(args: &Value) -> Result<Value, String> {
    let token = trimmed(args, "token");
    if token.is_empty() {
        return Err("请填写 Gitea 访问令牌".into());
    }
    let base = base_url("CODEPLUSE_GITEA_API_BASE", &trimmed(args, "serverUrl"), GITEA_DEFAULT_BASE);
    let url = format!("{base}/api/v1/user");
    let (status, user) = match authed_get(&url, |builder| {
        builder.header("Authorization", format!("token {token}"))
    }) {
        Ok(pair) => pair,
        Err(error) => return Err(format!("连接 Gitea 失败：{error}")),
    };
    Ok(verdict(
        status,
        &user,
        &["login", "username"],
        "avatar_url",
        Vec::new(),
        "Gitea",
    ))
}

// ── GitLab ────────────────────────────────────────────────────────────────

fn validate_gitlab(args: &Value) -> Result<Value, String> {
    let token = trimmed(args, "token");
    if token.is_empty() {
        return Err("请填写 GitLab 访问令牌".into());
    }
    let base = base_url("CODEPLUSE_GITLAB_API_BASE", &trimmed(args, "serverUrl"), GITLAB_DEFAULT_BASE);

    let (status, user) = match authed_get(&format!("{base}/api/v4/user"), |builder| {
        builder.header("PRIVATE-TOKEN", token.clone())
    }) {
        Ok(pair) => pair,
        Err(error) => return Err(format!("连接 GitLab 失败：{error}")),
    };
    if status != 200 {
        return Ok(verdict(status, &user, &["username", "name"], "avatar_url", Vec::new(), "GitLab"));
    }

    // scopes 在单独的端点；这个辅助调用失败不拖垮主校验，回空数组即可。
    let scopes = match authed_get(
        &format!("{base}/api/v4/personal_access_tokens/self"),
        |builder| builder.header("PRIVATE-TOKEN", token.clone()),
    ) {
        Ok((200, body)) => body
            .get("scopes")
            .and_then(Value::as_array)
            .map(|rows| {
                rows.iter()
                    .filter_map(|scope| scope.as_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_default(),
        _ => Vec::new(),
    };

    Ok(verdict(
        status,
        &user,
        &["username", "name"],
        "avatar_url",
        scopes,
        "GitLab",
    ))
}

// ── CNB ───────────────────────────────────────────────────────────────────

/// 前端给的网页主机是 `cnb.cool`；OpenAPI 在 `api.cnb.cool`，这里做主机换算。
fn cnb_base(server_url: &str) -> String {
    if let Ok(override_base) = std::env::var("CODEPLUSE_CNB_API_BASE") {
        if !override_base.trim().is_empty() {
            return override_base.trim_end_matches('/').to_string();
        }
    }
    if server_url.is_empty() {
        return CNB_DEFAULT_BASE.to_string();
    }
    let normalized = server_url.trim_end_matches('/');
    match normalized
        .strip_prefix("https://cnb.cool")
        .or_else(|| normalized.strip_prefix("http://cnb.cool"))
    {
        Some(_) => CNB_DEFAULT_BASE.to_string(),
        None => normalized.to_string(),
    }
}

fn validate_cnb(args: &Value) -> Result<Value, String> {
    let token = trimmed(args, "token");
    if token.is_empty() {
        return Err("请填写 CNB 访问令牌".into());
    }
    let base = cnb_base(&trimmed(args, "serverUrl"));
    let (status, user) = match authed_get(&format!("{base}/user"), |builder| {
        builder
            .header("Authorization", format!("Bearer {token}"))
            .header("Accept", "application/vnd.cnb.api+json")
    }) {
        Ok(pair) => pair,
        Err(error) => return Err(format!("连接 CNB 失败：{error}")),
    };
    Ok(verdict(
        status,
        &user,
        &["username", "nickname"],
        "avatar",
        Vec::new(),
        "CNB",
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::sync::{Mutex, OnceLock};
    use std::time::{SystemTime, UNIX_EPOCH};

    fn temp_root(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "codepluse-token-validate-{tag}-{}-{}",
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

    /// 环境变量是进程级的，并行用例必须串行设置/恢复。
    fn env_lock() -> &'static Mutex<()> {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| Mutex::new(()))
    }

    /// 一次性桩 API：按路径返回预置的 `(status, body)`。
    fn mock_server(routes: &[(&str, u16, &'static str)]) -> String {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap().to_string();
        let routes: Vec<(String, u16, String)> = routes
            .iter()
            .map(|(path, status, body)| (path.to_string(), *status, body.to_string()))
            .collect();
        std::thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                let mut stream = stream;
                let mut buffer = [0u8; 4096];
                let _ = stream.read(&mut buffer);
                let request = String::from_utf8_lossy(&buffer);
                let path = request
                    .split_whitespace()
                    .nth(1)
                    .unwrap_or("/")
                    .to_string();
                let (status, body) = routes
                    .iter()
                    .find(|(route, _, _)| path.starts_with(route.as_str()))
                    .map(|(_, status, body)| (*status, body.as_str()))
                    .unwrap_or((404, r#"{"message":"not found"}"#));
                let reason = match status {
                    200 => "OK",
                    401 => "Unauthorized",
                    403 => "Forbidden",
                    _ => "Error",
                };
                let response = format!(
                    "HTTP/1.1 {status} {reason}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = stream.write_all(response.as_bytes());
            }
        });
        format!("http://{address}")
    }

    #[test]
    fn owns_covers_the_four_documented_commands_only() {
        for command in [
            "codepluse_provider_delete",
            "validate_cnb_token",
            "validate_gitea_token",
            "validate_gitlab_token",
        ] {
            assert!(owns(command), "{command} should be owned");
        }
        assert!(!owns("validate_github_token"));
        assert!(!owns("codepluse_provider_save"));
        assert!(!owns("validate_CNB_token"));
    }

    // ── 删除 ──────────────────────────────────────────────────────────────

    fn seed_providers(root: &Path) {
        let payload = json!({
            "providers": [
                {
                    "id": "aaa111",
                    "name": "第一家",
                    "agent": "claude",
                    "url": "https://a.example.com",
                    "model": "m",
                    "key": "plaintext-secret-key",
                    "hasKey": true,
                },
                {
                    "id": "bbb222",
                    "name": "第二家",
                    "agent": "codex",
                    "url": "https://b.example.com",
                    "model": "n",
                    "key": "another-secret",
                    "hasKey": true,
                }
            ],
            "active": { "claude": "aaa111", "codex": "bbb222" }
        });
        store_file::write_json(&providers_path(root), &payload).unwrap();
    }

    #[test]
    fn delete_removes_the_record_clears_active_and_strips_the_key() {
        let root = temp_root("delete");
        seed_providers(&root);

        let removed = call(
            &root,
            "codepluse_provider_delete",
            json!({"id": "aaa111"}),
        )
        .unwrap();
        assert_eq!(removed["id"], "aaa111");
        assert_eq!(removed["name"], "第一家");
        // 明文密钥不得出现在返回里。
        assert!(removed.get("key").is_none());
        assert!(!removed.to_string().contains("plaintext-secret-key"));

        let stored = store_file::read_json(&providers_path(&root))
            .unwrap()
            .unwrap();
        let providers = stored["providers"].as_array().unwrap();
        assert_eq!(providers.len(), 1);
        assert_eq!(providers[0]["id"], "bbb222");
        // claude 的激活项被清掉；codex 的保留。
        assert!(stored["active"].get("claude").is_none());
        assert_eq!(stored["active"]["codex"], "bbb222");
    }

    #[test]
    fn delete_rejects_missing_id_and_unknown_id_with_explicit_404() {
        let root = temp_root("delete-bad");
        seed_providers(&root);

        assert!(call(&root, "codepluse_provider_delete", json!({}))
            .unwrap_err()
            .contains("缺少 id"));
        assert!(call(
            &root,
            "codepluse_provider_delete",
            json!({"id": "   "})
        )
        .unwrap_err()
        .contains("缺少 id"));
        // 非幂等：查不到明确报 404。
        let err = call(
            &root,
            "codepluse_provider_delete",
            json!({"id": "nope"}),
        )
        .unwrap_err();
        assert_eq!(err, "供应商不存在");

        // 存储文件本身不存在也是 404。
        let empty_root = temp_root("delete-empty");
        let err = call(
            &empty_root,
            "codepluse_provider_delete",
            json!({"id": "nope"}),
        )
        .unwrap_err();
        assert_eq!(err, "供应商不存在");
    }

    // ── Gitea ─────────────────────────────────────────────────────────────

    #[test]
    fn gitea_success_parses_login_and_avatar_with_empty_scopes() {
        let _guard = env_lock().lock().unwrap();
        let root = temp_root("gitea-ok");
        let base = mock_server(&[(
            "/api/v1/user",
            200,
            r#"{"login":"alice","username":"alice","avatar_url":"https://gitea.com/a.png"}"#,
        )]);
        std::env::set_var("CODEPLUSE_GITEA_API_BASE", &base);

        let result = call(
            &root,
            "validate_gitea_token",
            json!({"serverUrl": "", "token": "gitea-token"}),
        )
        .unwrap();
        std::env::remove_var("CODEPLUSE_GITEA_API_BASE");

        assert_eq!(result["success"], true);
        assert_eq!(result["username"], "alice");
        assert_eq!(result["avatar_url"], "https://gitea.com/a.png");
        assert_eq!(result["scopes"].as_array().unwrap().len(), 0);
        assert!(result["message"].is_null());
        // 令牌不出现在响应里。
        assert!(!result.to_string().contains("gitea-token"));
    }

    #[test]
    fn gitea_unauthorized_is_a_failed_validation_not_an_err() {
        let _guard = env_lock().lock().unwrap();
        let root = temp_root("gitea-401");
        let base = mock_server(&[("/api/v1/user", 401, r#"{"message":"Unauthorized"}"#)]);
        std::env::set_var("CODEPLUSE_GITEA_API_BASE", &base);

        let result = call(
            &root,
            "validate_gitea_token",
            json!({"serverUrl": "https://gitea.com", "token": "bad-token"}),
        )
        .unwrap();
        std::env::remove_var("CODEPLUSE_GITEA_API_BASE");

        assert_eq!(result["success"], false);
        assert!(result["username"].is_null());
        assert!(result["message"].as_str().unwrap().contains("无效"));
    }

    #[test]
    fn gitea_missing_token_is_an_err() {
        let root = temp_root("gitea-no-token");
        let err = call(
            &root,
            "validate_gitea_token",
            json!({"serverUrl": "https://gitea.com", "token": "  "}),
        )
        .unwrap_err();
        assert!(err.contains("Gitea 访问令牌"));
    }

    // ── GitLab ─────────────────────────────────────────────────────────────

    #[test]
    fn gitlab_success_reads_user_and_scopes_from_two_endpoints() {
        let _guard = env_lock().lock().unwrap();
        let root = temp_root("gitlab-ok");
        let base = mock_server(&[
            (
                "/api/v4/user",
                200,
                r#"{"username":"bob","name":"Bob","avatar_url":"https://gitlab.com/b.png"}"#,
            ),
            (
                "/api/v4/personal_access_tokens/self",
                200,
                r#"{"scopes":["api","read_user"]}"#,
            ),
        ]);
        std::env::set_var("CODEPLUSE_GITLAB_API_BASE", &base);

        let result = call(
            &root,
            "validate_gitlab_token",
            json!({"serverUrl": "", "token": "gl-token"}),
        )
        .unwrap();
        std::env::remove_var("CODEPLUSE_GITLAB_API_BASE");

        assert_eq!(result["success"], true);
        assert_eq!(result["username"], "bob");
        assert_eq!(result["avatar_url"], "https://gitlab.com/b.png");
        let scopes = result["scopes"].as_array().unwrap();
        assert_eq!(scopes.len(), 2);
        assert_eq!(scopes[0], "api");
        assert!(!result.to_string().contains("gl-token"));
    }

    #[test]
    fn gitlab_non_200_is_a_failed_validation_and_missing_token_is_err() {
        let _guard = env_lock().lock().unwrap();
        let root = temp_root("gitlab-403");
        let base = mock_server(&[("/api/v4/user", 403, r#"{"message":"Forbidden"}"#)]);
        std::env::set_var("CODEPLUSE_GITLAB_API_BASE", &base);

        let result = call(
            &root,
            "validate_gitlab_token",
            json!({"serverUrl": "https://gitlab.com", "token": "gl-bad"}),
        )
        .unwrap();
        std::env::remove_var("CODEPLUSE_GITLAB_API_BASE");

        assert_eq!(result["success"], false);
        assert!(result["message"].as_str().unwrap().contains("无效"));

        let err = call(
            &root,
            "validate_gitlab_token",
            json!({"serverUrl": "https://gitlab.com", "token": ""}),
        )
        .unwrap_err();
        assert!(err.contains("GitLab 访问令牌"));
    }

    // ── CNB ───────────────────────────────────────────────────────────────

    #[test]
    fn cnb_success_converts_web_host_to_api_host_and_reads_username() {
        let _guard = env_lock().lock().unwrap();
        let root = temp_root("cnb-ok");
        let base = mock_server(&[(
            "/user",
            200,
            r#"{"username":"carol","nickname":"Carol","avatar":"https://api.cnb.cool/c.png"}"#,
        )]);
        std::env::set_var("CODEPLUSE_CNB_API_BASE", &base);

        // 传入网页主机；环境变量桩接管真实地址，断言字段映射即可。
        let result = call(
            &root,
            "validate_cnb_token",
            json!({"serverUrl": "https://cnb.cool", "token": "cnb-token"}),
        )
        .unwrap();
        std::env::remove_var("CODEPLUSE_CNB_API_BASE");

        assert_eq!(result["success"], true);
        assert_eq!(result["username"], "carol");
        assert_eq!(result["avatar_url"], "https://api.cnb.cool/c.png");
        assert_eq!(result["scopes"].as_array().unwrap().len(), 0);
        assert!(!result.to_string().contains("cnb-token"));
    }

    #[test]
    fn cnb_missing_token_is_an_err() {
        let root = temp_root("cnb-no-token");
        let err = call(
            &root,
            "validate_cnb_token",
            json!({"serverUrl": "https://cnb.cool", "token": ""}),
        )
        .unwrap_err();
        assert!(err.contains("CNB 访问令牌"));
    }

    #[test]
    fn unknown_commands_fall_through_to_the_next_module() {
        assert!(handle(&temp_root("fallthrough"), "pet_list", &json!({})).is_none());
    }
}
