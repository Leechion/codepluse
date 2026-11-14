//! The collaboration domain, ported from
//! `server-go/internal/codeg/delegation.go` (plus `delegation_profiles.go`).
//!
//! A delegation is a child session the model opened on another harness: the
//! parent records the task, the child runs it, and the parent collects the
//! result. The registry lives in the connection layer (`state.delegations`)
//! and is mirrored to `.data/delegations.json`, so a restart keeps the tasks.
//!
//! The tools the companion exposes:
//! - `delegate_to_agent` — start a child task on another harness,
//! - `get_delegation_status` — collect the results (optionally waiting),
//! - `cancel_delegation` — stop a running task,
//! - `resume_delegation` — continue an unfinished task in the same session,
//! - `send_agent_message` / `check_user_feedback` — the inbox between tasks.

use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;

use serde_json::{json, Value};

use super::agents;
use super::connections::Layer;
use super::tool_identity::Caller;

/// Go `Delegation` (delegation.go:20).
#[derive(Clone, Default)]
pub struct Delegation {
    pub id: String,
    pub parent: String,
    pub root: String,
    pub agent_type: String,
    pub task: String,
    pub workdir: String,
    pub model: String,
    pub permission: String,
    pub mode: String,
    pub status: String,
    pub result: String,
    pub error: String,
    pub connection_id: String,
    pub conversation_id: i64,
    pub parent_conversation_id: i64,
    pub native_session_id: String,
    pub next_prompt: String,
    pub depth: i64,
    pub parent_tool_use_id: String,
    pub split_id: String,
    pub title: String,
    pub inbox: Vec<Value>,
}

/// Go `taskReport` (delegation.go:445): the row the model sees.
pub fn task_report(task: &Delegation) -> Value {
    let mut row = json!({
        "task_id": task.id,
        "status": task.status,
        "agent_type": task.agent_type,
        "task_preview": task.task,
    });
    if !task.split_id.is_empty() {
        row["split_id"] = json!(task.split_id);
    }
    if task.status == "interrupted" {
        row["status"] = json!("failed");
        row["error_code"] = json!("interrupted");
    }
    if task.conversation_id != 0 {
        row["child_conversation_id"] = json!(task.conversation_id);
    }
    if task.status == "completed" {
        row["text"] = json!(task.result);
    }
    if !task.error.is_empty() {
        row["message"] = json!(task.error);
        if task.status != "interrupted" {
            row["error_code"] = json!("subagent_error");
        }
    }
    if task.status == "canceled" {
        row["error_code"] = json!("canceled");
    }
    row
}

/// The registry file (Go `saveDelegationsLocked`).
fn registry_path(root: &Path) -> std::path::PathBuf {
    root.join(".data").join("delegations.json")
}

/// Reads the persisted registry back into the layer state.
pub fn load(root: &Path, into: &mut HashMap<String, Delegation>) {
    let Ok(Some(value)) = crate::backend::store_file::read_json(&registry_path(root)) else {
        return;
    };
    let Some(rows) = value.get("delegations").and_then(|value| value.as_array()) else {
        return;
    };
    for row in rows {
        let text = |key: &str| {
            row.get(key)
                .and_then(|value| value.as_str())
                .unwrap_or("")
                .to_string()
        };
        let id = text("task_id");
        if id.is_empty() {
            continue;
        }
        into.insert(
            id.clone(),
            Delegation {
                id,
                parent: text("parent"),
                root: text("root"),
                agent_type: text("agent_type"),
                task: text("task"),
                workdir: text("workdir"),
                model: text("model"),
                permission: text("permission"),
                mode: text("mode"),
                status: text("status"),
                result: text("result"),
                error: text("error"),
                connection_id: text("child_connection_id"),
                conversation_id: row
                    .get("child_conversation_id")
                    .and_then(|value| value.as_i64())
                    .unwrap_or(0),
                parent_conversation_id: row
                    .get("parent_conversation_id")
                    .and_then(|value| value.as_i64())
                    .unwrap_or(0),
                native_session_id: text("native_session_id"),
                next_prompt: text("next_prompt"),
                depth: row.get("depth").and_then(|value| value.as_i64()).unwrap_or(0),
                parent_tool_use_id: text("parent_tool_use_id"),
                split_id: text("split_id"),
                title: text("title"),
                inbox: Vec::new(),
            },
        );
    }
}

/// Writes the registry back (Go `saveDelegationsLocked`).
pub fn save(root: &Path, tasks: &HashMap<String, Delegation>) -> Result<(), String> {
    let mut rows: Vec<Value> = Vec::new();
    let mut sorted: Vec<&Delegation> = tasks.values().collect();
    sorted.sort_by(|left, right| left.id.cmp(&right.id));
    for task in sorted {
        rows.push(json!({
            "task_id": task.id,
            "parent": task.parent,
            "root": task.root,
            "agent_type": task.agent_type,
            "task": task.task,
            "workdir": task.workdir,
            "model": task.model,
            "permission": task.permission,
            "mode": task.mode,
            "status": task.status,
            "result": task.result,
            "error": task.error,
            "child_connection_id": task.connection_id,
            "child_conversation_id": task.conversation_id,
            "parent_conversation_id": task.parent_conversation_id,
            "native_session_id": task.native_session_id,
            "next_prompt": task.next_prompt,
            "depth": task.depth,
            "parent_tool_use_id": task.parent_tool_use_id,
            "split_id": task.split_id,
            "title": task.title,
        }));
    }
    let path = registry_path(root);
    let _guard = crate::backend::store_file::lock(&path)?;
    crate::backend::store_file::write_json(&path, &json!({ "delegations": rows }))
}

impl Layer {
    /// Go `delegateToAgent` (delegation.go:227): start a child task.
    pub async fn delegate_to_agent(&self, caller: &Caller, args: &Value) -> Result<Value, String> {
        if caller.depth >= 4 {
            return Err("委派深度达到 4 层上限".into());
        }
        let agent_type = args
            .get("agent_type")
            .and_then(|value| value.as_str())
            .unwrap_or("")
            .to_string();
        let task = args
            .get("task")
            .and_then(|value| value.as_str())
            .unwrap_or("")
            .trim()
            .to_string();
        if task.is_empty() || task.len() > 120_000 {
            return Err("子任务必须包含完整目标，且不超过 120000 字节".into());
        }
        let agent = agents::pick(&self.root, &agent_type)
            .ok_or_else(|| format!("执行端不可用：{agent_type}"))?;
        let model = args
            .get("model")
            .and_then(|value| value.as_str())
            .unwrap_or("")
            .to_string();

        // The child inherits the parent's working directory and permission
        // level (Go `delegateScope`).
        let (workdir, permission, mode, parent_conversation) = {
            let state = self.state.lock().unwrap();
            let Some(parent) = state.connections.get(&caller.connection_id) else {
                return Err("父会话已断开".into());
            };
            let parent = parent.lock().unwrap();
            let permission = if parent.options.permission == "workspace" {
                "workspace".to_string()
            } else {
                "readonly".to_string()
            };
            (
                parent.options.workdir.clone(),
                permission.clone(),
                parent.options.mode.clone(),
                parent.conversation_id,
            )
        };
        let requested_dir = args
            .get("working_dir")
            .and_then(|value| value.as_str())
            .unwrap_or("")
            .to_string();
        let workdir = if requested_dir.is_empty() {
            workdir
        } else {
            super::connections::resolve_workdir(&self.root, &requested_dir)?
                .to_string_lossy()
                .to_string()
        };
        let mode = if mode.is_empty() {
            if permission == "workspace" {
                "workspace-write".to_string()
            } else {
                "plan".to_string()
            }
        } else {
            mode
        };

        let mut record = Delegation {
            id: crate::backend::goformat::hex_id(),
            parent: caller.connection_id.clone(),
            root: caller.root_id.clone(),
            agent_type: agent_type.clone(),
            task: task.clone(),
            workdir: workdir.clone(),
            model: model.clone(),
            permission,
            mode: mode.clone(),
            status: "running".into(),
            parent_conversation_id: parent_conversation,
            depth: caller.depth + 1,
            ..Delegation::default()
        };
        record.parent_tool_use_id = record.id.clone();

        // Register before launching: a cancel that races in must find the row.
        {
            let mut state = self.state.lock().unwrap();
            state.delegations.insert(record.id.clone(), record.clone());
        }
        let _ = save(&self.root, &self.state.lock().unwrap().delegations);

        // Launch the child session.
        let connect_args = json!({
            "agentType": agent_type,
            "workingDir": workdir,
            "preferredModeId": mode,
            "preferredConfigValues": { "model": model },
        });
        let cid = match self.connect(&connect_args).await {
            Ok(cid) => cid,
            Err(error) => {
                self.finish_delegation(&record.id, "failed", "", &error);
                return Err(error);
            }
        };
        {
            let mut state = self.state.lock().unwrap();
            if let Some(entry) = state.delegations.get_mut(&record.id) {
                entry.connection_id = cid.clone();
            }
            if let Some(child) = state.connections.get(&cid) {
                let mut child = child.lock().unwrap();
                child.depth = record.depth;
            }
        }

        // Prompt the child; the turn runs in the background, so a watcher
        // collects the result.
        let prompt = json!({
            "connectionId": cid,
            "blocks": [{"type": "text", "text": task}],
        });
        if let Err(error) = self.prompt(&prompt).await {
            self.finish_delegation(&record.id, "failed", "", &error);
            return Err(error);
        }
        let run_seq = {
            let state = self.state.lock().unwrap();
            state
                .connections
                .get(&cid)
                .map(|conn| conn.lock().unwrap().run_seq)
                .unwrap_or(0)
        };
        self.watch_delegation(record.id.clone(), cid.clone(), run_seq);

        let state = self.state.lock().unwrap();
        let record = state.delegations.get(&record.id).cloned().unwrap_or(record);
        Ok(task_report(&record))
    }

    /// Waits for the child's turn to finish and records its result.
    fn watch_delegation(&self, task_id: String, cid: String, run_seq: u64) {
        let layer = self.self_arc();
        tokio::spawn(async move {
            for _ in 0..3000 {
                tokio::time::sleep(std::time::Duration::from_millis(200)).await;
                let (done, output, canceled) = {
                    let state = layer.state.lock().unwrap();
                    let Some(record) = state.delegations.get(&task_id) else {
                        return;
                    };
                    if record.status == "canceled" {
                        (true, String::new(), true)
                    } else {
                        match state.connections.get(&cid) {
                            Some(conn) => {
                                let conn = conn.lock().unwrap();
                                (
                                    conn.output_run_seq >= run_seq && run_seq > 0,
                                    conn.last_output.clone(),
                                    false,
                                )
                            }
                            None => (true, String::new(), false),
                        }
                    }
                };
                if !done {
                    continue;
                }
                if canceled {
                    return;
                }
                if output.is_empty() {
                    layer.finish_delegation(&task_id, "failed", "", "子会话没有返回结果");
                } else {
                    layer.finish_delegation(&task_id, "completed", &output, "");
                }
                return;
            }
            layer.finish_delegation(&task_id, "failed", "", "子会话超时未完成");
        });
    }

    /// Records the outcome of a delegation.
    pub fn finish_delegation(&self, task_id: &str, status: &str, result: &str, error: &str) {
        let snapshot = {
            let mut state = self.state.lock().unwrap();
            let Some(record) = state.delegations.get_mut(task_id) else {
                return;
            };
            if record.status == "canceled" {
                return;
            }
            record.status = status.to_string();
            record.result = result.to_string();
            record.error = error.to_string();
            record.clone()
        };
        let _ = save(&self.root, &self.state.lock().unwrap().delegations);
        self.event(
            &snapshot.parent,
            json!({
                "type": "delegation_completed",
                "parent_connection_id": snapshot.parent,
                "parent_tool_use_id": snapshot.parent_tool_use_id,
                "child_connection_id": snapshot.connection_id,
                "child_conversation_id": snapshot.conversation_id,
                "agent_type": snapshot.agent_type,
                "task_id": snapshot.id,
                "result": {
                    "kind": if snapshot.status == "completed" { "ok" } else { "err" },
                    "error_code": if snapshot.status == "completed" { Value::Null } else { json!(snapshot.status) },
                    "text_preview": snapshot.result,
                },
            }),
        );
    }

    /// Go `taskStatus` + the `get_delegation_status` tool.
    pub async fn delegation_status(
        &self,
        caller: &Caller,
        ids: Option<Vec<String>>,
        wait_ms: Option<i64>,
    ) -> Result<Value, String> {
        let mut wait = wait_ms.unwrap_or(0);
        if wait < 0 {
            return Err("wait_ms 不能为负数".into());
        }
        if wait > 60_000 {
            wait = 60_000;
        }
        let deadline = std::time::Instant::now() + std::time::Duration::from_millis(wait as u64);
        loop {
            let (rows, finished) = self.task_rows(caller, ids.clone());
            if wait == 0 || finished || std::time::Instant::now() >= deadline {
                return Ok(json!({ "tasks": rows }));
            }
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        }
    }

    /// The rows of the caller's tasks; `finished` says whether every asked
    /// task has left the running state.
    fn task_rows(&self, caller: &Caller, ids: Option<Vec<String>>) -> (Vec<Value>, bool) {
        let state = self.state.lock().unwrap();
        let mut ids = ids.unwrap_or_default();
        if ids.is_empty() {
            let mut all: Vec<String> = state
                .delegations
                .values()
                .filter(|task| task.root == caller.root_id)
                .map(|task| task.id.clone())
                .collect();
            all.sort();
            ids = all;
        }
        let mut rows = Vec::new();
        let mut finished = true;
        for id in ids {
            let Some(task) = state.delegations.get(&id) else {
                continue;
            };
            if task.root != caller.root_id {
                continue;
            }
            if task.status == "running" {
                finished = false;
            }
            rows.push(task_report(task));
        }
        (rows, finished)
    }

    /// Go `cancel_delegation`.
    pub async fn cancel_delegation(&self, caller: &Caller, task_id: &str) -> Result<Value, String> {
        let (connection_id, already) = {
            let mut state = self.state.lock().unwrap();
            let Some(task) = state.delegations.get_mut(task_id) else {
                return Err("任务不存在".into());
            };
            if task.root != caller.root_id {
                return Err("任务不存在".into());
            }
            if task.status != "running" {
                (task.connection_id.clone(), true)
            } else {
                task.status = "canceled".into();
                (task.connection_id.clone(), false)
            }
        };
        let _ = save(&self.root, &self.state.lock().unwrap().delegations);
        if already {
            let state = self.state.lock().unwrap();
            let record = state.delegations.get(task_id).cloned();
            return Ok(json!({
                "task_id": task_id,
                "status": record.map(|task| task.status).unwrap_or_default(),
            }));
        }
        if !connection_id.is_empty() {
            let _ = self.cancel(&connection_id).await;
        }
        Ok(json!({ "task_id": task_id, "status": "canceled" }))
    }

    /// Go `resumeDelegation`: continue an unfinished task in its own session.
    pub async fn resume_delegation(
        &self,
        caller: &Caller,
        task_id: &str,
        task: &str,
    ) -> Result<Value, String> {
        let (connection_id, agent_type, next_prompt) = {
            let state = self.state.lock().unwrap();
            let Some(record) = state.delegations.get(task_id) else {
                return Err("任务不存在".into());
            };
            if record.root != caller.root_id {
                return Err("任务不存在".into());
            }
            (
                record.connection_id.clone(),
                record.agent_type.clone(),
                record.next_prompt.clone(),
            )
        };
        if connection_id.is_empty() {
            return Err("任务没有可继续的会话".into());
        }
        let mut input = "继续此前未完成的任务，沿用当前会话上下文与已有成果。".to_string();
        if !next_prompt.is_empty() {
            input.push('\n');
            input.push_str(&next_prompt);
        }
        if !task.is_empty() {
            input.push('\n');
            input.push_str(task);
        }
        let prompt = json!({
            "connectionId": connection_id,
            "blocks": [{"type": "text", "text": input}],
        });
        if let Err(error) = self.prompt(&prompt).await {
            self.finish_delegation(task_id, "failed", "", &error);
            return Err(error);
        }
        let run_seq = {
            let state = self.state.lock().unwrap();
            state
                .connections
                .get(&connection_id)
                .map(|conn| conn.lock().unwrap().run_seq)
                .unwrap_or(0)
        };
        self.watch_delegation(task_id.to_string(), connection_id.clone(), run_seq);
        let state = self.state.lock().unwrap();
        let record = state.delegations.get(task_id).cloned();
        match record {
            Some(record) => Ok(task_report(&record)),
            None => Err("任务不存在".into()),
        }
    }

    /// Go `send_agent_message`: hand a message to another task in the team.
    pub fn send_agent_message(
        &self,
        caller: &Caller,
        task_id: &str,
        body: &str,
    ) -> Result<Value, String> {
        let body = body.trim();
        if body.is_empty() || body.len() > 12_000 {
            return Err("消息需要 1–12000 字节".into());
        }
        {
            let mut state = self.state.lock().unwrap();
            let Some(task) = state.delegations.get_mut(task_id) else {
                return Err("目标任务不存在或不属于当前协作".into());
            };
            if task.root != caller.root_id {
                return Err("目标任务不存在或不属于当前协作".into());
            }
            task.inbox.push(json!({
                "id": crate::backend::goformat::hex_id(),
                "from": caller.connection_id,
                "text": body,
                "at": crate::backend::goformat::timestamp(),
            }));
        }
        let _ = save(&self.root, &self.state.lock().unwrap().delegations);
        Ok(json!({ "delivered": true, "task_id": task_id }))
    }

    /// Go `check_user_feedback`: the caller's messages plus the team roster.
    pub fn check_user_feedback(&self, caller: &Caller) -> Value {
        let messages = {
            let state = self.state.lock().unwrap();
            let mut messages: Vec<Value> = Vec::new();
            if let Some(conn) = state.connections.get(&caller.connection_id) {
                let mut conn = conn.lock().unwrap();
                for item in conn.feedback.iter() {
                    messages.push(item.clone());
                }
                conn.feedback.clear();
            }
            for task in state.delegations.values() {
                if task.connection_id == caller.connection_id {
                    messages.extend(task.inbox.iter().cloned());
                }
            }
            messages
        };
        let (rows, _) = self.task_rows(caller, None);
        json!({ "messages": messages, "team_tasks": rows })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Delegation {
        Delegation {
            id: "task-1".into(),
            agent_type: "codex".into(),
            task: "do it".into(),
            status: "running".into(),
            ..Delegation::default()
        }
    }

    #[test]
    fn report_follows_the_go_shape() {
        let mut task = sample();
        task.status = "completed".into();
        task.result = "done".into();
        let row = task_report(&task);
        assert_eq!(row["task_id"], json!("task-1"));
        assert_eq!(row["status"], json!("completed"));
        assert_eq!(row["text"], json!("done"));
        assert!(row.get("message").is_none());
    }

    #[test]
    fn interrupted_and_failed_carry_their_error_codes() {
        let mut task = sample();
        task.status = "interrupted".into();
        task.error = "断了".into();
        let row = task_report(&task);
        assert_eq!(row["status"], json!("failed"));
        assert_eq!(row["error_code"], json!("interrupted"));
        assert_eq!(row["message"], json!("断了"));
    }

    #[test]
    fn registry_round_trips_through_disk() {
        let root = std::env::temp_dir().join(format!(
            "codepluse-delegations-{}",
            crate::backend::goformat::hex_id()
        ));
        std::fs::create_dir_all(root.join(".data")).unwrap();
        let mut tasks = HashMap::new();
        let mut task = sample();
        task.connection_id = "cid-1".into();
        task.conversation_id = 42;
        tasks.insert(task.id.clone(), task);
        save(&root, &tasks).unwrap();

        let mut restored = HashMap::new();
        load(&root, &mut restored);
        let record = restored.get("task-1").expect("记录已读回");
        assert_eq!(record.agent_type, "codex");
        assert_eq!(record.connection_id, "cid-1");
        assert_eq!(record.conversation_id, 42);
        let _ = std::fs::remove_dir_all(&root);
    }
}
