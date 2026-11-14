//! Port of Go's `codeg/question.go` registry half: the blocking
//! `ask_user_question` cards. The companion MCP process (which parks the tool
//! call) is still Go-served, but the card lifecycle — the snapshot slot, the
//! event pair, settling on turn end or disconnect — lives with the connection
//! layer that owns that state (see `connections.rs::release_questions_for_connection`).
//! This module keeps the shared shapes and the settle semantics' payloads so
//! both sides agree on them.

use std::collections::HashMap;

use serde_json::{json, Value};

use crate::backend::acp::wire::bool_of;

/// One blocking ask_user_question waiting on the user (Go `parkedQuestion`).
/// The answer channel goes to whoever parks the companion call; with the
/// companion half still Go-side there is no Rust receiver yet, so settling
/// still clears the card and the answer drops with the channel.
pub struct ParkedQuestion {
    pub conn_id: String,
    pub answer_tx: tokio::sync::mpsc::UnboundedSender<Value>,
}

/// Registry alias matching Go's `s.questions` map.
pub type QuestionRegistry = HashMap<String, ParkedQuestion>;

/// Go `questionToolResult` (question.go:106): the user's answer as the MCP
/// tool result the model reads back.
pub fn question_tool_result(answer: Option<&Value>) -> Value {
    match answer {
        None => declined_answer(),
        Some(answer) if answer.is_null() => declined_answer(),
        Some(answer) => json!({
            "declined": bool_of(answer.get("declined").unwrap_or(&Value::Null)),
            "answers": answer.get("answers").cloned().unwrap_or(Value::Null),
        }),
    }
}

/// Go `declinedAnswer` (question.go:113).
pub fn declined_answer() -> Value {
    json!({"declined": true, "answers": []})
}

/// Go `answerArg` (question.go:117): a malformed answer settles declined.
pub fn answer_arg(v: Option<&Value>) -> Value {
    match v.and_then(Value::as_object) {
        Some(map) => Value::Object(map.clone()),
        None => declined_answer(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tool_result_shapes_match_go() {
        assert_eq!(question_tool_result(None), json!({"declined": true, "answers": []}));
        assert_eq!(question_tool_result(Some(&Value::Null)), json!({"declined": true, "answers": []}));
        assert_eq!(
            question_tool_result(Some(&json!({"declined": false, "answers": [1]}))),
            json!({"declined": false, "answers": [1]})
        );
        assert_eq!(
            question_tool_result(Some(&json!({"declined": "x"}))),
            json!({"declined": false, "answers": Value::Null})
        );
    }

    #[test]
    fn answer_arg_falls_back_to_declined() {
        assert_eq!(answer_arg(None), declined_answer());
        assert_eq!(answer_arg(Some(&json!(3))), declined_answer());
        assert_eq!(
            answer_arg(Some(&json!({"declined": true}))),
            json!({"declined": true})
        );
    }

    #[test]
    fn registry_channels_settle_without_receiver() {
        // A parked question whose companion half is gone still settles: the
        // send fails silently (Go's buffered channel behaves the same once
        // abandoned).
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel::<Value>();
        drop(rx);
        let mut registry: QuestionRegistry = HashMap::new();
        registry.insert(
            "q1".into(),
            ParkedQuestion { conn_id: "c1".into(), answer_tx: tx },
        );
        let park = registry.remove("q1").unwrap();
        assert!(park.answer_tx.send(declined_answer()).is_err());
    }
}
