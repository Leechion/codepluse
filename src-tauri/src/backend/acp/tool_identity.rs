//! Port of Go's `codeg/tool_identity.go` and the `ToolDefinitions` table in
//! `codeg/companion.go`: which MCP tool names belong to the workbench's own
//! collaboration server, and whether a runtime session currently has a caller
//! that may auto-allow them.
//!
//! Port of `acp/delegation/mod.rs::is_codeg_mcp_tool` upstream: namespace and
//! tool boundaries are significant; near matches must keep their permission
//! card.

use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;

use serde_json::{json, Value};

use crate::backend::acp::wire::text;

/// Go `ToolDefinitions` (companion.go:15): the codeg-mcp tool table, used by
/// the companion MCP server (still Go-served) and by the internal-tool allow
/// list here.
pub fn tool_definitions() -> Vec<Value> {
    let property = |kind: &str, description: &str| {
        json!({"type": kind, "description": description})
    };
    let tool = |name: &str, description: &str, required: Value, props: Value| {
        json!({"name": name, "description": description,
               "inputSchema": {"type": "object", "required": required, "properties": props}})
    };
    vec![
        tool(
            "list_agents",
            "列出可委派的 Harness 配置；每个 agent_type 对应已保存的供应商与 default_model。同一个 Harness 可有多个模型配置，请按任务选择具体 agent_type。",
            json!([]),
            json!({}),
        ),
        tool(
            "resume_delegation",
            "在原子对话和原生会话中继续已有任务，适用于中断、失败、取消或追加工作。先用 check_user_feedback 获取原 task_id；不要重新委派同一任务。",
            json!(["task_id"]),
            json!({"task_id": property("string", "已有任务 ID"), "task": property("string", "可选，继续任务的补充要求")}),
        ),
        tool(
            "delegate_to_agent",
            "异步委派完整、自包含的子任务，立即返回 task_id。子代理没有你的对话上下文。可多次调用并行分工，再查询状态收集结果。用户点名执行端时使用指定目标。",
            json!(["agent_type", "task"]),
            json!({"agent_type": property("string", "list_agents 返回的 agent_type"), "task": property("string", "完整任务、相关路径、约束、预期结果"), "working_dir": property("string", "可选工作目录，默认继承父会话"), "model": property("string", "可选，目标 Harness 支持的模型 ID"), "task_id": property("string", "可选，指定已有任务以复用原子对话"), "new_task": property("boolean", "明确创建不同的新任务；继续中断任务时不要设置")}),
        ),
        tool(
            "get_delegation_status",
            "收集一个或多个子任务的真实结果。省略 wait_ms 为即时快照，正数最多等待 60000ms，0 等待到完成或需要用户授权。任务耗时正常时继续等待，不要假称完成。",
            json!(["task_ids"]),
            json!({"task_ids": {"type": "array", "items": {"type": "string"}, "minItems": 1}, "wait_ms": {"type": "integer", "minimum": 0}}),
        ),
        tool(
            "cancel_delegation",
            "仅当不再需要结果时停止子任务；已经结束的任务返回原结果。",
            json!(["task_id"]),
            json!({"task_id": property("string", "delegate_to_agent 返回的 task_id")}),
        ),
        tool(
            "check_user_feedback",
            "在关键决策和阶段切换时读取新消息及本团队任务分工，及时响应用户和同僚。",
            json!([]),
            json!({}),
        ),
        tool(
            "fetch_url",
            "抓取网页并返回正文文本。当 harness 自带的网页工具失败时（例如 Claude Code 的 WebFetch 报 “Unable to verify if domain … is safe to fetch”，即域名安全校验服务不可达），改用本工具抓取同一地址：它直接走本机网络，不依赖该校验。",
            json!(["url"]),
            json!({"url": property("string", "http 或 https 地址"), "max_chars": property("integer", "可选，返回文本的最大字符数，默认 200000")}),
        ),
        tool(
            "propose_task_split",
            "把整体任务拆成 1–8 个子任务交给用户确认：返回 split_id 并在界面展示卡片，用户确认后由系统并行派发，无需你逐条调用 delegate_to_agent。每个子任务可指定 agent_type（list_agents 中的 agent_type；留空=自动选择）与 model（留空=该执行端钉住的默认模型）。调用后立即返回：卡片已在用户界面展示，用户确认后由系统自动派发全部子任务。本轮到此结束即可，不要结束前自行执行这些子任务，也禁止再调用 delegate_to_agent 重复派发；若用户随后发来新消息，先查看上方任务状态，切勿重复提案。无需用户挑选目标的单独立项仍用 delegate_to_agent。",
            json!(["tasks"]),
            json!({"tasks": {"type": "array", "minItems": 1, "maxItems": 8, "items": {"type": "object", "required": ["prompt"], "properties": {"title": property("string", "一句话任务名，用于卡片展示"), "prompt": property("string", "完整、自包含的子任务指令（子代理没有你的上下文）"), "agent_type": property("string", "list_agents 中的 agent_type；留空由系统自动选择"), "model": property("string", "可选，目标 Harness 支持的模型 ID；留空使用钉住的默认模型")}}}}),
        ),
        tool(
            "send_agent_message",
            "向同一团队的子任务发送消息；对方通过 check_user_feedback 收取。发送确认只代表入箱，不代表对方已经读取。",
            json!(["task_id", "message"]),
            json!({"task_id": property("string", "同一团队的目标任务 ID"), "message": property("string", "消息正文")}),
        ),
        tool(
            "ask_user_question",
            "向用户提出一个或多个选择题并阻塞等待作答，返回用户的选择。用于需要用户决策、澄清或确认方向时；每题给出若干选项（可单选或多选），用户也可自由作答或跳过。调用后会暂停本轮，直到用户应答。",
            json!(["questions"]),
            json!({"questions": {
                "type": "array", "minItems": 1,
                "items": {
                    "type": "object",
                    "required": ["id", "question", "options"],
                    "properties": {
                        "id": property("string", "问题唯一 ID"),
                        "question": property("string", "问题正文"),
                        "header": property("string", "简短标签"),
                        "multi_select": property("boolean", "是否允许多选"),
                        "options": {"type": "array", "minItems": 1, "items": {"type": "object", "properties": {
                            "label": property("string", "选项文本"),
                            "description": property("string", "选项说明"),
                        }}},
                    },
                },
            }}),
        ),
    ]
}

/// Go `internalToolName` (tool_identity.go:8): resolve the codeg-mcp tool
/// behind a permission-card title, honouring namespace separators.
pub fn internal_tool_name(name: &str) -> String {
    let mut name = name.trim().to_lowercase();
    for sep in ["__", "/", ":", "."] {
        if name.starts_with(&format!("mcp{sep}")) {
            name = name[3 + sep.len()..].to_string();
            break;
        }
    }
    let mut at: Option<usize> = None;
    let mut boundary = "";
    for sep in ["__", "/", ":", "."] {
        if let Some(index) = name.find(sep) {
            if at.is_none() || index < at.unwrap() {
                at = Some(index);
                boundary = sep;
            }
        }
    }
    let Some(at) = at else { return String::new() };
    let server = name[..at].trim().to_string();
    let tool = name[at + boundary.len()..].trim().to_string();
    if server != "codeg-mcp" && server != "codeg_mcp" {
        return String::new();
    }
    for definition in tool_definitions() {
        if definition.get("name").and_then(Value::as_str) == Some(tool.as_str()) {
            return tool;
        }
    }
    String::new()
}

/// Caller bookkeeping for the connection layer (Go `Caller`, delegation.go).
#[derive(Clone, Debug, Default)]
pub struct Caller {
    pub token: String,
    pub connection_id: String,
    pub root_id: String,
    pub depth: i64,
}

/// Go `allowInternalTool` (tool_identity.go:38): the tool must be one of the
/// workbench's own and the session must have a live caller.
pub fn allow_internal_tool(
    callers: &HashMap<String, Caller>,
    connections_runtime_ids: &[(String, String)], // (connection_id, runtime_id)
    runtime_id: &str,
    tool_call: &Value,
) -> bool {
    if internal_tool_name(&text(tool_call.get("title"))) == "" {
        return false;
    }
    connections_runtime_ids
        .iter()
        .any(|(cid, rid)| rid == runtime_id && callers.contains_key(cid))
}

/// The runtime hook path: the layer state owns the caller registry and the
/// connection→runtime mapping (Go closure over `s.callers`/`s.connections`).
pub fn allow_internal_tool_layer(
    state: &crate::backend::acp::connections::LayerState,
    runtime_id: &str,
    tool_call: &Value,
) -> bool {
    if internal_tool_name(&text(tool_call.get("title"))) == "" {
        return false;
    }
    for (cid, conn) in state.connections.iter() {
        if conn.lock().unwrap().runtime_id == runtime_id && state.callers.contains_key(cid) {
            return true;
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn internal_tool_name_matches_boundaries() {
        assert_eq!(internal_tool_name("mcp__codeg-mcp__list_agents"), "list_agents");
        assert_eq!(internal_tool_name("mcp__codeg_mcp__fetch_url"), "fetch_url");
        assert_eq!(internal_tool_name("codeg-mcp/fetch_url"), "fetch_url");
        assert_eq!(internal_tool_name("codeg-mcp:fetch_url"), "fetch_url");
        assert_eq!(internal_tool_name("mcp__other__list_agents"), "", "foreign server");
        assert_eq!(internal_tool_name("mcp__codeg-mcp__not_a_tool"), "", "unknown tool");
        assert_eq!(internal_tool_name("list_agents"), "", "no namespace");
        // near matches keep their permission card
        assert_eq!(internal_tool_name("mcp__codeg-mcp__list_agents_extra"), "");
    }

    #[test]
    fn allow_internal_tool_requires_caller() {
        let mut callers = HashMap::new();
        callers.insert(
            "c1".to_string(),
            Caller { token: "t".into(), connection_id: "c1".into(), root_id: String::new(), depth: 0 },
        );
        let conns = vec![("c1".to_string(), "session_x".to_string())];
        let tool = json!({"title": "mcp__codeg-mcp__list_agents"});
        assert!(allow_internal_tool(&callers, &conns, "session_x", &tool));
        assert!(!allow_internal_tool(&callers, &conns, "session_y", &tool));
        let native = json!({"title": "Edit file"});
        assert!(!allow_internal_tool(&callers, &conns, "session_x", &native));
    }
}
