//! Forge: the Issue/PR workbench, ported from `server-go/internal/codeg/forge.go`
//! and `forge_github.go`.
//!
//! Ported here (10): `validate_github_token`, `folder_forge_remote`,
//! `get_github_accounts`, `update_github_accounts`, `forge_identity`,
//! `forge_settings_get`, `forge_settings_set`, `forge_auto_status`,
//! `forge_auto_scan_now` — plus `github_request`, the REST core the twelve
//! GitHub workbench commands (`forge_list_issues` … `forge_merge_change`)
//! will reuse in the next slice.
//!
//! Two contracts worth spelling out:
//!
//! - **Account tokens live in process memory only** (Go: "a restart asks for
//!   re-entry"). `update_github_accounts` validates each token against
//!   `GET /user` and keeps the list in RAM; the settings file
//!   `.data/codeg-forge.json` persists without tokens. The two processes must
//!   not share this state, so the whole forge surface flips as one unit —
//!   while any forge command is still answered by Go, its in-memory accounts
//!   are the only ones that exist, which is exactly why these commands move
//!   together.
//! - **`validate_github_token` is a dead path in Go**: `forgeCommand` has no
//!   case for it, so it falls through to repository resolution and errors
//!   (项目文件夹不存在 without a folder, 未知的 Forge 命令 once the remote
//!   parses). The port reproduces that fall-through byte for byte — tests
//!   pin it, because "fixing" it here would diverge from the sidecar.
//!
//! All handlers are blocking (HTTPS via `net`, git via the CLI) and run on
//! the blocking pool.

use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

use serde_json::{json, Map, Value};

use crate::backend::net;
use crate::backend::store_file;

const GITHUB_API_DEFAULT: &str = "https://api.github.com";

pub fn owns(command: &str) -> bool {
    matches!(
        command,
        "validate_github_token"
            | "folder_forge_remote"
            | "get_github_accounts"
            | "update_github_accounts"
            | "forge_identity"
            | "forge_settings_get"
            | "forge_settings_set"
            | "forge_auto_status"
            | "forge_auto_scan_now"
            // GitHub workbench (forge_github.go) — the REST slice.
            | "forge_list_issues"
            | "forge_tab_count"
            | "forge_issue_detail"
            | "forge_list_labels"
            | "forge_list_comments"
            | "forge_create_comment"
            | "forge_set_item_state"
            | "forge_create_issue"
            | "forge_change_detail"
            | "forge_change_files"
            | "forge_merge_options"
            | "forge_merge_change"
    )
}

pub fn handle(root: &Path, command: &str, args: &Value) -> Option<Result<Value, String>> {
    if !owns(command) {
        return None;
    }
    Some(run_with_api(&github_api(), root, command, args))
}

/// forge_github.go keeps `githubAPI` as a var so tests can point the client
/// at a mock server; the environment hook plays that role here.
fn github_api() -> String {
    std::env::var("CODEPLUSE_GITHUB_API")
        .ok()
        .filter(|v| !v.trim().is_empty())
        .unwrap_or_else(|| GITHUB_API_DEFAULT.to_string())
}

// ── state ───────────────────────────────────────────────────────────────────

#[derive(Clone)]
struct Account {
    id: i64,
    name: String,
    login: String,
    token: String,
}

struct ForgeState {
    root: PathBuf,
    next_id: i64,
    accounts: Vec<Account>,
    settings: Map<String, Value>,
    global: Value,
}

static STATE: OnceLock<Mutex<ForgeState>> = OnceLock::new();

fn state(root: &Path) -> &'static Mutex<ForgeState> {
    STATE.get_or_init(|| Mutex::new(init_state(root)))
}

fn init_state(root: &Path) -> ForgeState {
    let mut state = ForgeState {
        root: root.to_path_buf(),
        next_id: 1,
        accounts: Vec::new(),
        settings: Map::new(),
        global: json!({}),
    };
    // Go keeps the saved settings only when both decode cleanly; the file
    // never carries tokens, so nothing to wipe.
    if let Ok(Some(value)) = store_file::read_json(&settings_path(root)) {
        if let Some(settings) = value.get("settings").and_then(|v| v.as_object()) {
            state.settings = settings.clone();
        }
        if let Some(global) = value.get("global").filter(|v| v.is_object()) {
            state.global = global.clone();
        }
    }
    state
}

#[cfg(test)]
fn fresh_state(root: &Path) {
    *state(root).lock().unwrap() = init_state(root);
}

fn settings_path(root: &Path) -> PathBuf {
    root.join(".data").join("codeg-forge.json")
}

/// forge.go `accountFor`: the explicit account when given, else the first one.
fn account_for(state: &ForgeState, account_id: &str) -> Result<Account, String> {
    if !account_id.is_empty() {
        for account in &state.accounts {
            if account.id.to_string() == account_id {
                return Ok(account.clone());
            }
        }
        return Err("找不到指定的 GitHub 账户".into());
    }
    state
        .accounts
        .first()
        .cloned()
        .ok_or_else(|| "尚未配置 GitHub 账户，请先在设置里添加 token".to_string())
}

// ── command entry ───────────────────────────────────────────────────────────

fn run_with_api(api: &str, root: &Path, command: &str, args: &Value) -> Result<Value, String> {
    let mut guard = state(root).lock().unwrap();
    match command {
        "get_github_accounts" => {
            let rows: Vec<Value> = guard
                .accounts
                .iter()
                .map(|account| {
                    json!({
                        "id": account.id.to_string(),
                        "kind": "github",
                        "name": account.name,
                        "server_url": "",
                        "login": account.login,
                    })
                })
                .collect();
            Ok(Value::Array(rows))
        }
        "update_github_accounts" => {
            let result = update_accounts(guard, api, args.get("settings"));
            result
        }
        "forge_identity" => {
            let user = github_request(&guard, api, &txt(args, "accountId"), "GET", "/user", None)?;
            Ok(json!({
                "username": text(user.get("login")),
                "avatar_url": text(user.get("avatar_url")),
            }))
        }
        "folder_forge_remote" => {
            let (host, repo) = folder_remote(&guard, num(args, "folderId"))?;
            Ok(json!({"provider": "github", "server_host": host, "owner_repo": repo}))
        }
        "validate_github_token" => {
            // Go has no case for this command: it falls through to repository
            // resolution and dies there. Reproduced verbatim — see the module
            // comment before "fixing".
            let (host, _) = folder_remote(&guard, num(args, "folderId"))?;
            if host != "github.com" && !host.ends_with(".github.com") {
                return Err(format!("Forge 工作台当前仅支持 GitHub（该远程指向 {host}）"));
            }
            Err("未知的 Forge 命令：validate_github_token".into())
        }
        "forge_settings_get" => {
            let key = num(args, "folderId").to_string();
            Ok(json!({
                "global": guard.global.clone(),
                "folder": guard.settings.get(&key).cloned().unwrap_or(Value::Null),
            }))
        }
        "forge_settings_set" => {
            let folder_id = num(args, "folderId");
            let settings = args.get("settings").filter(|v| v.is_object()).cloned();
            if folder_id == 0 {
                if settings.is_none() {
                    return Err("全局设置不能删除".into());
                }
                guard.global = settings.unwrap_or(Value::Null);
            } else {
                let key = folder_id.to_string();
                match settings {
                    None => {
                        guard.settings.remove(&key);
                    }
                    Some(value) => {
                        guard.settings.insert(key, value);
                    }
                }
            }
            save_settings(&guard);
            Ok(settings_snapshot(&guard))
        }
        "forge_auto_status" => Ok(json!({"enabled": false, "reason": "自动处理尚未接通"})),
        "forge_auto_scan_now" => Ok(json!({"scanned": 0, "created": 0})),

        // Everything below resolves the repository first (forgeCommand's
        // fall-through): folder remote → host gate → per-command work.
        "forge_list_issues" | "forge_tab_count" | "forge_issue_detail"
        | "forge_list_labels" | "forge_list_comments" | "forge_create_comment"
        | "forge_set_item_state" | "forge_create_issue" | "forge_change_detail"
        | "forge_change_files" | "forge_merge_options" | "forge_merge_change" => {
            let (host, repo) = folder_remote(&guard, num(args, "folderId"))?;
            if host != "github.com" && !host.ends_with(".github.com") {
                return Err(format!("Forge 工作台当前仅支持 GitHub（该远程指向 {host}）"));
            }
            forge_rest(&guard, api, command, &repo, args)
        }
        _ => Err(format!("未知的 Forge 命令：{command}")),
    }
}

fn settings_snapshot(state: &ForgeState) -> Value {
    json!({
        "global": state.global.clone(),
        "folder": Value::Object(state.settings.clone()),
    })
}

/// forge.go `saveSettings`: fire-and-forget atomic write of the token-free
/// document (compact, sorted map keys — `canon` renders Go's marshal shape).
fn save_settings(state: &ForgeState) {
    let document = json!({
        "global": state.global.clone(),
        "settings": Value::Object(state.settings.clone()),
    });
    let bytes = crate::backend::goformat::marshal_preserve(&crate::backend::goformat::canon(
        &document,
    ));
    let _ = store_file::write_bytes(&settings_path(&state.root), &bytes);
}

// ── accounts (forge.go forgeUpdateAccounts) ─────────────────────────────────

fn update_accounts(
    mut guard: std::sync::MutexGuard<'_, ForgeState>,
    api: &str,
    settings: Option<&Value>,
) -> Result<Value, String> {
    let entries: Vec<Value> = match settings.and_then(|s| s.get("accounts")) {
        Some(Value::Array(list)) => list.clone(),
        // Go's type assertion yields nil for a missing/non-object settings,
        // and a nil entries list replaces the accounts with an empty set.
        _ => Vec::new(),
    };
    let mut next: Vec<Account> = Vec::new();
    for entry in &entries {
        let Some(item) = entry.as_object() else {
            continue;
        };
        let item = Value::Object(item.clone());
        let name = text(item.get("name")).trim().to_string();
        let token = text(item.get("token"));
        if token.is_empty() {
            // Keep an existing account when its token is not re-entered.
            let id = text(item.get("id"));
            if !id.is_empty() {
                if let Ok(old) = account_for(&guard, &id) {
                    next.push(old);
                    continue;
                }
            }
            return Err(format!("账户 {name} 缺少 token"));
        }
        let checked = validate_token(api, &token)?;
        if checked.get("valid") != Some(&Value::Bool(true)) {
            let message = text(checked.get("message"));
            return Err(format!("token 校验失败：{message}"));
        }
        let login = text(checked.get("login"));
        let raw_id = text(item.get("id"));
        let id = if raw_id.is_empty() {
            let id = guard.next_id;
            guard.next_id += 1;
            id
        } else {
            // Go: fmt.Sscanf("%d") with the error ignored — leading digits
            // parse, anything else stays 0.
            parse_leading_int(&raw_id)
        };
        next.push(Account { id, name, login, token });
    }
    guard.accounts = next;
    Ok(Value::Null)
}

/// forge_github.go `forgeValidateToken`: check a PAT against GET /user.
fn validate_token(api: &str, token: &str) -> Result<Value, String> {
    if token.trim().is_empty() {
        return Err("请填写 GitHub token".into());
    }
    let response = net::github_get(&format!("{api}/user"), token, Duration::from_secs(15))?;
    let status = response.status().as_u16();
    let body = response.bytes().map_err(|e| e.to_string())?;
    if status == 401 {
        return Ok(json!({"valid": false, "message": "token 无效或已过期"}));
    }
    if status != 200 {
        return Ok(json!({"valid": false, "message": format!("GitHub 返回 {status}")}));
    }
    // Go ignores decode errors: a malformed body just yields empty fields.
    let user: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
    Ok(json!({
        "valid": true,
        "login": text(user.get("login")),
        "name": text(user.get("name")),
        "avatar_url": text(user.get("avatar_url")),
    }))
}

// ── GitHub REST core (forge_github.go githubRequest) ────────────────────────

/// One authenticated REST v3 request; returns the decoded JSON body. Statuses
/// >= 300 become the Go-shaped error (message trimmed, truncated to 300).
fn github_request(
    guard: &ForgeState,
    api: &str,
    account_id: &str,
    method: &str,
    path: &str,
    body: Option<&Value>,
) -> Result<Value, String> {
    let account = account_for(guard, account_id)?;
    let request = net::client()
        .request(
            reqwest::Method::from_bytes(method.as_bytes())
                .map_err(|e| format!("无法连接 GitHub：{e}"))?,
            format!("{api}{path}"),
        )
        .timeout(Duration::from_secs(30))
        .header("Authorization", format!("Bearer {}", account.token))
        .header("Accept", "application/vnd.github+json")
        .header("X-GitHub-Api-Version", "2022-11-28");
    let request = match body {
        Some(value) => request
            .header("Content-Type", "application/json")
            // Go marshals the Row body: sorted keys, HTML escapes — keep the
            // request bytes identical so mock servers see one shape.
            .body(crate::backend::goformat::marshal_preserve(
                &crate::backend::goformat::canon(value),
            )),
        None => request,
    };
    let response = request.send().map_err(|e| format!("无法连接 GitHub：{e}"))?;
    let status = response.status().as_u16();
    let data = response.bytes().map_err(|e| e.to_string())?;
    if status >= 300 {
        let mut message = String::from_utf8_lossy(&data).trim().to_string();
        if message.len() > 300 {
            message.truncate(300);
        }
        return Err(format!("GitHub {method} {path} 返回 {status}：{message}"));
    }
    if data.is_empty() {
        return Ok(Value::Null);
    }
    serde_json::from_slice(&data).map_err(|e| e.to_string())
}

// ── GitHub workbench REST (forge_github.go) ─────────────────────────────────

/// forge_github.go `forgeRow`: one wire row per issue/PR payload. `body` is
/// byte-truncated at 2000 like Go's `body[:2000]` (at a char boundary — Go's
/// marshal would render the cut rune as \ufffd; see the ledger note).
fn forge_row(item: &Value) -> Value {
    let is_pr = item.get("pull_request").is_some() || item.get("head").is_some();
    let mut labels = Vec::new();
    if let Some(list) = item.get("labels").and_then(|v| v.as_array()) {
        for label in list {
            let color = text(label.get("color"));
            let color = if color.is_empty() { color } else { format!("#{color}") };
            labels.push(json!({"name": text(label.get("name")), "color": color}));
        }
    }
    let mut author = String::new();
    let mut avatar = String::new();
    if let Some(user) = item.get("user") {
        author = text(user.get("login"));
        avatar = text(user.get("avatar_url"));
    }
    let mut state = text(item.get("state"));
    if is_pr && item.get("merged_at").map(|v| !v.is_null()).unwrap_or(false) {
        state = "merged".into();
    }
    let draft = item
        .get("pull_request")
        .and_then(|pr| pr.get("draft"))
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let mut body = text(item.get("body"));
    if body.len() > 2000 {
        let mut end = 2000;
        while !body.is_char_boundary(end) {
            end -= 1;
        }
        body.truncate(end);
    }
    json!({
        "number": item.get("number").cloned().unwrap_or(Value::Null),
        "title": item.get("title").cloned().unwrap_or(Value::Null),
        "body": body,
        "state": state,
        "draft": draft,
        "labels": labels,
        "author": author,
        "author_avatar": avatar,
        "updated_at": item.get("updated_at").cloned().unwrap_or(Value::Null),
        "html_url": item.get("html_url").cloned().unwrap_or(Value::Null),
        "is_pr": is_pr,
        "comments": item.get("comments").cloned().unwrap_or(Value::Null),
    })
}

/// Go's `url.QueryEscape`: alphanumerics plus `-_.~` pass, space becomes `+`,
/// everything else is %XX over UTF-8 bytes.
fn query_escape(input: &str) -> String {
    let mut out = String::new();
    for byte in input.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(byte as char);
            }
            b' ' => out.push('+'),
            _ => out.push_str(&format!("%{byte:02X}")),
        }
    }
    out
}

fn opt_number(value: &Value, key: &str) -> i64 {
    number(value.get(key))
}

fn page_params(value: &Value, page_key: &str, per_page_key: &str) -> (i64, i64) {
    let mut page = opt_number(value, page_key);
    if page < 1 {
        page = 1;
    }
    let mut per_page = opt_number(value, per_page_key);
    if per_page < 1 || per_page > 100 {
        per_page = 30;
    }
    (page, per_page)
}

/// The twelve workbench commands; `repo` is `owner/name` off the folder's
/// origin remote.
fn forge_rest(
    guard: &ForgeState,
    api: &str,
    command: &str,
    repo: &str,
    args: &Value,
) -> Result<Value, String> {
    match command {
        "forge_list_issues" => {
            let query = args.get("query").cloned().unwrap_or(json!({}));
            let tab = or_text(&query, "tab", "issues");
            let state = or_text(&query, "state", "open");
            let (page, per_page) = page_params(&query, "page", "perPage");
            let mut terms = vec![format!("repo:{repo}"), format!("is:{tab}"), format!("state:{state}")];
            if query.get("assignedMe") == Some(&Value::Bool(true)) {
                terms.push("assignee:@me".into());
            }
            if let Some(labels) = query.get("labels").and_then(|v| v.as_array()) {
                for label in labels {
                    terms.push(format!("label:\"{}\"", text(Some(label))));
                }
            }
            let search = text(query.get("search"));
            if !search.is_empty() {
                terms.push(search);
            }
            let mut endpoint = format!(
                "/search/issues?q={}&page={page}&per_page={per_page}",
                query_escape(&terms.join(" "))
            );
            match text(query.get("sort")).as_str() {
                "updated" => endpoint.push_str("&sort=updated&order=desc"),
                "created" => endpoint.push_str("&sort=created&order=desc"),
                "comments" => endpoint.push_str("&sort=comments&order=desc"),
                _ => {}
            }
            let result = github_request(&guard, api, &text(query.get("accountId")), "GET", &endpoint, None)?;
            let rows: Vec<Value> = result
                .get("items")
                .and_then(|v| v.as_array())
                .map(|items| items.iter().filter(|item| item.is_object()).map(forge_row).collect())
                .unwrap_or_default();
            let total = opt_number(&result, "total_count");
            let mut reachable = total;
            let mut has_next = page * per_page < reachable;
            if reachable > 1000 {
                reachable = 1000;
                has_next = page * per_page < reachable;
            }
            Ok(json!({
                "rows": rows, "page": page, "per_page": per_page, "total_count": total,
                "reachable_count": reachable, "has_next": has_next, "incomplete": false,
            }))
        }
        "forge_tab_count" => {
            let query = args.get("query").cloned().unwrap_or(json!({}));
            let tab = or_text(&query, "tab", "issues");
            let state = or_text(&query, "state", "open");
            let mut terms = vec![format!("repo:{repo}"), format!("is:{tab}"), format!("state:{state}")];
            if query.get("assignedMe") == Some(&Value::Bool(true)) {
                terms.push("assignee:@me".into());
            }
            let endpoint = format!(
                "/search/issues?q={}&per_page=1",
                query_escape(&terms.join(" "))
            );
            let result = github_request(&guard, api, &text(query.get("accountId")), "GET", &endpoint, None)?;
            Ok(json!({"count": opt_number(&result, "total_count")}))
        }
        "forge_issue_detail" => {
            let query = args.get("query").cloned().unwrap_or(json!({}));
            let item = github_request(
                &guard,
                api,
                &text(query.get("accountId")),
                "GET",
                &format!("/repos/{repo}/issues/{}", opt_number(&query, "number")),
                None,
            )?;
            let mut row = forge_row(&item);
            row["body"] = item.get("body").cloned().unwrap_or(Value::Null);
            Ok(row)
        }
        "forge_list_labels" => {
            let items = github_request(
                &guard,
                api,
                &txt(args, "accountId"),
                "GET",
                &format!("/repos/{repo}/labels?per_page=100"),
                None,
            )?;
            let labels: Vec<Value> = items
                .as_array()
                .map(|list| {
                    list.iter()
                        .map(|label| {
                            let color = text(label.get("color"));
                            let color = if color.is_empty() { color } else { format!("#{color}") };
                            json!({"name": text(label.get("name")), "color": color})
                        })
                        .collect()
                })
                .unwrap_or_default();
            Ok(json!({"labels": labels}))
        }
        "forge_list_comments" => {
            let filters = args.get("filters").cloned().unwrap_or(json!({}));
            let kind = or_text(&filters, "kind", "issue");
            let _ = kind; // GitHub serves PR discussions from the same endpoint.
            let (page, per_page) = page_params(&filters, "page", "perPage");
            let items = github_request(
                &guard,
                api,
                &text(filters.get("accountId")),
                "GET",
                &format!(
                    "/repos/{repo}/issues/{}/comments?page={page}&per_page={per_page}",
                    opt_number(&filters, "number")
                ),
                None,
            )?;
            let mut comments = Vec::new();
            for item in items.as_array().map(|v| v.as_slice()).unwrap_or(&[]) {
                let mut author = String::new();
                let mut avatar = String::new();
                if let Some(user) = item.get("user") {
                    author = text(user.get("login"));
                    avatar = text(user.get("avatar_url"));
                }
                comments.push(json!({
                    "id": opt_number(item, "id").to_string(),
                    "author": author,
                    "author_avatar": avatar,
                    "body": item.get("body").cloned().unwrap_or(Value::Null),
                    "created_at": item.get("created_at").cloned().unwrap_or(Value::Null),
                    "html_url": item.get("html_url").cloned().unwrap_or(Value::Null),
                }));
            }
            let has_next = comments.len() as i64 == per_page;
            Ok(json!({"comments": comments, "page": page, "per_page": per_page, "has_next": has_next}))
        }
        "forge_create_comment" => {
            let draft = args.get("draft").cloned().unwrap_or(json!({}));
            let _ = or_text(&draft, "kind", "issue");
            let item = github_request(
                &guard,
                api,
                &text(draft.get("accountId")),
                "POST",
                &format!("/repos/{repo}/issues/{}/comments", opt_number(&draft, "number")),
                Some(&json!({"body": text(draft.get("body"))})),
            )?;
            Ok(comment_row(&item))
        }
        "forge_set_item_state" => {
            let request = args.get("request").cloned().unwrap_or(json!({}));
            let number_ = opt_number(&request, "number");
            let state = if text(request.get("action")) == "reopen" { "open" } else { "closed" };
            let endpoint = if text(request.get("kind")) == "pr" {
                format!("/repos/{repo}/pulls/{number_}")
            } else {
                format!("/repos/{repo}/issues/{number_}")
            };
            let item = github_request(
                &guard,
                api,
                &text(request.get("accountId")),
                "PATCH",
                &endpoint,
                Some(&json!({"state": state})),
            )?;
            let mut row = forge_row(&item);
            row["body"] = item.get("body").cloned().unwrap_or(Value::Null);
            Ok(row)
        }
        "forge_create_issue" => {
            let draft = args.get("draft").cloned().unwrap_or(json!({}));
            let mut body = json!({"title": text(draft.get("title"))});
            if !text(draft.get("body")).is_empty() {
                body["body"] = json!(text(draft.get("body")));
            }
            if let Some(labels) = draft.get("labels").and_then(|v| v.as_array()) {
                if !labels.is_empty() {
                    body["labels"] = json!(labels);
                }
            }
            let item = github_request(
                &guard,
                api,
                &text(draft.get("accountId")),
                "POST",
                &format!("/repos/{repo}/issues"),
                Some(&body),
            )?;
            Ok(forge_row(&item))
        }
        "forge_change_detail" => {
            let query = args.get("query").cloned().unwrap_or(json!({}));
            let account_id = text(query.get("accountId"));
            let pr = github_request(
                &guard,
                api,
                &account_id,
                "GET",
                &format!("/repos/{repo}/pulls/{}", opt_number(&query, "number")),
                None,
            )?;
            let head = pr.get("head").cloned().unwrap_or(Value::Null);
            let base = pr.get("base").cloned().unwrap_or(Value::Null);
            let head_repo = head
                .get("repo")
                .and_then(|r| r.get("full_name"))
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let mut checks = Vec::new();
            let sha = text(head.get("sha"));
            if !sha.is_empty() {
                // A failed checks call is not a failure of the detail view.
                if let Ok(combined) = github_request(
                    &guard,
                    api,
                    &account_id,
                    "GET",
                    &format!("/repos/{repo}/commits/{sha}/check-runs?per_page=100"),
                    None,
                ) {
                    if let Some(items) = combined.get("check_runs").and_then(|v| v.as_array()) {
                        for run in items {
                            let mut state = text(run.get("conclusion"));
                            if state.is_empty() {
                                state = text(run.get("status"));
                            }
                            checks.push(json!({
                                "name": text(run.get("name")),
                                "state": state,
                                "url": text(run.get("html_url")),
                            }));
                        }
                    }
                }
            }
            Ok(json!({
                "number": pr.get("number").cloned().unwrap_or(Value::Null),
                "base_ref": text(base.get("ref")),
                "head_ref": text(head.get("ref")),
                "head_repo": head_repo,
                "head_sha": head.get("sha").cloned().unwrap_or(Value::Null),
                "mergeable": pr.get("mergeable").cloned().unwrap_or(Value::Null),
                "mergeable_state": pr.get("mergeable_state").cloned().unwrap_or(Value::Null),
                "draft": pr.get("draft").cloned().unwrap_or(Value::Null),
                "checks": checks,
                "state": pr.get("state").cloned().unwrap_or(Value::Null),
                "merged": pr.get("merged").cloned().unwrap_or(Value::Null),
            }))
        }
        "forge_change_files" => {
            let query = args.get("query").cloned().unwrap_or(json!({}));
            let (page, per_page) = page_params(&query, "page", "perPage");
            let items = github_request(
                &guard,
                api,
                &text(query.get("accountId")),
                "GET",
                &format!(
                    "/repos/{repo}/pulls/{}/files?page={page}&per_page={per_page}",
                    opt_number(&query, "number")
                ),
                None,
            )?;
            let files: Vec<Value> = items
                .as_array()
                .map(|list| {
                    list.iter()
                        .map(|item| {
                            json!({
                                "file": item.get("filename").cloned().unwrap_or(Value::Null),
                                "additions": item.get("additions").cloned().unwrap_or(Value::Null),
                                "deletions": item.get("deletions").cloned().unwrap_or(Value::Null),
                                "changes": item.get("changes").cloned().unwrap_or(Value::Null),
                                "status": item.get("status").cloned().unwrap_or(Value::Null),
                            })
                        })
                        .collect()
                })
                .unwrap_or_default();
            let has_next = files.len() as i64 == per_page;
            Ok(json!({"files": files, "page": page, "per_page": per_page, "has_next": has_next}))
        }
        "forge_merge_options" => {
            let repo_row = github_request(
                &guard,
                api,
                &txt(args, "accountId"),
                "GET",
                &format!("/repos/{repo}"),
                None,
            )?;
            let mut methods = Vec::new();
            for (flag, method) in [
                ("allow_merge_commit", "merge"),
                ("allow_squash_merge", "squash"),
                ("allow_rebase_merge", "rebase"),
            ] {
                if repo_row.get(flag) == Some(&Value::Bool(true)) {
                    methods.push(method);
                }
            }
            let default_method = methods
                .first()
                .map(|m| m.to_string())
                .unwrap_or_else(|| "merge".into());
            Ok(json!({
                "methods": methods,
                "default_method": default_method,
                "merge_strategy": "commit",
            }))
        }
        "forge_merge_change" => {
            let request = args.get("request").cloned().unwrap_or(json!({}));
            let number_ = opt_number(&request, "number");
            let mut body = json!({"merge_method": text(request.get("method"))});
            let sha = text(request.get("headSha"));
            if !sha.is_empty() {
                body["sha"] = json!(sha);
            }
            let result = github_request(
                &guard,
                api,
                &text(request.get("accountId")),
                "PUT",
                &format!("/repos/{repo}/pulls/{number_}/merge"),
                Some(&body),
            )?;
            if result.get("merged") != Some(&Value::Bool(true)) {
                return Ok(Value::Null);
            }
            // Read the row back; a failure here is not a failure of the merge.
            match github_request(
                &guard,
                api,
                &text(request.get("accountId")),
                "GET",
                &format!("/repos/{repo}/pulls/{number_}"),
                None,
            ) {
                Ok(item) => Ok(forge_row(&item)),
                Err(_) => Ok(Value::Null),
            }
        }
        _ => Err(format!("未知的 Forge 命令：{command}")),
    }
}

/// forge_list_comments / forge_create_comment's shared row shape.
fn comment_row(item: &Value) -> Value {
    let mut author = String::new();
    let mut avatar = String::new();
    if let Some(user) = item.get("user") {
        author = text(user.get("login"));
        avatar = text(user.get("avatar_url"));
    }
    json!({
        "id": opt_number(item, "id").to_string(),
        "author": author,
        "author_avatar": avatar,
        "body": item.get("body").cloned().unwrap_or(Value::Null),
        "created_at": item.get("created_at").cloned().unwrap_or(Value::Null),
        "html_url": item.get("html_url").cloned().unwrap_or(Value::Null),
    })
}

fn or_text(value: &Value, key: &str, default: &str) -> String {
    let out = text(value.get(key));
    if out.is_empty() {
        default.to_string()
    } else {
        out
    }
}

// ── remote parsing (forge.go) ───────────────────────────────────────────────

/// forge.go `folderRemote`: the folder's origin remote as forge coordinates.
fn folder_remote(guard: &ForgeState, folder_id: i64) -> Result<(String, String), String> {
    let path = workspace_folder_path(&guard.root, folder_id)?;
    if path.is_empty() {
        return Err("项目文件夹不存在".into());
    }
    let output = git_remote_url(&path).map_err(|e| format!("读取 origin 远程失败：{e}"))?;
    parse_github_remote(output.trim())
}

/// The workspace document's folder row path (worktask_run.go `folderRow`).
fn workspace_folder_path(root: &Path, folder_id: i64) -> Result<String, String> {
    let doc = store_file::read_json(&root.join(".data").join("codeg-workspace.json"))?;
    Ok(doc
        .as_ref()
        .and_then(|doc| doc.get("folders"))
        .and_then(|v| v.as_array())
        .and_then(|rows| {
            rows.iter()
                .find(|row| row.get("id").and_then(|v| v.as_i64()) == Some(folder_id))
        })
        .and_then(|row| row.get("path"))
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string())
}

/// worktask_run.go `execGit`: stdout only; Go's ExitError prints as
/// "exit status N", which the caller wraps into its message.
fn git_remote_url(cwd: &str) -> Result<String, String> {
    if cwd.is_empty() {
        return Err("no cwd".into());
    }
    let output = std::process::Command::new("git")
        .args(["remote", "get-url", "origin"])
        .current_dir(cwd)
        .output()
        .map_err(|e| e.to_string())?;
    if !output.status.success() {
        return Err(format!("exit status {}", output.status.code().unwrap_or(-1)));
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

/// forge.go `parseGitHubRemote`: git@host:path or https(s)://host/owner/repo.
fn parse_github_remote(url: &str) -> Result<(String, String), String> {
    let mut u = url.trim().to_string();
    if u.ends_with(".git") {
        u.truncate(u.len() - 4);
    }
    if let Some(rest) = u.strip_prefix("git@") {
        if let Some(at) = rest.find(':') {
            if at > 0 {
                return Ok((rest[..at].to_string(), rest[at + 1..].to_string()));
            }
        }
    }
    for prefix in ["https://", "http://"] {
        if let Some(rest) = u.strip_prefix(prefix) {
            if let Some(slash) = rest.find('/') {
                if slash > 0 {
                    let host = rest[..slash].to_string();
                    let repo = rest[slash + 1..].trim_matches('/').to_string();
                    if repo.matches('/').count() >= 1 {
                        return Ok((host, repo));
                    }
                }
            }
        }
    }
    Err(format!("无法从远程地址解析仓库：{url}"))
}

fn parse_leading_int(raw: &str) -> i64 {
    let trimmed = raw.trim_start();
    let (sign, digits) = match trimmed.strip_prefix('-') {
        Some(rest) => (-1i64, rest),
        None => (1i64, trimmed),
    };
    let end = digits
        .find(|c: char| !c.is_ascii_digit())
        .unwrap_or(digits.len());
    format!("{}{}", if sign < 0 { "-" } else { "" }, &digits[..end])
        .parse::<i64>()
        .unwrap_or(0)
}

// ── small helpers ───────────────────────────────────────────────────────────

fn text(value: Option<&Value>) -> String {
    value.and_then(|v| v.as_str()).unwrap_or("").to_string()
}

/// Go's `number()` coercion for one value.
fn number(value: Option<&Value>) -> i64 {
    match value {
        Some(Value::Number(n)) => n
            .as_i64()
            .or_else(|| n.as_f64().map(|f| f.trunc() as i64))
            .unwrap_or(0),
        _ => 0,
    }
}

fn num(args: &Value, key: &str) -> i64 {
    args.get(key)
        .and_then(|v| v.as_i64())
        .or_else(|| args.get(key).and_then(|v| v.as_f64()).map(|f| f.trunc() as i64))
        .unwrap_or(0)
}

fn txt(args: &Value, key: &str) -> String {
    text(args.get(key))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::sync::Mutex as StdMutex;

    fn temp_root(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "codepluse-forge-{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(dir.join(".data")).unwrap();
        dir
    }

    /// One-shot mock GitHub API: answers every connection with `payload`.
    fn mock_github(status: u16, payload: &'static str) -> String {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap().to_string();
        std::thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                let mut stream = stream;
                let mut buffer = [0u8; 4096];
                let _ = stream.read(&mut buffer);
                let reason = if status == 200 { "OK" } else { "Unauthorized" };
                let response = format!(
                    "HTTP/1.1 {status} {reason}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    payload.len(),
                    payload
                );
                let _ = stream.write_all(response.as_bytes());
            }
        });
        format!("http://{address}")
    }

    /// 进程级 STATE 是单例，并行测试必须串行；`seq` 把一个用例（可能多条
    /// 命令）变成原子段。段内 `step` 每条命令前重置状态（等价一次全新进程），
    /// `keep` 沿用上一条的内存态（accounts）。
    static TEST_LOCK: StdMutex<()> = StdMutex::new(());

    fn seq<T>(root: &Path, f: impl FnOnce(&Path) -> T) -> T {
        let _guard = TEST_LOCK.lock().unwrap();
        fresh_state(root);
        f(root)
    }

    fn step(api: &str, root: &Path, command: &str, args: Value) -> Result<Value, String> {
        fresh_state(root);
        run_with_api(api, root, command, &args)
    }

    fn keep(api: &str, root: &Path, command: &str, args: Value) -> Result<Value, String> {
        run_with_api(api, root, command, &args)
    }

    #[test]
    fn accounts_start_empty_and_settings_default() {
        let root = temp_root("empty");
        seq(&root, |root| {
            assert_eq!(step("", root, "get_github_accounts", json!({})).unwrap(), json!([]));
            assert_eq!(
                step("", root, "forge_settings_get", json!({"folderId": 1})).unwrap(),
                json!({"global": {}, "folder": Value::Null})
            );
            assert_eq!(
                step("", root, "forge_auto_status", json!({})).unwrap(),
                json!({"enabled": false, "reason": "自动处理尚未接通"})
            );
            assert_eq!(
                step("", root, "forge_auto_scan_now", json!({})).unwrap(),
                json!({"scanned": 0, "created": 0})
            );
        });
    }

    #[test]
    fn update_accounts_validates_and_persists_login() {
        let api = mock_github(200, r#"{"login":"octocat","name":"Octo","avatar_url":"a.png"}"#);
        let root = temp_root("update");
        seq(&root, |root| {
            keep(&api, root, "update_github_accounts", json!({"settings": {"accounts": [
                {"name": " main ", "token": "tok1"}
            ]}})).unwrap();
            let accounts = keep(&api, root, "get_github_accounts", json!({})).unwrap();
            assert_eq!(accounts[0]["id"], "1");
            assert_eq!(accounts[0]["login"], "octocat");
            assert_eq!(accounts[0]["name"], "main");
            assert_eq!(accounts[0]["server_url"], "");
            assert!(accounts[0].get("token").is_none(), "token 绝不出现在响应里");

            // identity 走已保存的 token。
            assert_eq!(
                keep(&api, root, "forge_identity", json!({})).unwrap(),
                json!({"username": "octocat", "avatar_url": "a.png"})
            );
            // 空 token 且带旧 id：保留旧账户（token 不重填）。
            keep(&api, root, "update_github_accounts",
                json!({"settings": {"accounts": [{"name": "main", "id": "1", "token": ""}]}})).unwrap();
            assert_eq!(
                keep(&api, root, "get_github_accounts", json!({})).unwrap()[0]["login"],
                "octocat"
            );
        });

        // token 只在内存：update 不写设置文件（Go 同样只在 settings_set 落盘）。
        assert!(
            !settings_path(&root).exists(),
            "update_github_accounts 不应写 codeg-forge.json"
        );
    }

    #[test]
    fn update_accounts_error_paths_match_go() {
        let root = temp_root("update-err");
        let unauthorized = mock_github(401, r#"{"message":"Bad credentials"}"#);
        seq(&root, |root| {
            assert_eq!(
                step(&unauthorized, root, "update_github_accounts",
                     json!({"settings": {"accounts": [{"name": "x", "token": "bad"}]}})).unwrap_err(),
                "token 校验失败：token 无效或已过期"
            );
            assert_eq!(
                step(&unauthorized, root, "update_github_accounts",
                     json!({"settings": {"accounts": [{"name": "x", "token": ""}]}})).unwrap_err(),
                "账户 x 缺少 token"
            );
            // 非对象/缺失 settings：Go 的类型断言得 nil → 账户清空。
            step(&unauthorized, root, "update_github_accounts", json!({"settings": "nope"})).unwrap();
            assert_eq!(step(&unauthorized, root, "get_github_accounts", json!({})).unwrap(), json!([]));
        });
    }

    #[test]
    fn validate_token_shapes_by_status() {
        let forbidden = mock_github(403, "{}");
        let root = temp_root("shapes");
        seq(&root, |root| {
            // update_accounts 是 validate_token 的调用方，用它驱动各状态分支。
            let err = step(&forbidden, root, "update_github_accounts",
                json!({"settings": {"accounts": [{"name": "x", "token": "t"}]}})).unwrap_err();
            assert_eq!(err, "token 校验失败：GitHub 返回 403");
        });
    }

    #[test]
    fn dead_path_replicates_go_errors() {
        let root = temp_root("dead");
        // 无 folder / folder 不存在：项目文件夹不存在（Go 的 fall-through）。
        seq(&root, |root| {
            assert_eq!(
                step("", root, "validate_github_token", json!({"token": "t"})).unwrap_err(),
                "项目文件夹不存在"
            );
            assert_eq!(
                step("", root, "validate_github_token", json!({"token": "t", "folderId": 9})).unwrap_err(),
                "项目文件夹不存在"
            );
        });
    }

    #[test]
    fn folder_remote_reads_git_origin() {
        let root = temp_root("remote");
        let project = root.join("proj");
        std::fs::create_dir_all(&project).unwrap();
        std::fs::write(
            root.join(".data").join("codeg-workspace.json"),
            json!({"next": 2, "folders": [{"id": 1, "name": "p", "path": project.to_string_lossy(), "kind": "regular", "open": true}]}).to_string(),
        ).unwrap();
        let run = |args: &[&str]| {
            std::process::Command::new("git").args(args).current_dir(&project).output().unwrap()
        };
        let _ = run(&["init", "-q"]);
        let _ = run(&["remote", "add", "origin", "git@github.com:acme/widget.git"]);

        // folder_forge_remote 不做 host 检查（Go 的检查只在 fall-through 段）。
        seq(&root, |root| {
            assert_eq!(
                step("", root, "folder_forge_remote", json!({"folderId": 1})).unwrap(),
                json!({"provider": "github", "server_host": "github.com", "owner_repo": "acme/widget"})
            );
            // 非 GitHub 远程：坐标原样返回。
            let _ = run(&["remote", "set-url", "origin", "https://gitlab.com/acme/widget.git"]);
            assert_eq!(
                step("", root, "folder_forge_remote", json!({"folderId": 1})).unwrap(),
                json!({"provider": "github", "server_host": "gitlab.com", "owner_repo": "acme/widget"})
            );
            // validate 死路径：非 GitHub 远程在 fall-through 检查处被拒。
            assert_eq!(
                step("", root, "validate_github_token", json!({"token": "t", "folderId": 1})).unwrap_err(),
                "Forge 工作台当前仅支持 GitHub（该远程指向 gitlab.com）"
            );
            // 死路径完整链：github 远程 → 未知的 Forge 命令。
            let _ = run(&["remote", "set-url", "origin", "https://github.com/acme/widget.git"]);
            assert_eq!(
                step("", root, "validate_github_token", json!({"token": "t", "folderId": 1})).unwrap_err(),
                "未知的 Forge 命令：validate_github_token"
            );
        });
    }

    #[test]
    fn parse_github_remote_forms() {
        assert_eq!(
            parse_github_remote("git@github.com:acme/w.git").unwrap(),
            ("github.com".into(), "acme/w".into())
        );
        assert_eq!(
            parse_github_remote("https://github.com/acme/w/").unwrap(),
            ("github.com".into(), "acme/w".into())
        );
        assert_eq!(
            parse_github_remote("http://github.com/acme/w").unwrap(),
            ("github.com".into(), "acme/w".into())
        );
        assert!(parse_github_remote("https://github.com/only-owner").is_err());
        assert!(parse_github_remote("ssh://weird").is_err());
    }

    #[test]
    fn settings_set_roundtrips_and_writes_sorted_file() {
        let root = temp_root("settings");
        seq(&root, |root| {
            let out = step("", root, "forge_settings_set",
                           json!({"folderId": 0, "settings": {"pinned": 3}})).unwrap();
            assert_eq!(out["global"]["pinned"], 3);
            let out = step("", root, "forge_settings_set",
                           json!({"folderId": 7, "settings": {"z": 1, "a": 2}})).unwrap();
            assert_eq!(out["folder"]["7"]["a"], 2);
            let out = step("", root, "forge_settings_set",
                           json!({"folderId": 7, "settings": Value::Null})).unwrap();
            assert!(out["folder"].get("7").is_none(), "folder 设置可删除");
            assert_eq!(
                step("", root, "forge_settings_set", json!({"folderId": 0, "settings": Value::Null})).unwrap_err(),
                "全局设置不能删除"
            );
        });
        // 落盘：顶层键排序、紧凑。
        let raw = std::fs::read_to_string(settings_path(&root)).unwrap();
        assert!(raw.starts_with(r#"{"global":"#), "{raw}");
        assert!(!raw.contains('\n'), "{raw}");
        // 持久化跨“重启”：状态按 root 初始化，文件重读。
        seq(&root, |root| {
            let out = step("", root, "forge_settings_get", json!({"folderId": 0})).unwrap();
            assert_eq!(out["global"]["pinned"], 3);
        });
    }

    #[test]
    fn leading_int_matches_sscanf() {
        assert_eq!(parse_leading_int("12abc"), 12);
        assert_eq!(parse_leading_int("abc"), 0);
        assert_eq!(parse_leading_int("-3x"), -3);
        assert_eq!(parse_leading_int(""), 0);
    }

    // ---- GitHub workbench REST（forge_github.go）----

    use std::sync::Mutex as RouteMutex;

    /// 路由式 mock：按 (method, path) 应答，并记录收到的请求行供断言。
    fn mock_routes(
        routes: &'static [(&'static str, &'static str, u16, &'static str)],
    ) -> (String, std::sync::Arc<RouteMutex<Vec<String>>>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap().to_string();
        let seen: std::sync::Arc<RouteMutex<Vec<String>>> = std::sync::Arc::default();
        let writer = seen.clone();
        std::thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                let mut stream = stream;
                let mut buffer = [0u8; 8192];
                let _ = stream.read(&mut buffer);
                let request = String::from_utf8_lossy(&buffer).into_owned();
                let request_line = request.lines().next().unwrap_or_default().to_string();
                let mut parts = request_line.split_whitespace();
                let method = parts.next().unwrap_or_default().to_string();
                let path = parts
                    .next()
                    .unwrap_or_default()
                    .split(" HTTP")
                    .next()
                    .unwrap_or_default()
                    .to_string();
                writer.lock().unwrap().push(format!("{method} {path}"));
                let mut hit = (200u16, "{}".to_string());
                for (want_method, want_prefix, status, payload) in routes {
                    if *want_method == method && path.starts_with(want_prefix) {
                        hit = (*status, payload.to_string());
                        break;
                    }
                }
                let reason = if hit.0 == 200 { "OK" } else { "Error" };
                let response = format!(
                    "HTTP/1.1 {} {}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    hit.0,
                    reason,
                    hit.1.len(),
                    hit.1
                );
                let _ = stream.write_all(response.as_bytes());
            }
        });
        (format!("http://{address}"), seen)
    }

    const ISSUE_PAYLOAD: &str = r#"{"number":7,"title":"Fix","body":"b","state":"open","labels":[{"name":"bug","color":"ff0000"}],"user":{"login":"ann","avatar_url":"a.png"},"updated_at":"2026-11-14T00:00:00Z","html_url":"h","comments":2}"#;
    const PR_PAYLOAD: &str = r#"{"number":9,"title":"PR","body":"p","state":"closed","merged_at":"2026-11-14T01:00:00Z","pull_request":{"draft":true},"user":{"login":"bo","avatar_url":"b.png"},"head":{"ref":"h1","sha":"s1","repo":{"full_name":"acme/fork"}},"base":{"ref":"main"},"mergeable":true,"mergeable_state":"clean","updated_at":"u","html_url":"h","comments":0}"#;

    fn rest_fixture(tag: &str, routes: &'static [(&'static str, &'static str, u16, &'static str)]) -> (PathBuf, String, std::sync::Arc<RouteMutex<Vec<String>>>) {
        let root = temp_root(tag);
        let project = root.join("proj");
        std::fs::create_dir_all(&project).unwrap();
        std::fs::write(
            root.join(".data").join("codeg-workspace.json"),
            json!({"next": 2, "folders": [{"id": 1, "name": "p", "path": project.to_string_lossy(), "kind": "regular", "open": true}]}).to_string(),
        ).unwrap();
        let _ = std::process::Command::new("git").args(["init", "-q"]).current_dir(&project).output();
        let _ = std::process::Command::new("git").args(["remote", "add", "origin", "https://github.com/acme/widget.git"]).current_dir(&project).output();
        let (api, seen) = mock_routes(routes);
        (root, api, seen)
    }

    fn setup_account(root: &Path, api: &str) {
        keep(api, root, "update_github_accounts", json!({"settings": {"accounts": [{"name": "a", "token": "t"}]}})).unwrap();
    }

    #[test]
    fn list_issues_shapes_rows_and_query() {
        let routes = &[
            ("GET", "/search/issues", 200, r#"{"total_count": 1200, "items": [ISSUE, PR]}"#),
        ];
        // 静态载荷里嵌占位再替换：mock 是 &'static str，用 const 拼。
        let _ = routes;
        let routes: &'static [(&'static str, &'static str, u16, &'static str)] = &[
            ("GET", "/search/issues", 200, concat!(
                r#"{"total_count": 1200, "items": ["#,
                r#"{"number":7,"title":"Fix","body":"b","state":"open","labels":[{"name":"bug","color":"ff0000"}],"user":{"login":"ann","avatar_url":"a.png"},"updated_at":"2026-11-14T00:00:00Z","html_url":"h","comments":2},"#,
                r#"{"number":9,"title":"PR","body":"p","state":"closed","merged_at":"2026-11-14T01:00:00Z","pull_request":{"draft":true},"user":{"login":"bo","avatar_url":"b.png"},"head":{"ref":"h1","sha":"s1","repo":{"full_name":"acme/fork"}},"base":{"ref":"main"},"mergeable":true,"mergeable_state":"clean","updated_at":"u","html_url":"h","comments":0}"#,
                r#"]}"#)),
        ];
        let (root, api, seen) = rest_fixture("list-issues", routes);
        seq(&root, |root| {
            setup_account(root, &api);
            let out = keep(&api, root, "forge_list_issues", json!({
                "folderId": 1,
                "query": {"tab": "pulls", "state": "all", "page": 2, "perPage": 50,
                          "assignedMe": true, "labels": ["bug", "ui"], "search": "in:title fix",
                          "sort": "updated", "accountId": ""}
            })).unwrap();
            assert_eq!(out["page"], 2);
            assert_eq!(out["per_page"], 50);
            assert_eq!(out["total_count"], 1200);
            assert_eq!(out["reachable_count"], 1000, "GitHub 搜索上限 1000");
            assert_eq!(out["has_next"], true);
            let rows = out["rows"].as_array().unwrap();
            assert_eq!(rows[0]["is_pr"], false);
            assert_eq!(rows[0]["labels"][0]["color"], "#ff0000");
            assert_eq!(rows[0]["author"], "ann");
            assert_eq!(rows[1]["is_pr"], true);
            assert_eq!(rows[1]["state"], "merged", "merged_at 非空 → merged");
            assert_eq!(rows[1]["draft"], true);
            // 查询串：标签带引号、空格转 +、sort 追加。
            let requests = seen.lock().unwrap().clone();
            let search = requests.iter().find(|r| r.contains("/search/issues")).unwrap();
            assert!(search.contains("repo%3Aacme%2Fwidget"), "{search}");
            assert!(search.contains("is%3Apulls"), "{search}");
            assert!(search.contains("assignee%3A%40me"), "{search}");
            assert!(search.contains("label%3A%22bug%22"), "{search}");
            assert!(search.contains("in%3Atitle+fix"), "{search}");
            assert!(search.ends_with("sort=updated&order=desc"), "{search}");
        });
    }

    #[test]
    fn tab_count_issue_detail_labels_comments_roundtrip() {
        let routes: &'static [(&'static str, &'static str, u16, &'static str)] = &[
            ("GET", "/repos/acme/widget/issues/7/comments", 200, r#"[{"id":5,"user":{"login":"ann","avatar_url":"a.png"},"body":"hi","created_at":"t","html_url":"h"}]"#),
            ("GET", "/repos/acme/widget/issues/7", 200, ISSUE_PAYLOAD),
            ("GET", "/repos/acme/widget/labels", 200, r#"[{"name":"bug","color":"ff0000"},{"name":"no-color","color":""}]"#),
            ("GET", "/search/issues", 200, r#"{"total_count": 41}"#),
        ];
        let (root, api, seen) = rest_fixture("tabs", routes);
        seq(&root, |root| {
            setup_account(root, &api);
            let out = keep(&api, root, "forge_tab_count", json!({"folderId": 1, "query": {}})).unwrap();
            assert_eq!(out["count"], 41);
            let detail = keep(&api, root, "forge_issue_detail", json!({"folderId": 1, "query": {"number": 7}})).unwrap();
            assert_eq!(detail["body"], "b", "detail 不截断 body");
            let labels = keep(&api, root, "forge_list_labels", json!({"folderId": 1, "accountId": ""})).unwrap();
            assert_eq!(labels["labels"][0]["color"], "#ff0000");
            assert_eq!(labels["labels"][1]["color"], "", "空色保持空");
            let comments = keep(&api, root, "forge_list_comments", json!({"folderId": 1, "filters": {"number": 7}})).unwrap();
            assert_eq!(comments["comments"][0]["id"], "5");
            assert_eq!(comments["has_next"], false);
            let requests = seen.lock().unwrap().clone();
            assert!(requests.iter().any(|r| r.contains("per_page=100") && r.contains("/labels")));
        });
    }

    #[test]
    fn create_comment_state_issue_and_merge_flow() {
        let routes: &'static [(&'static str, &'static str, u16, &'static str)] = &[
            ("POST", "/repos/acme/widget/issues/7/comments", 200, r#"{"id":9,"user":{"login":"ann","avatar_url":"a.png"},"body":"c","created_at":"t","html_url":"h"}"#),
            ("PATCH", "/repos/acme/widget/pulls/9", 200, PR_PAYLOAD),
            ("POST", "/repos/acme/widget/issues", 200, ISSUE_PAYLOAD),
            ("PUT", "/repos/acme/widget/pulls/7/merge", 200, r#"{"merged": true}"#),
            ("GET", "/repos/acme/widget/pulls/7", 200, PR_PAYLOAD),
        ];
        let (root, api, seen) = rest_fixture("create", routes);
        seq(&root, |root| {
            setup_account(root, &api);
            let comment = keep(&api, root, "forge_create_comment", json!({"folderId": 1, "draft": {"number": 7, "body": "c"}})).unwrap();
            assert_eq!(comment["id"], "9");
            let state = keep(&api, root, "forge_set_item_state", json!({"folderId": 1, "request": {"kind": "pr", "number": 9, "action": "reopen"}})).unwrap();
            assert_eq!(state["is_pr"], true);
            let issue = keep(&api, root, "forge_create_issue", json!({"folderId": 1, "draft": {"title": "T", "body": "", "labels": []}})).unwrap();
            assert_eq!(issue["number"], 7);
            let merged = keep(&api, root, "forge_merge_change", json!({"folderId": 1, "request": {"number": 7, "method": "squash"}})).unwrap();
            assert_eq!(merged["is_pr"], true, "merged 后回读 PR 行");
            let requests = seen.lock().unwrap().clone();
            assert!(requests.iter().any(|r| r.starts_with("PATCH /repos/acme/widget/pulls/9")));
            assert!(requests.iter().any(|r| r.starts_with("PUT /repos/acme/widget/pulls/7/merge")));
            // create_issue：body 为空不带 body 字段；labels 空不带 labels。
            assert!(requests.iter().any(|r| *r == "POST /repos/acme/widget/issues"), "create_issue 只打 issue 创建端点");
        });
    }

    #[test]
    fn change_detail_files_merge_options() {
        let routes: &'static [(&'static str, &'static str, u16, &'static str)] = &[
            ("GET", "/repos/acme/widget/pulls/9/files", 200, r#"[{"filename":"a.go","additions":1,"deletions":2,"changes":3,"status":"modified"}]"#),
            ("GET", "/repos/acme/widget/commits/s1/check-runs", 200, r#"{"check_runs":[{"name":"ci","conclusion":"success","html_url":"c"},{"name":"build","conclusion":"","status":"in_progress","html_url":"b"}]}"#),
            ("GET", "/repos/acme/widget/pulls/9", 200, PR_PAYLOAD),
            ("GET", "/repos/acme/widget", 200, r#"{"allow_merge_commit": false, "allow_squash_merge": true, "allow_rebase_merge": true}"#),
        ];
        let (root, api, seen) = rest_fixture("detail", routes);
        seq(&root, |root| {
            setup_account(root, &api);
            let detail = keep(&api, root, "forge_change_detail", json!({"folderId": 1, "query": {"number": 9}})).unwrap();
            assert_eq!(detail["head_repo"], "acme/fork");
            assert_eq!(detail["head_sha"], "s1");
            let checks = detail["checks"].as_array().unwrap();
            assert_eq!(checks[0]["state"], "success");
            assert_eq!(checks[1]["state"], "in_progress", "conclusion 空 → status");
            let files = keep(&api, root, "forge_change_files", json!({"folderId": 1, "query": {"number": 9}})).unwrap();
            assert_eq!(files["files"][0]["file"], "a.go");
            assert_eq!(files["has_next"], false);
            let options = keep(&api, root, "forge_merge_options", json!({"folderId": 1, "accountId": ""})).unwrap();
            assert_eq!(options["methods"], json!(["squash", "rebase"]));
            assert_eq!(options["default_method"], "squash");
            assert_eq!(options["merge_strategy"], "commit");
            // check-runs 请求带 sha。
            assert!(seen.lock().unwrap().iter().any(|r| r.contains("commits/s1/check-runs")));
        });
    }

    #[test]
    fn merge_not_merged_answers_null_and_non_github_host_rejected() {
        let routes: &'static [(&'static str, &'static str, u16, &'static str)] = &[
            ("PUT", "/repos/acme/widget/pulls/7/merge", 200, r#"{"merged": false, "message": "not mergeable"}"#),
        ];
        let (root, api, _seen) = rest_fixture("nomerge", routes);
        seq(&root, |root| {
            setup_account(root, &api);
            let out = keep(&api, root, "forge_merge_change", json!({"folderId": 1, "request": {"number": 7}})).unwrap();
            assert_eq!(out, Value::Null);
            // 非 GitHub 远程：fall-through 检查。
            let project = root.join("proj");
            let _ = std::process::Command::new("git").args(["remote", "set-url", "origin", "https://gitlab.com/acme/widget.git"]).current_dir(&project).output();
            assert_eq!(
                keep(&api, root, "forge_list_issues", json!({"folderId": 1, "query": {}})).unwrap_err(),
                "Forge 工作台当前仅支持 GitHub（该远程指向 gitlab.com）"
            );
        });
    }

    #[test]
    fn query_escape_matches_go() {
        assert_eq!(query_escape("a b&c=d/e"), "a+b%26c%3Dd%2Fe");
        assert_eq!(query_escape("label:\"x\""), "label%3A%22x%22");
        assert_eq!(query_escape("汉字"), "%E6%B1%89%E5%AD%97");
        assert_eq!(query_escape("a.b-c_d~e"), "a.b-c_d~e");
    }
}
