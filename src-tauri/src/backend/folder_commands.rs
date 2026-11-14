//! Per-folder shell commands, ported from `server-go/internal/codeg/folder_commands.go`.
//!
//! The store is its own file (`.data/codeg-folder-commands.json`) holding a
//! JSON array of typed rows — Go marshals the `FolderCommand` struct in
//! declared field order, so unlike the workspace document these files must NOT
//! be key-sorted on write (`goformat::marshal_preserve`).
//!
//! The Go store keeps the rows in memory and never re-reads the file, so all
//! six commands flip together: once Rust answers any of them, Go's stale
//! in-memory copy must never be consulted again (and its next save would
//! clobber Rust's rows).

use std::path::Path;

use serde_json::{json, Map, Value};

use crate::backend::goformat;
use crate::backend::store_file;

const FILE: &str = "codeg-folder-commands.json";

pub fn owns(command: &str) -> bool {
    matches!(
        command,
        "list_folder_commands"
            | "create_folder_command"
            | "update_folder_command"
            | "delete_folder_command"
            | "reorder_folder_commands"
            | "bootstrap_folder_commands_from_package_json"
    )
}

pub fn handle(root: &Path, command: &str, args: &Value) -> Option<Result<Value, String>> {
    if !owns(command) {
        return None;
    }
    Some(run(root, command, args))
}

/// One FolderCommand row in Go's declared field order.
fn row(
    id: i64,
    folder_id: i64,
    name: &str,
    command: &str,
    sort_order: i64,
    run_local: bool,
    created_at: &str,
    updated_at: &str,
) -> Value {
    let mut object = Map::new();
    object.insert("id".into(), json!(id));
    object.insert("folder_id".into(), json!(folder_id));
    object.insert("name".into(), json!(name));
    object.insert("command".into(), json!(command));
    object.insert("sort_order".into(), json!(sort_order));
    object.insert("run_local".into(), json!(run_local));
    object.insert("created_at".into(), json!(created_at));
    object.insert("updated_at".into(), json!(updated_at));
    Value::Object(object)
}

fn field_number(row: &Value, key: &str) -> i64 {
    row.get(key).and_then(|v| v.as_i64()).unwrap_or(0)
}

fn store_path(root: &Path) -> std::path::PathBuf {
    root.join(".data").join(FILE)
}

fn load(root: &Path) -> Result<Vec<Value>, String> {
    match store_file::read_json(&store_path(root))? {
        Some(Value::Array(rows)) => Ok(rows),
        Some(_) => Ok(Vec::new()),
        None => Ok(Vec::new()),
    }
}

fn save(root: &Path, rows: &[Value]) -> Result<(), String> {
    let bytes = goformat::marshal_preserve(&Value::Array(rows.to_vec()));
    store_file::write_bytes(&store_path(root), &bytes)
}

fn find<'a>(rows: &'a [Value], id: i64) -> Option<&'a Value> {
    rows.iter().find(|row| field_number(row, "id") == id)
}

/// Rows of one folder in display order (sort_order, then id).
fn for_folder(rows: &[Value], folder_id: i64) -> Vec<Value> {
    let mut out: Vec<Value> = rows
        .iter()
        .filter(|row| field_number(row, "folder_id") == folder_id)
        .cloned()
        .collect();
    out.sort_by(|a, b| {
        field_number(a, "sort_order")
            .cmp(&field_number(b, "sort_order"))
            .then(field_number(a, "id").cmp(&field_number(b, "id")))
    });
    out
}

fn next_sort_order(rows: &[Value], folder_id: i64) -> i64 {
    rows.iter()
        .filter(|row| field_number(row, "folder_id") == folder_id)
        .map(|row| field_number(row, "sort_order"))
        .max()
        .unwrap_or(-1)
        + 1
}

/// The folder row must exist in the workspace store before the command store
/// is touched (folder_commands.go `resolveFolderPath`).
fn resolve_folder_path(root: &Path, folder_id: i64) -> Result<String, String> {
    let doc = store_file::read_json(&root.join(".data").join("codeg-workspace.json"))?;
    let folder = doc
        .as_ref()
        .and_then(|doc| doc.get("folders"))
        .and_then(|v| v.as_array())
        .and_then(|rows| {
            rows.iter()
                .find(|row| row.get("id").and_then(|v| v.as_i64()) == Some(folder_id))
        });
    match folder {
        Some(folder) => Ok(folder
            .get("path")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string()),
        None => Err("项目不存在".into()),
    }
}

fn run(root: &Path, command: &str, args: &Value) -> Result<Value, String> {
    // Folder context is resolved up front, before the store lock.
    let folder_path = match command {
        "create_folder_command" | "bootstrap_folder_commands_from_package_json" => {
            Some(resolve_folder_path(root, num(args, "folderId"))?)
        }
        _ => None,
    };

    // One lock covers the whole read-modify-write, same as the workspace store.
    let _guard = store_file::lock(&store_path(root))?;
    let mut rows = load(root)?;
    match command {
        "list_folder_commands" => Ok(Value::Array(for_folder(&rows, num(args, "folderId")))),
        "create_folder_command" => {
            let folder_id = num(args, "folderId");
            let name = txt(args, "name").trim().to_string();
            if name.is_empty() {
                return Err("请填写命令名称".into());
            }
            let command_line = txt(args, "command");
            if command_line.trim().is_empty() {
                return Err("请填写命令内容".into());
            }
            let run_local = args.get("runLocal") == Some(&Value::Bool(true));
            let now = goformat::timestamp();
            let next_id = rows
                .iter()
                .map(|row| field_number(row, "id"))
                .max()
                .unwrap_or(0)
                + 1;
            let item = row(
                next_id,
                folder_id,
                &name,
                &command_line,
                next_sort_order(&rows, folder_id),
                run_local,
                &now,
                &now,
            );
            rows.push(item.clone());
            save(root, &rows)?;
            Ok(item)
        }
        "update_folder_command" => {
            let id = num(args, "id");
            let index = rows
                .iter()
                .position(|row| field_number(row, "id") == id)
                .ok_or_else(|| "命令不存在".to_string())?;
            let mut item = rows[index].clone();
            if let Some(name) = args.get("name").filter(|v| !v.is_null()) {
                let name = text(Some(name)).trim().to_string();
                if name.is_empty() {
                    return Err("请填写命令名称".into());
                }
                item["name"] = json!(name);
            }
            if let Some(command_line) = args.get("command").filter(|v| !v.is_null()) {
                if text(Some(command_line)).trim().is_empty() {
                    return Err("请填写命令内容".into());
                }
                item["command"] = json!(text(Some(command_line)));
            }
            if let Some(sort_order) = args.get("sortOrder").filter(|v| !v.is_null()) {
                item["sort_order"] = json!(number(Some(sort_order)));
            }
            if let Some(run_local) = args.get("runLocal").filter(|v| !v.is_null()) {
                item["run_local"] = json!(run_local == &Value::Bool(true));
            }
            item["updated_at"] = json!(goformat::timestamp());
            rows[index] = item.clone();
            save(root, &rows)?;
            Ok(item)
        }
        "delete_folder_command" => {
            let id = num(args, "id");
            if find(&rows, id).is_none() {
                return Err("命令不存在".into());
            }
            rows.retain(|row| field_number(row, "id") != id);
            save(root, &rows)?;
            Ok(Value::Null)
        }
        "reorder_folder_commands" => {
            let folder_id = num(args, "folderId");
            let order: Vec<i64> = ids(args.get("ids"));
            for item in rows.iter_mut() {
                if field_number(item, "folder_id") != folder_id {
                    continue;
                }
                // Go builds a map id→position, so a duplicated id ends up at
                // its LAST occurrence.
                if let Some(pos) = order.iter().rposition(|id| *id == field_number(item, "id")) {
                    item["sort_order"] = json!(pos);
                }
            }
            save(root, &rows)?;
            Ok(Value::Null)
        }
        "bootstrap_folder_commands_from_package_json" => {
            let folder_id = num(args, "folderId");
            // Idempotent: rows for this folder win over package.json.
            let existing = for_folder(&rows, folder_id);
            if !existing.is_empty() {
                return Ok(Value::Array(existing));
            }
            let created = bootstrap_from_package_json(
                &mut rows,
                folder_id,
                &folder_path.unwrap_or_default(),
            )?;
            save(root, &rows)?;
            Ok(Value::Array(created))
        }
        _ => Err(format!("未知的文件夹命令：{command}")),
    }
}

/// Each package.json "scripts" entry becomes a command; names are sorted so
/// the generated order is stable. No package.json is a normal outcome.
fn bootstrap_from_package_json(
    rows: &mut Vec<Value>,
    folder_id: i64,
    folder_path: &str,
) -> Result<Vec<Value>, String> {
    let pkg_path = Path::new(folder_path).join("package.json");
    let data = match std::fs::read(&pkg_path) {
        Ok(data) => data,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(err) => return Err(err.to_string()),
    };
    // Typed like Go's struct: a scripts value that is not a string fails the
    // parse with the same message shape.
    #[derive(serde::Deserialize)]
    struct Pkg {
        #[serde(default)]
        scripts: std::collections::HashMap<String, String>,
    }
    let pkg: Pkg =
        serde_json::from_slice(&data).map_err(|e| format!("解析 package.json 失败：{e}"))?;
    let mut names: Vec<&String> = pkg.scripts.keys().collect();
    names.sort();
    let now = goformat::timestamp();
    let mut created = Vec::new();
    for (index, name) in names.into_iter().enumerate() {
        let next_id = rows
            .iter()
            .map(|row| field_number(row, "id"))
            .max()
            .unwrap_or(0)
            + 1;
        let item = row(
            next_id,
            folder_id,
            name,
            &pkg.scripts[name],
            index as i64,
            false,
            &now,
            &now,
        );
        rows.push(item.clone());
        created.push(item);
    }
    Ok(created)
}

fn num(args: &Value, key: &str) -> i64 {
    number(args.get(key))
}

/// Go's `number()` for the typed rows here: ints pass, floats truncate.
fn number(value: Option<&Value>) -> i64 {
    match value {
        Some(Value::Number(n)) => n
            .as_i64()
            .or_else(|| n.as_f64().map(|f| f.trunc() as i64))
            .unwrap_or(0),
        _ => 0,
    }
}

fn txt(args: &Value, key: &str) -> String {
    text(args.get(key))
}

fn text(value: Option<&Value>) -> String {
    value.and_then(|v| v.as_str()).unwrap_or("").to_string()
}

fn ids(value: Option<&Value>) -> Vec<i64> {
    match value {
        Some(Value::Array(list)) => list.iter().map(number_of).collect(),
        _ => Vec::new(),
    }
}

/// Go's `number()` coercion: ints pass, floats truncate.
fn number_of(value: &Value) -> i64 {
    match value {
        Value::Number(n) => n
            .as_i64()
            .or_else(|| n.as_f64().map(|f| f.trunc() as i64))
            .unwrap_or(0),
        _ => 0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::fs;
    use std::path::PathBuf;

    fn temp_root(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "codepluse-folder-commands-{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(dir.join(".data")).unwrap();
        dir
    }

    fn seed_workspace(root: &Path, folder_id: i64, path: &str) {
        let store = json!({"next": 2, "folders": [{"id": folder_id, "path": path, "name": "a", "kind": "regular", "open": true}]});
        fs::write(
            root.join(".data").join("codeg-workspace.json"),
            serde_json::to_vec_pretty(&store).unwrap(),
        )
        .unwrap();
    }

    fn call(root: &Path, command: &str, args: Value) -> Result<Value, String> {
        handle(root, command, &args).expect("命令应被接管")
    }

    #[test]
    fn create_list_update_delete_roundtrip() {
        let root = temp_root("crud");
        seed_workspace(&root, 1, "E:/ws/a");
        assert_eq!(
            call(
                &root,
                "create_folder_command",
                json!({"folderId": 1, "name": "  "})
            ),
            Err("请填写命令名称".into())
        );
        let created = call(
            &root,
            "create_folder_command",
            json!({"folderId": 1, "name": "build", "command": "pnpm build", "runLocal": true}),
        )
        .unwrap();
        assert_eq!(created["id"], 1);
        assert_eq!(created["sort_order"], 0);
        assert_eq!(created["run_local"], true);

        // 第二条的 sort_order 排到最后。
        let second = call(
            &root,
            "create_folder_command",
            json!({"folderId": 1, "name": "test", "command": "pnpm test"}),
        )
        .unwrap();
        assert_eq!(second["sort_order"], 1);

        // 不属于该文件夹的行不混入列表。
        let mut other = Map::new();
        other.insert("id".into(), json!(99));
        other.insert("folder_id".into(), json!(2));
        other.insert("name".into(), json!("elsewhere"));
        other.insert("command".into(), json!("x"));
        other.insert("sort_order".into(), json!(0));
        other.insert("run_local".into(), json!(false));
        other.insert("created_at".into(), json!("t"));
        other.insert("updated_at".into(), json!("t"));
        let mut rows = load(&root).unwrap();
        rows.push(Value::Object(other));
        save(&root, &rows).unwrap();
        let list = call(&root, "list_folder_commands", json!({"folderId": 1})).unwrap();
        assert_eq!(list.as_array().unwrap().len(), 2);

        let updated = call(
            &root,
            "update_folder_command",
            json!({"id": 1, "name": "build all", "runLocal": false}),
        )
        .unwrap();
        assert_eq!(updated["name"], "build all");
        assert_eq!(updated["run_local"], false);
        assert_eq!(
            call(&root, "update_folder_command", json!({"id": 404})),
            Err("命令不存在".into())
        );

        // reorder：按 ids 顺序写 sort_order。
        call(
            &root,
            "reorder_folder_commands",
            json!({"folderId": 1, "ids": [2, 1]}),
        )
        .unwrap();
        let list = call(&root, "list_folder_commands", json!({"folderId": 1})).unwrap();
        assert_eq!(list[0]["id"], 2);

        assert_eq!(
            call(&root, "delete_folder_command", json!({"id": 2})),
            Ok(Value::Null)
        );
        assert_eq!(
            call(&root, "delete_folder_command", json!({"id": 2})),
            Err("命令不存在".into())
        );
    }

    #[test]
    fn bootstrap_reads_package_json_and_is_idempotent() {
        let root = temp_root("bootstrap");
        let project = root.join("proj");
        fs::create_dir_all(&project).unwrap();
        fs::write(
            project.join("package.json"),
            r#"{"scripts": {"zulu": "z", "alpha": "a", "build": "b"}}"#,
        )
        .unwrap();
        seed_workspace(&root, 1, &project.to_string_lossy());

        let created = call(
            &root,
            "bootstrap_folder_commands_from_package_json",
            json!({"folderId": 1}),
        )
        .unwrap();
        let rows = created.as_array().unwrap();
        assert_eq!(rows.len(), 3);
        assert_eq!(rows[0]["name"], "alpha", "脚本名排序生成");
        assert_eq!(rows[0]["sort_order"], 0);
        assert_eq!(rows[2]["command"], "z");

        // 已有行：直接返回，不重复创建。
        let again = call(
            &root,
            "bootstrap_folder_commands_from_package_json",
            json!({"folderId": 1}),
        )
        .unwrap();
        assert_eq!(again, created);

        // 没有 package.json：空列表而非错误。
        let empty_root = temp_root("bootstrap-empty");
        seed_workspace(&empty_root, 1, "E:/nothing");
        let empty = call(
            &empty_root,
            "bootstrap_folder_commands_from_package_json",
            json!({"folderId": 1}),
        )
        .unwrap();
        assert_eq!(empty, json!([]));

        assert_eq!(
            call(
                &root,
                "create_folder_command",
                json!({"folderId": 77, "name": "x", "command": "y"})
            ),
            Err("项目不存在".into())
        );
    }

    #[test]
    fn file_stays_in_go_struct_field_order() {
        let root = temp_root("shape");
        seed_workspace(&root, 1, "E:/ws/a");
        call(
            &root,
            "create_folder_command",
            json!({"folderId": 1, "name": "a<b>", "command": "c&d"}),
        )
        .unwrap();
        let text = String::from_utf8(fs::read(store_path(&root)).unwrap()).unwrap();
        let prefix = r#"[{"id":1,"folder_id":1,"name":"a\u003cb\u003e","command":"c\u0026d","sort_order":0,"run_local":false,"created_at":""#;
        assert!(text.starts_with(prefix), "{text}");
        assert!(text.ends_with(r#"}]"#), "{text}");
        let tail = &text[prefix.len()..text.len() - 2];
        // `<ts>","updated_at":"<ts>`：紧凑、无空格、时间戳非空。
        assert_eq!(tail.matches("\",\"updated_at\":\"").count(), 1, "{text}");
    }

    #[test]
    fn only_folder_command_names_are_owned() {
        let root = temp_root("owns");
        assert!(handle(&root, "list_conversations", &json!({})).is_none());
        assert!(handle(&root, "canvas_list_boards", &json!({})).is_none());
    }
}
