//! Filesystem links that combine sibling directories into one workspace root.
//! The registry is stored separately from the workspace document so these
//! commands do not race the conversation store's in-memory state.

use std::path::{Path, PathBuf};

use serde_json::{json, Value};

use crate::backend::store_file;

const STORE: &str = "codeg-folder-links.json";

pub fn owns(command: &str) -> bool {
    matches!(
        command,
        "list_folder_links"
            | "preview_folder_links"
            | "create_folder_links"
            | "rename_folder_link"
            | "repair_folder_link"
            | "remove_folder_link"
    )
}

pub struct Outcome {
    pub result: Value,
    pub events: Vec<(String, Value)>,
}

pub fn handle(root: &Path, command: &str, args: &Value) -> Option<Result<Outcome, String>> {
    if !owns(command) {
        return None;
    }
    Some(run(root, command, args))
}

fn number(v: Option<&Value>) -> i64 {
    v.and_then(Value::as_i64).unwrap_or(0)
}

fn text(v: Option<&Value>) -> &str {
    v.and_then(Value::as_str).unwrap_or("")
}

fn store_path(root: &Path) -> PathBuf {
    root.join(".data").join(STORE)
}

fn read_rows(root: &Path) -> Result<Vec<Value>, String> {
    match store_file::read_json(&store_path(root))? {
        Some(Value::Array(rows)) => Ok(rows),
        Some(_) => Err("文件夹链接存储格式无效".into()),
        None => Ok(Vec::new()),
    }
}

fn write_rows(root: &Path, rows: &[Value]) -> Result<(), String> {
    store_file::write_json(&store_path(root), &Value::Array(rows.to_vec()))
}

fn folder_path(root: &Path, id: i64) -> Result<PathBuf, String> {
    let doc_path = root.join(".data").join("codeg-workspace.json");
    let doc = store_file::read_json(&doc_path)?.ok_or("工作区不存在")?;
    let folder = doc
        .get("folders")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .find(|row| number(row.get("id")) == id)
        .ok_or("项目不存在")?;
    let path = PathBuf::from(text(folder.get("path")));
    let resolved = std::fs::canonicalize(&path).map_err(|e| format!("工作区目录不可用：{e}"))?;
    if !resolved.is_dir() {
        return Err("工作区路径不是目录".into());
    }
    Ok(resolved)
}

fn canonical(path: &str) -> Result<PathBuf, String> {
    let path = PathBuf::from(path);
    let resolved = std::fs::canonicalize(&path).map_err(|e| e.to_string())?;
    if !resolved.is_dir() {
        return Err("目标不是目录".into());
    }
    Ok(resolved)
}

fn key(path: &Path) -> String {
    let value = path.to_string_lossy().replace('/', "\\");
    if cfg!(windows) {
        value.to_lowercase()
    } else {
        value
    }
}

fn contains(parent: &Path, child: &Path) -> bool {
    let p = key(parent);
    let c = key(child);
    c == p || c.starts_with(&(p.trim_end_matches('\\').to_string() + "\\"))
}

fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name != "."
        && name != ".."
        && name.trim() == name
        && !name.contains(['/', '\\', ':', '\0'])
        && !name.ends_with(['.', ' '])
}

fn detail(row: &Value, root_path: &Path) -> Value {
    let name = text(row.get("name"));
    let target = PathBuf::from(text(row.get("target_path")));
    let link = root_path.join(name);
    let status = match std::fs::symlink_metadata(&link) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => "missing",
        Err(_) => "broken",
        Ok(meta) if !meta.file_type().is_symlink() => "conflicted",
        Ok(_) => match std::fs::canonicalize(&link) {
            Err(_) => "broken",
            Ok(actual) if key(&actual) == key(&target) => "ok",
            Ok(_) => "conflicted",
        },
    };
    json!({"id":row.get("id").cloned().unwrap_or(Value::Null),"folderId":row.get("folder_id").cloned().unwrap_or(Value::Null),"name":name,"targetPath":text(row.get("target_path")),"status":status})
}

fn folder_rows(root: &Path, folder_id: i64, root_path: &Path) -> Result<Vec<Value>, String> {
    let rows = read_rows(root)?;
    let mut out: Vec<Value> = rows
        .iter()
        .filter(|row| number(row.get("folder_id")) == folder_id)
        .map(|row| detail(row, root_path))
        .collect();
    out.sort_by(|a, b| a["name"].as_str().cmp(&b["name"].as_str()));
    Ok(out)
}

fn plan_one(
    root: &Path,
    folder_id: i64,
    root_path: &Path,
    raw: &str,
    requested: Option<&str>,
    reserved: &mut Vec<String>,
) -> Result<Value, String> {
    let raw_path = PathBuf::from(raw);
    let target = match std::fs::metadata(&raw_path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(
                json!({"targetPath":raw,"baseName":"","name":"","renamed":false,"collidesWithExistingEntry":false,"rejection":"not_found","existingLinkName":Value::Null}),
            );
        }
        Err(_) => {
            return Ok(
                json!({"targetPath":raw,"baseName":"","name":"","renamed":false,"collidesWithExistingEntry":false,"rejection":"not_found","existingLinkName":Value::Null}),
            );
        }
        Ok(metadata) if !metadata.is_dir() => {
            return Ok(
                json!({"targetPath":raw,"baseName":"","name":"","renamed":false,"collidesWithExistingEntry":false,"rejection":"not_a_directory","existingLinkName":Value::Null}),
            );
        }
        Ok(_) => std::fs::canonicalize(&raw_path).map_err(|e| e.to_string())?,
    };
    let base_name = target
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_default();
    let mut rejection = None;
    let mut existing_link_name = Value::Null;
    if key(&target) == key(root_path) {
        rejection = Some("same_as_root");
    } else if contains(&target, root_path) {
        rejection = Some("ancestor_of_root");
    } else if contains(root_path, &target) {
        rejection = Some("inside_root");
    }
    let rows = read_rows(root)?;
    if rejection.is_none() {
        if let Some(row) = rows.iter().find(|row| {
            number(row.get("folder_id")) == folder_id
                && key(Path::new(text(row.get("target_path")))) == key(&target)
        }) {
            rejection = Some("already_linked");
            existing_link_name = json!(text(row.get("name")));
        }
    }
    let desired = requested
        .filter(|s| !s.is_empty())
        .unwrap_or(&base_name)
        .to_string();
    if rejection.is_none() && !valid_name(&desired) {
        rejection = Some("name_unavailable");
    }
    let mut name = desired.clone();
    let mut collides = false;
    let mut renamed = false;
    if rejection.is_none() {
        let used = |candidate: &str| {
            reserved.iter().any(|n| n.eq_ignore_ascii_case(candidate))
                || root_path.join(candidate).exists()
        };
        collides = root_path.join(&name).exists();
        if used(&name) {
            if requested.is_some() {
                rejection = Some("name_unavailable");
            } else {
                let mut suffix = 2;
                while used(&format!("{base_name} ({suffix})")) {
                    suffix += 1;
                }
                name = format!("{base_name} ({suffix})");
                renamed = true;
            }
        }
        if rejection.is_none() {
            reserved.push(name.to_lowercase());
        }
    }
    Ok(
        json!({"targetPath":target.to_string_lossy(),"baseName":base_name,"name":if rejection.is_some(){""}else{&name},"renamed":renamed,"collidesWithExistingEntry":collides,"rejection":rejection,"existingLinkName":existing_link_name}),
    )
}

#[cfg(windows)]
fn create_dir_link(target: &Path, link: &Path) -> Result<(), String> {
    use std::os::windows::process::CommandExt;
    let status = std::process::Command::new("cmd.exe")
        .args(["/C", "mklink", "/J"])
        .arg(link)
        .arg(target)
        .creation_flags(0x0800_0000)
        .status()
        .map_err(|e| e.to_string())?;
    if status.success() {
        Ok(())
    } else {
        Err("创建目录联接失败".into())
    }
}

#[cfg(unix)]
fn create_dir_link(target: &Path, link: &Path) -> Result<(), String> {
    std::os::unix::fs::symlink(target, link).map_err(|e| e.to_string())
}

fn remove_dir_link(link: &Path) -> Result<(), String> {
    #[cfg(windows)]
    {
        std::fs::remove_dir(link).map_err(|e| e.to_string())
    }
    #[cfg(unix)]
    {
        std::fs::remove_file(link).map_err(|e| e.to_string())
    }
}

fn add_git_exclude(root_path: &Path, name: &str) -> Result<(), String> {
    let dot_git = root_path.join(".git");
    let git_dir = if dot_git.is_dir() {
        dot_git
    } else if dot_git.is_file() {
        let text = std::fs::read_to_string(&dot_git).map_err(|e| e.to_string())?;
        let value = text
            .trim()
            .strip_prefix("gitdir:")
            .ok_or("Git worktree 元数据无效")?
            .trim();
        let path = PathBuf::from(value);
        if path.is_absolute() {
            path
        } else {
            root_path.join(path)
        }
    } else {
        return Ok(());
    };
    let exclude = git_dir.join("info").join("exclude");
    std::fs::create_dir_all(exclude.parent().unwrap()).map_err(|e| e.to_string())?;
    let mut body = std::fs::read_to_string(&exclude).unwrap_or_default();
    let rule = format!("/{name}/");
    if !body.lines().any(|line| line.trim() == rule) {
        if !body.is_empty() && !body.ends_with('\n') {
            body.push('\n');
        }
        body.push_str(&rule);
        body.push('\n');
        std::fs::write(exclude, body).map_err(|e| e.to_string())?;
    }
    Ok(())
}

fn changed(folder_id: i64) -> Vec<(String, Value)> {
    vec![(
        "folder://links-changed".into(),
        json!({"folder_id":folder_id}),
    )]
}

fn run(root: &Path, command: &str, args: &Value) -> Result<Outcome, String> {
    let folder_id = number(args.get("folderId"));
    let root_path = folder_path(root, folder_id)?;
    match command {
        "list_folder_links" => Ok(Outcome {
            result: Value::Array(folder_rows(root, folder_id, &root_path)?),
            events: Vec::new(),
        }),
        "preview_folder_links" => {
            let mut reserved: Vec<String> = folder_rows(root, folder_id, &root_path)?
                .iter()
                .filter_map(|r| r["name"].as_str().map(str::to_lowercase))
                .collect();
            let paths = args
                .get("paths")
                .and_then(Value::as_array)
                .ok_or("路径列表无效")?;
            let items = paths
                .iter()
                .map(|p| {
                    plan_one(
                        root,
                        folder_id,
                        &root_path,
                        text(Some(p)),
                        None,
                        &mut reserved,
                    )
                })
                .collect::<Result<Vec<_>, _>>()?;
            Ok(Outcome {
                result: Value::Array(items),
                events: Vec::new(),
            })
        }
        "create_folder_links" => {
            let items = args
                .get("items")
                .and_then(Value::as_array)
                .ok_or("链接列表无效")?;
            let _guard = store_file::lock(&store_path(root))?;
            let mut reserved: Vec<String> = folder_rows(root, folder_id, &root_path)?
                .iter()
                .filter_map(|r| r["name"].as_str().map(str::to_lowercase))
                .collect();
            let mut rows = read_rows(root)?;
            let mut created = Vec::new();
            let mut created_links = Vec::new();
            for item in items {
                let path = text(item.get("path"));
                let requested = item.get("name").and_then(Value::as_str);
                let plan = plan_one(root, folder_id, &root_path, path, requested, &mut reserved)?;
                if !plan["rejection"].is_null() {
                    continue;
                }
                let name = text(plan.get("name"));
                let target = PathBuf::from(text(plan.get("targetPath")));
                let link = root_path.join(name);
                if create_dir_link(&target, &link).is_err() {
                    continue;
                }
                if args
                    .get("gitExclude")
                    .and_then(Value::as_bool)
                    .unwrap_or(true)
                {
                    if add_git_exclude(&root_path, name).is_err() {
                        let _ = remove_dir_link(&link);
                        continue;
                    }
                }
                let id = rows
                    .iter()
                    .map(|row| number(row.get("id")))
                    .max()
                    .unwrap_or(0)
                    + 1;
                let row = json!({"id":id,"folder_id":folder_id,"name":name,"target_path":target.to_string_lossy()});
                rows.push(row.clone());
                created.push(detail(&row, &root_path));
                created_links.push(link);
            }
            if !created.is_empty() {
                if let Err(error) = write_rows(root, &rows) {
                    for link in created_links {
                        let _ = remove_dir_link(&link);
                    }
                    return Err(error);
                }
            }
            let has_changes = !created.is_empty();
            Ok(Outcome {
                result: Value::Array(created),
                events: if has_changes {
                    Vec::new()
                } else {
                    changed(folder_id)
                },
            })
        }
        "rename_folder_link" => {
            let _guard = store_file::lock(&store_path(root))?;
            let link_id = number(args.get("linkId"));
            let name = text(args.get("newName"));
            if !valid_name(name) {
                return Err("链接名称无效".into());
            }
            let mut rows = read_rows(root)?;
            let index = rows
                .iter()
                .position(|r| {
                    number(r.get("id")) == link_id && number(r.get("folder_id")) == folder_id
                })
                .ok_or("文件夹链接不存在")?;
            let from = root_path.join(text(rows[index].get("name")));
            let to = root_path.join(name);
            if to.exists() {
                return Err("目标名称已被占用".into());
            }
            if std::fs::symlink_metadata(&from).is_ok() {
                std::fs::rename(&from, &to).map_err(|e| e.to_string())?;
            }
            rows[index]["name"] = json!(name);
            write_rows(root, &rows)?;
            let result = detail(&rows[index], &root_path);
            Ok(Outcome {
                result,
                events: changed(folder_id),
            })
        }
        "repair_folder_link" => {
            let _guard = store_file::lock(&store_path(root))?;
            let link_id = number(args.get("linkId"));
            let rows = read_rows(root)?;
            let row = rows
                .iter()
                .find(|r| number(r.get("id")) == link_id && number(r.get("folder_id")) == folder_id)
                .ok_or("文件夹链接不存在")?;
            let link = root_path.join(text(row.get("name")));
            if std::fs::symlink_metadata(&link).is_ok() {
                return Err("链接位置已被占用，不能修复".into());
            }
            let target = canonical(text(row.get("target_path")))?;
            create_dir_link(&target, &link)?;
            Ok(Outcome {
                result: detail(row, &root_path),
                events: changed(folder_id),
            })
        }
        "remove_folder_link" => {
            let _guard = store_file::lock(&store_path(root))?;
            let link_id = number(args.get("linkId"));
            let mut rows = read_rows(root)?;
            let index = rows
                .iter()
                .position(|r| {
                    number(r.get("id")) == link_id && number(r.get("folder_id")) == folder_id
                })
                .ok_or("文件夹链接不存在")?;
            if args
                .get("deleteLink")
                .and_then(Value::as_bool)
                .unwrap_or(true)
            {
                let link = root_path.join(text(rows[index].get("name")));
                if let Ok(meta) = std::fs::symlink_metadata(&link) {
                    if !meta.file_type().is_symlink() {
                        return Err("链接位置已被真实目录占用，拒绝删除".into());
                    }
                    remove_dir_link(&link)?;
                }
            }
            rows.remove(index);
            write_rows(root, &rows)?;
            Ok(Outcome {
                result: Value::Null,
                events: changed(folder_id),
            })
        }
        _ => Err(format!("未知文件夹链接命令：{command}")),
    }
}
