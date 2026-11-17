//! Office 监视续期与关窗请求 —— 补全批次（S8，2 条）。
//!
//! 前端 `codeg-ui/src/lib/api.ts` 已经在调用这 2 条命令，但原 Go sidecar
//! （提交 76712f5）与 Rust 都没有实现过它们（全树搜不到字面量），契约只取自
//! 调用点参数名与 `types.ts` 的返回结构。
//!
//! 命令清单：
//! * `renew_office_watch` —— 续期某路 Office 监视的持有租约，回是否仍是
//!   服务该文件的活租约（`boolean`）。
//! * `resolve_close_request` —— 应答“是否退出/最小化”的关窗提示：清掉后端
//!   “提示已弹出”标记，落盘选择并通过 `app://close-request` 频道广播。
//!
//! 限制（L3）：真实的 Office 文件监视由 OfficeCLI watcher 提供，而 OfficeCLI
//! 二进制不在本仓库分发（见 `office.rs`）；本模块在 `.data/` 下自维护一份租约
//! 状态表，续期只刷新本表的时间戳，不代表底层 watcher 真的还在跑。租约 TTL
//! 内重复续期算活；`hold` 对不上说明被新租约顶替，回 `false`；过期或缺记录
//! 则重新登记并回 `true`。

use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::{json, Value};

use crate::backend::store_file;

/// 租约续期有效期（毫秒）：超过这个时间没续期就算过期。
const LEASE_TTL_MS: u64 = 120_000;
/// 关窗请求事件频道（与前端 `CLOSE_REQUEST_EVENT` 对齐）。
const CLOSE_REQUEST_EVENT: &str = "app://close-request";
/// 允许的关窗动作（与前端 `action: "minimize" | "exit" | "cancel"` 对齐）。
const CLOSE_ACTIONS: [&str; 3] = ["minimize", "exit", "cancel"];

pub fn owns(command: &str) -> bool {
    matches!(command, "renew_office_watch" | "resolve_close_request")
}

pub fn handle(root: &Path, command: &str, args: &Value) -> Option<Result<Value, String>> {
    if !owns(command) {
        return None;
    }
    Some(run(root, command, args))
}

fn run(root: &Path, command: &str, args: &Value) -> Result<Value, String> {
    match command {
        "renew_office_watch" => renew_watch(root, args),
        "resolve_close_request" => resolve_close_request(root, args),
        _ => Err(format!("未知的 Office 扩展命令：{command}")),
    }
}

// ── 时间与路径 ──────────────────────────────────────────────────────────

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_millis() as u64)
        .unwrap_or_default()
}

fn watches_path(root: &Path) -> PathBuf {
    root.join(".data").join("office-watches.json")
}

fn close_request_path(root: &Path) -> PathBuf {
    root.join(".data").join("close-request.json")
}

fn required_str(args: &Value, field: &str) -> Result<String, String> {
    args.get(field)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| format!("缺少 {field} 参数"))
        .map(str::to_string)
}

// ── 监视租约 ────────────────────────────────────────────────────────────

fn renew_watch(root: &Path, args: &Value) -> Result<Value, String> {
    let root_path = required_str(args, "rootPath")?;
    let path = required_str(args, "path")?;
    let cap = required_str(args, "cap")?;
    let hold = args
        .get("hold")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string);

    let store_path = watches_path(root);
    let _lock = store_file::lock(&store_path)?;
    let mut doc = store_file::read_json(&store_path)
        .ok()
        .flatten()
        .filter(Value::is_object)
        .unwrap_or_else(|| json!({}));
    let leases = doc
        .as_object_mut()
        .ok_or_else(|| "Office 监视记录格式无效".to_string())?
        .entry("leases")
        .or_insert_with(|| json!({}));
    let leases = leases
        .as_object_mut()
        .ok_or_else(|| "Office 监视记录格式无效".to_string())?;

    let now = now_ms();

    if let Some(entry) = leases.get(&cap) {
        let entry_hold = entry.get("hold").and_then(Value::as_str).map(str::to_string);
        if hold.as_ref() != entry_hold.as_ref() {
            // 被新租约顶替：不刷新，回 false。
            return Ok(json!(false));
        }
        // 过期租约按重新登记处理（下面 upsert 刷新时间戳，仍回 true）。
        let renewed_at = entry
            .get("renewed_at_ms")
            .and_then(Value::as_u64)
            .unwrap_or(0);
        let _fresh = now.saturating_sub(renewed_at) <= LEASE_TTL_MS;
    }

    leases.insert(
        cap.clone(),
        json!({
            "rootPath": root_path,
            "path": path,
            "hold": hold,
            "renewed_at_ms": now,
        }),
    );
    store_file::write_json(&store_path, &doc)?;
    // 登记或续期成功都回 true；只有被新 hold 顶替才回 false。
    Ok(json!(true))
}

// ── 关窗请求状态机 ───────────────────────────────────────────────────────

fn resolve_close_request(root: &Path, args: &Value) -> Result<Value, String> {
    let action = required_str(args, "action")?;
    if !CLOSE_ACTIONS.contains(&action.as_str()) {
        return Err(format!("非法的关窗动作：{action}"));
    }
    let remember = args
        .get("remember")
        .and_then(Value::as_bool)
        .ok_or_else(|| "remember 必须是布尔值".to_string())?;

    let record = json!({
        "open": false,
        "action": action,
        "remember": remember,
        "resolved_at_ms": now_ms(),
    });
    let path = close_request_path(root);
    let _lock = store_file::lock(&path)?;
    store_file::write_json(&path, &record)?;

    // 广播应答结果；前端提示框据此关闭。
    crate::backend::emit_event(
        CLOSE_REQUEST_EVENT,
        json!({ "resolved": true, "action": action, "remember": remember }),
    );
    Ok(Value::Null)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn temp_root(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "codepluse-office-ext-{tag}-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn call(root: &Path, command: &str, args: Value) -> Result<Value, String> {
        handle(root, command, &args).unwrap()
    }

    #[test]
    fn owns_covers_the_two_documented_commands_only() {
        for command in ["renew_office_watch", "resolve_close_request"] {
            assert!(owns(command), "{command} should be owned");
        }
        assert!(!owns("start_office_watch"));
        assert!(!owns("stop_office_watch"));
        assert!(!owns("RENEW_OFFICE_WATCH"));
    }

    #[test]
    fn renew_registers_a_lease_and_keeps_it_alive_with_matching_hold() {
        let root = temp_root("renew");
        let first = call(
            &root,
            "renew_office_watch",
            json!({ "rootPath": "/ws", "path": "a.docx", "cap": "cap-1", "hold": "hold-a" }),
        )
        .unwrap();
        // 首条记录之前没有活租约，按“重新登记”回 true。
        assert_eq!(first, json!(true));

        let second = call(
            &root,
            "renew_office_watch",
            json!({ "rootPath": "/ws", "path": "a.docx", "cap": "cap-1", "hold": "hold-a" }),
        )
        .unwrap();
        assert_eq!(second, json!(true));

        let doc: Value =
            serde_json::from_slice(&fs::read(watches_path(&root)).unwrap()).unwrap();
        let lease = &doc["leases"]["cap-1"];
        assert_eq!(lease["path"], "a.docx");
        assert_eq!(lease["hold"], "hold-a");
    }

    #[test]
    fn renew_reports_false_when_the_hold_was_replaced() {
        let root = temp_root("renew-replaced");
        call(
            &root,
            "renew_office_watch",
            json!({ "rootPath": "/ws", "path": "a.docx", "cap": "cap-1", "hold": "hold-a" }),
        )
        .unwrap();
        let replaced = call(
            &root,
            "renew_office_watch",
            json!({ "rootPath": "/ws", "path": "a.docx", "cap": "cap-1", "hold": "hold-b" }),
        )
        .unwrap();
        assert_eq!(replaced, json!(false));
    }

    #[test]
    fn renew_rejects_missing_required_fields() {
        let root = temp_root("renew-bad");
        assert!(call(&root, "renew_office_watch", json!({}))
            .unwrap_err()
            .contains("缺少 rootPath"));
        assert!(call(
            &root,
            "renew_office_watch",
            json!({ "rootPath": "/ws", "path": "a.docx" })
        )
        .unwrap_err()
        .contains("缺少 cap"));
    }

    #[test]
    fn resolve_close_request_clears_the_prompt_and_persists_the_choice() {
        let root = temp_root("close");
        let out = call(
            &root,
            "resolve_close_request",
            json!({ "action": "minimize", "remember": true }),
        )
        .unwrap();
        assert!(out.is_null());

        let record: Value =
            serde_json::from_slice(&fs::read(close_request_path(&root)).unwrap()).unwrap();
        assert_eq!(record["open"], false);
        assert_eq!(record["action"], "minimize");
        assert_eq!(record["remember"], true);
        assert!(record["resolved_at_ms"].as_u64().unwrap() > 0);
    }

    #[test]
    fn resolve_close_request_rejects_bad_action_and_remember() {
        let root = temp_root("close-bad");
        assert!(call(
            &root,
            "resolve_close_request",
            json!({ "action": "destroy", "remember": false })
        )
        .unwrap_err()
        .contains("非法的关窗动作"));
        assert!(call(
            &root,
            "resolve_close_request",
            json!({ "action": "exit", "remember": "yes" })
        )
        .unwrap_err()
        .contains("remember 必须是布尔值"));
        assert!(call(&root, "resolve_close_request", json!({}))
            .unwrap_err()
            .contains("缺少 action"));
    }

    #[test]
    fn unknown_commands_fall_through_to_the_next_module() {
        assert!(handle(&temp_root("fallthrough"), "pet_list", &json!({})).is_none());
    }
}
