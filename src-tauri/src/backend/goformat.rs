//! Byte-compatible serializers for data Go also writes.
//!
//! The migration runs two backends against one `codeg-workspace.json`. Go
//! saves the whole document with `json.Marshal`, so Rust must reproduce that
//! exact encoding or every dual-run file comparison drifts:
//!
//! - compact output, no trailing newline;
//! - top level follows the Go `Store` struct's field order (see
//!   `server-go/internal/codeg/store.go`), while every nested object is a Go
//!   `map[string]any` and therefore comes out with byte-sorted keys;
//! - `<`, `>`, `&` (and U+2028/U+2029) are HTML-escaped inside strings, which
//!   Go's encoder does by default and serde_json never does;
//! - whole-valued floats collapse to integers: Go unmarshals every JSON number
//!   into `float64` and `json.Marshal(float64(320))` prints `320`, while
//!   serde_json would print `320.0`;
//! - `timestamp()` is Go's `time.RFC3339Nano`: UTC, sub-second digits with
//!   trailing zeros trimmed, no digits at all on whole seconds.

use serde_json::{Map, Value};
use serde_json::ser::Formatter;
use std::io;
use time::OffsetDateTime;
use time::macros::format_description;

const SECONDS: &[time::format_description::FormatItem<'static>] =
    format_description!("[year]-[month]-[day]T[hour]:[minute]:[second]");

/// Go `timestamp()`: `time.Now().UTC().Format(time.RFC3339Nano)`.
pub fn timestamp() -> String {
    let now = OffsetDateTime::now_utc();
    let base = now.format(&SECONDS).unwrap_or_default();
    let nanos = now.nanosecond();
    if nanos == 0 {
        return format!("{base}Z");
    }
    let frac = format!("{nanos:09}");
    let frac = frac.trim_end_matches('0');
    format!("{base}.{frac}Z")
}

/// Go's `number()` coercion for one value: ints pass, floats truncate toward
/// zero (clamped to int range), everything else is 0.
fn number_of(value: &Value) -> i64 {
    match value {
        Value::Number(n) => n
            .as_i64()
            .or_else(|| n.as_f64().map(|f| f.trunc() as i64))
            .unwrap_or(0),
        _ => 0,
    }
}

/// Go `id()`: 16 random bytes as hex (32 characters).
pub fn hex_id() -> String {
    let a = rand::random::<u64>();
    let b = rand::random::<u64>();
    format!("{a:016x}{b:016x}")
}

/// The `Store` struct's field order. `turns` sits where the Go field is
/// declared (after `next`) and is only written when the legacy document still
/// carries it; `customAgents` is `omitempty`, so an empty or missing map is
/// omitted. New Store fields must be added here in declaration order.
const STORE_FIELDS: &[&str] = &[
    "next",
    "turns",
    "folders",
    "conversations",
    "settings",
    "tabs",
    "tabsVersion",
    "canvasBoards",
    "canvasNodes",
    "canvasRevision",
    "folderGroups",
    "conversationTags",
    "assistants",
    "quickMessages",
    "agents",
    "customAgents",
];

/// Fields Go always writes even when empty (no `omitempty` tag), with the
/// zero value `newStore` starts from. A function, because `Value::new` is not
/// const-callable.
fn store_defaults() -> Vec<(&'static str, Value)> {
    vec![
        ("folders", Value::Array(Vec::new())),
        ("conversations", Value::Array(Vec::new())),
        ("settings", Value::Object(Map::new())),
        ("tabs", Value::Array(Vec::new())),
        ("tabsVersion", Value::from(0)),
        ("canvasBoards", Value::Null),
        ("canvasNodes", Value::Null),
        ("canvasRevision", Value::from(0)),
        ("folderGroups", Value::Array(Vec::new())),
        ("conversationTags", Value::Array(Vec::new())),
        ("assistants", Value::Array(Vec::new())),
        ("quickMessages", Value::Array(Vec::new())),
        ("agents", Value::Object(Map::new())),
    ]
}

/// Normalizes a loaded document into the shape Go would marshal, then encodes
/// it byte-identically to `json.Marshal(store)`.
pub fn marshal_store(doc: &Value) -> Vec<u8> {
    let mut out = Map::new();
    // `next` defaults to 1 in newStore; everything else to the zero values
    // above. Unknown top-level keys are dropped — Go's struct drops them too.
    let next = doc
        .get("next")
        .map(number_of)
        .unwrap_or(1);
    out.insert("next".into(), Value::from(next));
    for field in STORE_FIELDS {
        if *field == "next" {
            continue;
        }
        match doc.get(*field) {
            Some(value) => {
                if *field == "customAgents" {
                    // omitempty, and the inner structs keep Go's declared field
                    // order rather than sorted keys — pass through untouched.
                    let empty = value.as_object().map(|m| m.is_empty()).unwrap_or(true);
                    if !empty {
                        out.insert((*field).into(), value.clone());
                    }
                    continue;
                }
                if *field == "turns" {
                    let empty = value.as_object().map(|m| m.is_empty()).unwrap_or(true);
                    if empty {
                        continue;
                    }
                }
                out.insert((*field).into(), canon(value));
            }
            None => {
                if *field == "turns" || *field == "customAgents" {
                    continue;
                }
                if let Some((_, default)) = store_defaults().into_iter().find(|(k, _)| k == field) {
                    out.insert((*field).into(), default);
                }
            }
        }
    }
    // Every field was canon'd individually above; customAgents must stay as
    // loaded, so the final pass must not canonicalise the tree again.
    marshal_preserve(&Value::Object(out))
}

/// Recursively sorts object keys and collapses whole floats, mirroring what Go
/// does when it unmarshals into `any` and marshals back.
pub fn canon(value: &Value) -> Value {
    match value {
        Value::Object(map) => {
            let mut sorted = std::collections::BTreeMap::new();
            for (key, item) in map {
                sorted.insert(key.clone(), canon(item));
            }
            let mut out = Map::new();
            for (key, item) in sorted {
                out.insert(key, item);
            }
            Value::Object(out)
        }
        Value::Array(list) => Value::Array(list.iter().map(canon).collect()),
        Value::Number(n) => {
            // Go holds every number as float64; a whole float marshals without
            // a decimal point.
            if let Some(f) = n.as_f64() {
                if f.fract() == 0.0 && f.is_finite() && f.abs() < 9.007_199_254_740_992e15 {
                    if f >= 0.0 {
                        return Value::from(f as u64);
                    }
                    return Value::from(f as i64);
                }
            }
            value.clone()
        }
        _ => value.clone(),
    }
}

/// Escapes the characters Go's encoder always escapes inside strings
/// (HTML mode plus line separators). Everything else passes through as UTF-8.
fn write_escaped<W>(writer: &mut W, fragment: &str) -> io::Result<()>
where
    W: ?Sized + io::Write,
{
    let mut written = 0;
    for (index, ch) in fragment.char_indices() {
        let escaped: &[u8] = match ch {
            '<' => b"\\u003c",
            '>' => b"\\u003e",
            '&' => b"\\u0026",
            '\u{2028}' => b"\\u2028",
            '\u{2029}' => b"\\u2029",
            _ => continue,
        };
        writer.write_all(fragment[written..index].as_bytes())?;
        writer.write_all(escaped)?;
        written = index + ch.len_utf8();
    }
    writer.write_all(fragment[written..].as_bytes())
}

/// Compact encoding with Go's HTML escaping. Only the string-fragment hook
/// differs from serde_json's default; structure escapes are identical.
struct GoFormatter;

impl Formatter for GoFormatter {
    fn write_string_fragment<W>(&mut self, writer: &mut W, fragment: &str) -> io::Result<()>
    where
        W: ?Sized + io::Write,
    {
        write_escaped(writer, fragment)
    }

    /// Go routes every control byte below 0x20 through `\u00xx` except the
    /// `\n` `\r` `\t` short forms; serde_json additionally uses short forms
    /// for backspace and form feed, which would put different bytes on disk.
    fn write_char_escape<W>(
        &mut self,
        writer: &mut W,
        char_escape: serde_json::ser::CharEscape,
    ) -> io::Result<()>
    where
        W: ?Sized + io::Write,
    {
        use serde_json::ser::CharEscape;
        match char_escape {
            CharEscape::Backspace => writer.write_all(b"\\u0008"),
            CharEscape::FormFeed => writer.write_all(b"\\u000c"),
            CharEscape::Tab => writer.write_all(b"\\t"),
            CharEscape::LineFeed => writer.write_all(b"\\n"),
            CharEscape::CarriageReturn => writer.write_all(b"\\r"),
            CharEscape::Quote => writer.write_all(b"\\\""),
            CharEscape::ReverseSolidus => writer.write_all(b"\\\\"),
            CharEscape::Solidus => writer.write_all(b"\\/"),
            CharEscape::AsciiControl(byte) => {
                const HEX: &[u8; 16] = b"0123456789abcdef";
                let out = [
                    b'\\',
                    b'u',
                    b'0',
                    b'0',
                    HEX[(byte >> 4) as usize],
                    HEX[(byte & 0x0f) as usize],
                ];
                writer.write_all(&out)
            }
        }
    }
}

/// Same encoding without key canonicalisation — for stores whose rows are Go
/// structs (e.g. folder commands), where `json.Marshal` emits the declared
/// field order, not sorted map keys. The workspace writer canonicalises each
/// field itself and calls this at the end.
pub fn marshal_preserve(value: &Value) -> Vec<u8> {
    let mut buffer = Vec::new();
    let mut serializer = serde_json::Serializer::with_formatter(&mut buffer, GoFormatter);
    serde::Serialize::serialize(value, &mut serializer).expect("序列化不可能失败");
    buffer
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn timestamps_match_go_rfc3339nano() {
        let stamp = timestamp();
        // Whole-second nanos are possible but rare; assert the general shape
        // instead: date, time, optional trimmed fraction, Z suffix.
        assert!(stamp.len() >= 20, "{stamp}");
        assert!(stamp.ends_with('Z'), "{stamp}");
        assert!(stamp.contains('T'), "{stamp}");
        if let Some(dot) = stamp.find('.') {
            let frac = &stamp[dot + 1..stamp.len() - 1];
            assert!(!frac.is_empty(), "{stamp}");
            assert!(!frac.ends_with('0'), "尾零必须去掉：{stamp}");
        }
    }

    #[test]
    fn hex_ids_are_32_lowercase_hex_characters() {
        let a = hex_id();
        let b = hex_id();
        assert_eq!(a.len(), 32);
        assert_ne!(a, b);
        assert!(a.chars().all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()));
    }

    #[test]
    fn store_document_follows_go_struct_order_and_drops_unknowns() {
        let doc = json!({
            "unknown_key": 1,
            "customAgents": {},
            "quickMessages": [],
            "assistants": [{"b": 1, "a": 2}],
            "conversationTags": [],
            "folderGroups": [],
            "canvasRevision": 3,
            "canvasNodes": null,
            "canvasBoards": [],
            "tabsVersion": 2,
            "tabs": [],
            "settings": {"z": 1, "a": {"y": 2, "x": 3}},
            "conversations": [],
            "folders": [],
            "next": 7
        });
        let text = String::from_utf8(marshal_store(&doc)).unwrap();
        let expected_order = [
            "\"next\":7",
            "\"folders\":[]",
            "\"conversations\":[]",
            "\"settings\":{\"a\":{\"x\":3,\"y\":2},\"z\":1}",
            "\"tabs\":[],\"tabsVersion\":2",
            "\"canvasBoards\":[],\"canvasNodes\":null,\"canvasRevision\":3",
            "\"folderGroups\":[],\"conversationTags\":[]",
            "\"assistants\":[{\"a\":2,\"b\":1}]",
            "\"quickMessages\":[]",
            "\"agents\":{}",
        ];
        let mut cursor = 0usize;
        for piece in expected_order {
            let at = text[cursor..].find(piece).unwrap_or_else(|| panic!("{piece} 不在 {text}"));
            assert_eq!(cursor + at, text.find(piece).unwrap(), "字段顺序错乱：{text}");
            cursor = cursor + at + piece.len();
        }
        assert!(!text.contains("unknown_key"), "未知键必须丢弃：{text}");
        assert!(!text.contains("customAgents"), "空 customAgents 必须 omitempty：{text}");
    }

    #[test]
    fn legacy_turns_and_populated_custom_agents_are_kept() {
        let doc = json!({
            "next": 1,
            "turns": {"3": [{"z": 1, "a": 2}]},
            "customAgents": {"my-agent": {"name": "x", "registryId": "r"}},
            "folders": []
        });
        let text = String::from_utf8(marshal_store(&doc)).unwrap();
        assert!(text.contains(r#""turns":{"3":[{"a":2,"z":1}]}"#), "{text}");
        // customAgents passes through untouched: Go wrote the inner structs in
        // declared field order, not sorted order.
        assert!(
            text.contains(r#""customAgents":{"my-agent":{"name":"x","registryId":"r"}}"#),
            "{text}"
        );
    }

    #[test]
    fn whole_floats_collapse_and_html_escapes() {
        let value = json!({"t": "a<b>&c\u{2028}", "f": 320.0, "g": 1.5});
        let text = String::from_utf8(marshal_preserve(&canon(&value))).unwrap();
        assert_eq!(
            text,
            r#"{"f":320,"g":1.5,"t":"a\u003cb\u003e\u0026c\u2028"}"#
        );
    }

    #[test]
    fn marshal_store_matches_go_escaping_in_rows() {
        let doc = json!({"next": 2, "folders": [{"id": 1, "name": "<a&b>"}]});
        let text = String::from_utf8(marshal_store(&doc)).unwrap();
        assert!(text.contains(r#""name":"\u003ca\u0026b\u003e""#), "{text}");
    }

    /// Go writes `\u0008`/`\u000c` where serde_json's default would emit the
    /// `\b`/`\f` short forms — the exact drift this module exists to prevent.
    #[test]
    fn control_bytes_match_go_escaping() {
        let value = json!({"c": "a\u{8}b\u{c}c\u{9}d\u{a}e"});
        let text = String::from_utf8(marshal_preserve(&value)).unwrap();
        assert_eq!(text, r#"{"c":"a\u0008b\u000cc\td\ne"}"#);
    }
}
