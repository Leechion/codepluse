//! Row helpers and wire transformations, ported from Go's `codeg/wire.go`,
//! `codeg/history.go` (persistedBlocks/toolStateWire/toolOutputText) and the
//! small coercions in `codeg/store.go`. HTTP responses are canonicalised by
//! the front door (`go_json`), so construction order here is irrelevant —
//! the shapes and the coercions are what must match Go.

use serde_json::{json, Value};

/// Go `text(v any) string`: non-strings become "". Takes `Option<&Value>` so
/// `row.get(key)` results feed in directly.
pub fn text(value: Option<&Value>) -> String {
    value.and_then(Value::as_str).unwrap_or("").to_string()
}

/// Go `number(v any) int`: ints pass, floats truncate toward zero, rest 0.
pub fn number(value: Option<&Value>) -> i64 {
    match value {
        Some(Value::Number(n)) => n
            .as_i64()
            .or_else(|| n.as_f64().map(|f| f.trunc() as i64))
            .unwrap_or(0),
        _ => 0,
    }
}

/// Go `boolOf(v any) bool` (question.go).
pub fn bool_of(value: &Value) -> bool {
    value.as_bool().unwrap_or(false)
}

/// Go `optStr`: trimmed string or None (missing/non-string → None).
pub fn opt_str(value: &Value) -> Option<String> {
    let s = value.as_str()?;
    Some(s.trim().to_string())
}

/// Go `nilIfEmpty(v Row) any` (split.go:48).
pub fn nil_if_empty(value: &Value) -> Value {
    match value {
        Value::Object(map) if map.is_empty() => Value::Null,
        Value::Null => Value::Null,
        other => other.clone(),
    }
}

/// Go `modeWire` (wire.go:3): modes object → snake-cased view, else null.
pub fn mode_wire(value: &Value) -> Value {
    let Some(map) = value.as_object() else {
        return Value::Null;
    };
    json!({
        "current_mode_id": map.get("currentModeId").cloned().unwrap_or(Value::Null),
        "available_modes": map.get("availableModes").cloned().unwrap_or(Value::Null),
    })
}

/// Go `configWire` (wire.go:10): flatten `configOptions[].options` groups into
/// a select row each; when no options survived, fall back to the models
/// catalog as a single `model` select.
pub fn config_wire(view: &Value) -> Vec<Value> {
    let mut out: Vec<Value> = Vec::new();
    if let Some(opts) = view.get("configOptions").and_then(Value::as_array) {
        for raw in opts {
            let Some(o) = raw.as_object() else { continue };
            let mut options: Vec<Value> = Vec::new();
            if let Some(list) = o.get("options").and_then(Value::as_array) {
                for option in list {
                    match option.get("options").and_then(Value::as_array) {
                        Some(group) => options.extend(group.iter().cloned()),
                        None => options.push(option.clone()),
                    }
                }
            }
            out.push(json!({
                "id": o.get("id").cloned().unwrap_or(Value::Null),
                "name": o.get("name").cloned().unwrap_or(Value::Null),
                "category": o.get("category").cloned().unwrap_or(Value::Null),
                "description": o.get("description").cloned().unwrap_or(Value::Null),
                "kind": {
                    "type": "select",
                    "current_value": o.get("currentValue").cloned().unwrap_or(Value::Null),
                    "options": options,
                    "groups": [],
                },
            }));
        }
    }
    if out.is_empty() {
        if let Some(models) = view.get("models").and_then(Value::as_object) {
            let mut options: Vec<Value> = Vec::new();
            if let Some(list) = models.get("availableModels").and_then(Value::as_array) {
                for raw in list {
                    if let Some(model) = raw.as_object() {
                        options.push(json!({
                            "value": model.get("modelId").cloned().unwrap_or(Value::Null),
                            "name": model.get("name").cloned().unwrap_or(Value::Null),
                        }));
                    }
                }
            }
            out.push(json!({
                "id": "model",
                "name": "Model",
                "category": "model",
                "kind": {
                    "type": "select",
                    "current_value": models.get("currentModelId").cloned().unwrap_or(Value::Null),
                    "options": options,
                    "groups": [],
                },
            }));
        }
    }
    out
}

/// Go `permissionOptions` (wire.go:46).
pub fn permission_options(value: Option<&Value>) -> Vec<Value> {
    let Some(list) = value.and_then(Value::as_array) else {
        return Vec::new();
    };
    list.iter()
        .map(|raw| {
            let o = raw.as_object().cloned().unwrap_or_default();
            json!({
                "option_id": o.get("optionId").cloned().unwrap_or(Value::Null),
                "name": o.get("name").cloned().unwrap_or(Value::Null),
                "kind": o.get("kind").cloned().unwrap_or(Value::Null),
            })
        })
        .collect()
}

/// Go `toolStateWire` (history.go:123): one row per live tool call, with
/// `raw_input` re-parsed as JSON and `raw_output` wrapped in the text shape.
pub fn tool_state_wire(tools: &[Value]) -> Vec<Value> {
    tools
        .iter()
        .map(|tool| {
            let input: Value =
                serde_json::from_str(&text(tool.get("raw_input"))).unwrap_or(Value::Null);
            let raw_output = text(tool.get("raw_output"));
            let output = if raw_output.is_empty() {
                Value::Null
            } else {
                json!({"kind": "text", "content": raw_output})
            };
            json!({
                "id": tool.get("tool_call_id").cloned().unwrap_or(Value::Null),
                "kind": tool.get("kind").cloned().unwrap_or(Value::Null),
                "label": tool.get("title").cloned().unwrap_or(Value::Null),
                "status": tool.get("status").cloned().unwrap_or(Value::Null),
                "input": input,
                "output": output,
                "content": tool.get("content").cloned().unwrap_or(Value::Null),
                "locations": tool.get("locations").cloned().unwrap_or(Value::Null),
                "meta": tool.get("meta").cloned().unwrap_or(Value::Null),
            })
        })
        .collect()
}

/// Go `persistedBlocks` (history.go:61): promote the live streaming view plus
/// tool rows into the turn blocks stored in history, keeping the original
/// stream order. Top-level blocks only (nested `parent_tool_use_id` blocks are
/// child-tool streams and are skipped); a `tool_call_ref` expands into a
/// `tool_use` + optional `tool_result` pair.
pub fn persisted_blocks(live: &Value, tools: &[Value], fallback: &str) -> Vec<Value> {
    let mut blocks: Vec<Value> = Vec::new();
    let content = live
        .get("content")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    for block in &content {
        if !text(block.get("parent_tool_use_id")).is_empty() {
            continue;
        }
        match text(block.get("kind")).as_str() {
            "text" | "thinking" => {
                blocks.push(json!({
                    "type": text(block.get("kind")),
                    "text": text(block.get("text")),
                }));
            }
            "tool_call_ref" => {
                let id = text(block.get("tool_call_id"));
                for tool in tools {
                    if text(tool.get("tool_call_id")) != id {
                        continue;
                    }
                    blocks.push(json!({
                        "type": "tool_use",
                        "tool_use_id": tool.get("tool_call_id").cloned().unwrap_or(Value::Null),
                        "tool_name": tool.get("title").cloned().unwrap_or(Value::Null),
                        "input_preview": tool.get("raw_input").cloned().unwrap_or(Value::Null),
                        "status": tool.get("status").cloned().unwrap_or(Value::Null),
                        "meta": tool.get("meta").cloned().unwrap_or(Value::Null),
                    }));
                    let output = match tool.get("raw_output") {
                        Some(v) if !v.is_null() => Some(v.clone()),
                        _ => match tool.get("content") {
                            Some(v) if !v.is_null() => Some(v.clone()),
                            _ => None,
                        },
                    };
                    if let Some(output) = output {
                        blocks.push(json!({
                            "type": "tool_result",
                            "tool_use_id": tool.get("tool_call_id").cloned().unwrap_or(Value::Null),
                            "output_preview": tool_output_text(&text(Some(&output))),
                            "is_error": text(tool.get("status")) == "failed",
                        }));
                    }
                }
            }
            _ => {}
        }
    }
    if blocks.is_empty() {
        blocks.push(json!({"type": "text", "text": fallback}));
    }
    blocks
}

/// Go `toolOutputText` (history.go:93): ACP adapters can wrap MCP text in
/// content arrays; history stores the shown text instead of escaped frames.
/// Non-JSON input passes through untouched.
pub fn tool_output_text(raw: &str) -> String {
    let Ok(value) = serde_json::from_str::<Value>(raw) else {
        return raw.to_string();
    };
    let mut texts: Vec<String> = Vec::new();
    extract_texts(&value, &mut texts);
    if texts.is_empty() {
        raw.to_string()
    } else {
        texts.join("\n")
    }
}

fn extract_texts(value: &Value, out: &mut Vec<String>) {
    match value {
        Value::Array(items) => {
            for item in items {
                extract_texts(item, out);
            }
        }
        Value::Object(map) => {
            if let Some(t) = map.get("text").and_then(Value::as_str) {
                out.push(t.to_string());
            } else if let Some(nested) = map.get("content") {
                if !nested.is_null() {
                    extract_texts(nested, out);
                }
            }
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn text_and_number_coercions_match_go() {
        assert_eq!(text(Some(&json!("x"))), "x");
        assert_eq!(text(Some(&json!(12))), "");
        assert_eq!(text(None), "");
        assert_eq!(number(Some(&json!(7))), 7);
        assert_eq!(number(Some(&json!(7.9))), 7);
        assert_eq!(number(Some(&json!(-7.9))), -7);
        assert_eq!(number(Some(&json!("7"))), 0);
        assert_eq!(number(None), 0);
        assert!(bool_of(&json!(true)));
        assert!(!bool_of(&json!("true")));
        assert_eq!(opt_str(&json!("  a ")), Some("a".into()));
        assert_eq!(opt_str(&json!(3)), None);
    }

    #[test]
    fn nil_if_empty_matches_go() {
        assert_eq!(nil_if_empty(&json!({})), Value::Null);
        assert_eq!(nil_if_empty(&Value::Null), Value::Null);
        assert_eq!(nil_if_empty(&json!({"a": 1})), json!({"a": 1}));
        assert_eq!(nil_if_empty(&json!([1])), json!([1]));
    }

    #[test]
    fn mode_wire_maps_and_rejects() {
        let modes = json!({"currentModeId": "plan", "availableModes": [{"id": "plan"}]});
        assert_eq!(
            mode_wire(&modes),
            json!({"current_mode_id": "plan", "available_modes": [{"id": "plan"}]})
        );
        assert_eq!(mode_wire(&json!("plan")), Value::Null);
        assert_eq!(mode_wire(&Value::Null), Value::Null);
    }

    #[test]
    fn config_wire_flattens_groups_then_falls_back_to_models() {
        let view = json!({
            "configOptions": [
                {"id": "model", "name": "Model", "category": "model", "currentValue": "m1",
                 "options": [
                    {"optionId": "a", "options": [{"value": "m1", "name": "M1"}]},
                    {"value": "m2", "name": "M2"}
                 ]},
                {"bad": true}
            ]
        });
        let out = config_wire(&view);
        assert_eq!(
            out.len(),
            2,
            "non-map guard only; a junk map still emits a null row"
        );
        assert_eq!(out[0]["id"], "model");
        // group flattened first, then the plain option
        assert_eq!(
            out[0]["kind"]["options"],
            json!([{"value": "m1", "name": "M1"}, {"value": "m2", "name": "M2"}])
        );
        assert_eq!(out[0]["kind"]["groups"], json!([]));
        assert_eq!(out[1]["id"], Value::Null);
        assert_eq!(out[1]["kind"]["options"], json!([]));

        let fallback = json!({
            "models": {"currentModelId": "m2",
                       "availableModels": [{"modelId": "m1", "name": "M1"}, {"modelId": "m2", "name": "M2"}]}
        });
        let out = config_wire(&fallback);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0]["id"], "model");
        assert_eq!(out[0]["kind"]["current_value"], "m2");
        assert_eq!(out[0]["kind"]["options"][0]["value"], "m1");
        assert!(config_wire(&json!({})).is_empty());
    }

    #[test]
    fn permission_options_maps_camel_case() {
        let out = permission_options(Some(&json!([
            {"optionId": "yes", "name": "允许", "kind": "allow_once"},
            "junk"
        ])));
        assert_eq!(out.len(), 2);
        assert_eq!(
            out[0],
            json!({"option_id": "yes", "name": "允许", "kind": "allow_once"})
        );
        assert_eq!(
            out[1],
            json!({"option_id": null, "name": null, "kind": null})
        );
        assert!(permission_options(None).is_empty());
    }

    #[test]
    fn tool_state_wire_parses_input_and_wraps_output() {
        let tools = vec![json!({
            "tool_call_id": "t1", "kind": "read", "title": "读文件", "status": "completed",
            "raw_input": "{\"path\":\"a.txt\"}", "raw_output": "正文",
            "content": Value::Null, "locations": [], "meta": {"x": 1}
        })];
        let out = tool_state_wire(&tools);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0]["id"], "t1");
        assert_eq!(out[0]["input"], json!({"path": "a.txt"}));
        assert_eq!(out[0]["output"], json!({"kind": "text", "content": "正文"}));
        assert_eq!(out[0]["meta"], json!({"x": 1}));

        let empty = tool_state_wire(&[json!({"tool_call_id": "t2"})]);
        assert_eq!(empty[0]["input"], Value::Null);
        assert_eq!(empty[0]["output"], Value::Null);
    }

    #[test]
    fn persisted_blocks_keeps_stream_order_and_expands_tool_refs() {
        let live = json!({
            "content": [
                {"kind": "text", "text": "开头", "parent_tool_use_id": ""},
                {"kind": "thinking", "text": "想想", "parent_tool_use_id": ""},
                {"kind": "tool_call_ref", "tool_call_id": "t1", "parent_tool_use_id": ""},
                {"kind": "text", "text": "子工具流", "parent_tool_use_id": "t1"}
            ]
        });
        let tools = vec![json!({
            "tool_call_id": "t1", "title": "bash", "raw_input": "{\"cmd\":\"ls\"}",
            "status": "completed", "raw_output": "文件\n", "meta": Value::Null
        })];
        let blocks = persisted_blocks(&live, &tools, "兜底");
        assert_eq!(
            blocks.len(),
            4,
            "child blocks skipped, tool ref expands to 2"
        );
        assert_eq!(blocks[0], json!({"type": "text", "text": "开头"}));
        assert_eq!(blocks[1], json!({"type": "thinking", "text": "想想"}));
        assert_eq!(blocks[2]["type"], "tool_use");
        assert_eq!(blocks[2]["tool_use_id"], "t1");
        // Go passes raw_input through as the stored string, without re-parsing.
        assert_eq!(blocks[2]["input_preview"], json!(r#"{"cmd":"ls"}"#));
        assert_eq!(
            blocks[3],
            json!({
                "type": "tool_result", "tool_use_id": "t1",
                "output_preview": "文件\n", "is_error": false
            })
        );

        let bare = persisted_blocks(&json!({"content": []}), &[], "兜底");
        assert_eq!(bare, vec![json!({"type": "text", "text": "兜底"})]);
    }

    #[test]
    fn tool_output_text_unwraps_content_arrays() {
        assert_eq!(tool_output_text("纯文本"), "纯文本");
        assert_eq!(
            tool_output_text(r#"{"content":[{"type":"text","text":"a"},{"text":"b"}]}"#),
            "a\nb"
        );
        assert_eq!(
            tool_output_text(r#"{"content":{"content":{"text":"deep"}}}"#),
            "deep"
        );
        assert_eq!(tool_output_text(r#"{"text":"only"}"#), "only");
        // JSON without text anywhere passes through
        assert_eq!(tool_output_text(r#"{"other":1}"#), r#"{"other":1}"#);
        assert_eq!(tool_output_text("[1,2]"), "[1,2]");
    }
}
