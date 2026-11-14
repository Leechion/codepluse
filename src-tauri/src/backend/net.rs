//! Shared HTTP plumbing for ported commands that talk to the internet.
//!
//! Go's handlers build plain `http.Client`s on the default transport, which
//! honours the system proxy environment. This module mirrors that with a
//! blocking reqwest client (TLS via the OS store) and the small request
//! shapes those handlers need. Callers run on the blocking pool — a blocking
//! client must never be built or used on an async worker.

use std::time::Duration;

use reqwest::blocking::{Client, Response};
use std::sync::{OnceLock, RwLock};

static CLIENT: OnceLock<RwLock<Client>> = OnceLock::new();
static NO_PROXY_CLIENT: OnceLock<Client> = OnceLock::new();

/// The shared client: system proxy (like Go's `http.DefaultTransport`), no
/// global timeout — Go's default transport has none either. Per-call timeouts
/// mirror the specific `http.Client{Timeout: …}` the Go code builds. The
/// User-Agent matches Go's automatic header: GitHub rejects UA-less requests
/// with 403 where the sidecar gets 401, which would drift every error path.
pub fn client() -> Client {
    CLIENT
        .get_or_init(|| RwLock::new(build_client(None, None).expect("HTTP 客户端初始化失败")))
        .read()
        .expect("HTTP 客户端锁失败")
        .clone()
}

/// Apply the app-wide proxy configured in desktop preferences. Rebuilding the
/// client also makes a settings change take effect without restarting.
pub fn configure_proxy(proxy: Option<&str>, no_proxy: Option<&str>) -> Result<(), String> {
    let updated = build_client(proxy, no_proxy)?;
    let lock = CLIENT.get_or_init(|| RwLock::new(updated.clone()));
    *lock.write().map_err(|_| "HTTP 客户端锁失败")? = updated;
    Ok(())
}

fn build_client(proxy: Option<&str>, no_proxy: Option<&str>) -> Result<Client, String> {
    let mut builder = Client::builder().user_agent("Go-http-client/1.1");
    if let Some(proxy) = proxy {
        let bypass = no_proxy
            .filter(|value| !value.trim().is_empty())
            .and_then(reqwest::NoProxy::from_string);
        let proxy = reqwest::Proxy::all(proxy)
            .map_err(|error| error.to_string())?
            .no_proxy(bypass);
        builder = builder.proxy(proxy);
    }
    builder.build().map_err(|error| error.to_string())
}

/// 代理禁用客户端（Go：transport.Proxy = nil）—— loopback 目标与 SSRF 防护
/// 路径使用。
pub fn no_proxy_client() -> &'static Client {
    NO_PROXY_CLIENT.get_or_init(|| {
        Client::builder()
            .user_agent("Go-http-client/1.1")
            .no_proxy()
            .build()
            .expect("HTTP 客户端初始化失败")
    })
}

/// 指定代理的按次构建客户端（providermodels 的注册表代理优先语义）。
pub fn proxied_client(proxy: &str) -> Result<Client, String> {
    let proxy = reqwest::Proxy::all(proxy).map_err(|e| e.to_string())?;
    Client::builder()
        .user_agent("Go-http-client/1.1")
        .proxy(proxy)
        .build()
        .map_err(|e| e.to_string())
}

/// GET with the per-call timeout the Go handler uses.
pub fn get(url: &str, timeout: Duration) -> Result<Response, String> {
    client()
        .get(url)
        .timeout(timeout)
        .send()
        .map_err(|e| e.to_string())
}

/// GET with GitHub REST v3 headers (`forge_github.go`).
pub fn github_get(url: &str, token: &str, timeout: Duration) -> Result<Response, String> {
    client()
        .get(url)
        .timeout(timeout)
        .header("Authorization", format!("Bearer {token}"))
        .header("Accept", "application/vnd.github+json")
        .send()
        .map_err(|e| format!("无法连接 GitHub：{e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn client_can_build_requests() {
        let a = client();
        let b = client();
        assert!(a.get("https://example.com").build().is_ok());
        assert!(b.get("https://example.com").build().is_ok());
        // 时间量级仅作形状检查：Go 每处单独设 15s/30s。
        let _ = Duration::from_secs(15);
    }

    #[test]
    fn explicit_proxy_reconfiguration_is_validated_and_reusable() {
        assert!(configure_proxy(Some("not a proxy"), None).is_err());
        configure_proxy(Some("http://127.0.0.1:8765"), Some("localhost,127.0.0.1")).unwrap();
        assert!(client().get("https://example.com").build().is_ok());
        configure_proxy(None, None).unwrap();
    }
}
