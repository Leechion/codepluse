//! Workspace file operations, ported from `server-go/internal/codeg/files.go`
//! (12 commands).
//!
//! Anything that touches `codeg-workspace.json` stays in Go, but these do not:
//! they only *read* the folder list to decide which roots are readable, then
//! work on the filesystem. Writes here are the user's own file edits, which the
//! Go side never caches, so there is nothing to clobber.
//!
//! SHA-256 is hand-rolled: the ETag contract (`save_file_content` refuses a
//! write when `expectedEtag` no longer matches) needs it, and pulling a crate
//! into the build for one hash is not worth the supply-chain surface.

use std::path::{Path, PathBuf};

use serde_json::{json, Value};

use crate::backend::git::{
    git_string, lexical_rel, readable_roots, resolve, resolve_file, within_root,
};

const MAX_FILE: u64 = 8 << 20;
const IGNORED_DIRS: [&str; 6] = ["node_modules", ".git", ".next", ".data", "dist", "target"];

pub fn owns(command: &str) -> bool {
    matches!(
        command,
        "get_home_directory"
            | "list_directory_entries"
            | "list_directory_with_files"
            | "get_file_tree"
            | "list_workspace_files"
            | "read_file_preview"
            | "read_file_for_edit"
            | "read_file_base64"
            | "read_workspace_file_base64"
            | "save_file_content"
            | "save_file_copy"
            | "create_directory"
            | "create_file_tree_entry"
            | "rename_file_tree_entry"
            | "move_file_tree_entry"
            | "delete_file_tree_entry"
            | "get_git_head"
    )
}

pub fn handle(root: &Path, command: &str, args: &Value) -> Option<Result<Value, String>> {
    if !owns(command) {
        return None;
    }
    Some(run(root, command, args))
}

fn text(args: &Value, key: &str) -> String {
    args.get(key)
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string()
}

fn number(args: &Value, key: &str) -> i64 {
    args.get(key).and_then(|v| v.as_i64()).unwrap_or(0)
}

fn disk_browse_allowed() -> bool {
    matches!(
        std::env::var("HARNESS_HUB_ALLOW_ANY_PATH").as_deref(),
        Ok("1")
    ) || matches!(
        std::env::var("HARNESS_HUB_ALLOW_ANY_DIR_BROWSE").as_deref(),
        Ok("1")
    )
}

// ── SHA-256 ────────────────────────────────────────────────────────────────

const K: [u32; 64] = [
    0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4, 0xab1c5ed5,
    0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe, 0x9bdc06a7, 0xc19bf174,
    0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f, 0x4a7484aa, 0x5cb0a9dc, 0x76f988da,
    0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7, 0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967,
    0x27b70a85, 0x2e1b2138, 0x4d2c6dfc, 0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85,
    0xa2bfe8a1, 0xa81a664b, 0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070,
    0x19a4c116, 0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
    0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7, 0xc67178f2,
];

pub(crate) fn sha256(data: &[u8]) -> [u8; 32] {
    let mut h: [u32; 8] = [
        0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab,
        0x5be0cd19,
    ];
    let mut message = data.to_vec();
    let bit_len = (data.len() as u64) * 8;
    message.push(0x80);
    while message.len() % 64 != 56 {
        message.push(0);
    }
    message.extend_from_slice(&bit_len.to_be_bytes());

    for chunk in message.chunks(64) {
        let mut w = [0u32; 64];
        for i in 0..16 {
            w[i] = u32::from_be_bytes([
                chunk[i * 4],
                chunk[i * 4 + 1],
                chunk[i * 4 + 2],
                chunk[i * 4 + 3],
            ]);
        }
        for i in 16..64 {
            let s0 = w[i - 15].rotate_right(7) ^ w[i - 15].rotate_right(18) ^ (w[i - 15] >> 3);
            let s1 = w[i - 2].rotate_right(17) ^ w[i - 2].rotate_right(19) ^ (w[i - 2] >> 10);
            w[i] = w[i - 16]
                .wrapping_add(s0)
                .wrapping_add(w[i - 7])
                .wrapping_add(s1);
        }
        let (mut a, mut b, mut c, mut d, mut e, mut f, mut g, mut hh) =
            (h[0], h[1], h[2], h[3], h[4], h[5], h[6], h[7]);
        for i in 0..64 {
            let s1 = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
            let ch = (e & f) ^ ((!e) & g);
            let temp1 = hh
                .wrapping_add(s1)
                .wrapping_add(ch)
                .wrapping_add(K[i])
                .wrapping_add(w[i]);
            let s0 = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
            let maj = (a & b) ^ (a & c) ^ (b & c);
            let temp2 = s0.wrapping_add(maj);
            hh = g;
            g = f;
            f = e;
            e = d.wrapping_add(temp1);
            d = c;
            c = b;
            b = a;
            a = temp1.wrapping_add(temp2);
        }
        h[0] = h[0].wrapping_add(a);
        h[1] = h[1].wrapping_add(b);
        h[2] = h[2].wrapping_add(c);
        h[3] = h[3].wrapping_add(d);
        h[4] = h[4].wrapping_add(e);
        h[5] = h[5].wrapping_add(f);
        h[6] = h[6].wrapping_add(g);
        h[7] = h[7].wrapping_add(hh);
    }

    let mut out = [0u8; 32];
    for (i, word) in h.iter().enumerate() {
        out[i * 4..i * 4 + 4].copy_from_slice(&word.to_be_bytes());
    }
    out
}

pub(crate) fn sha256_hex(data: &[u8]) -> String {
    use std::fmt::Write;
    let digest = sha256(data);
    let mut out = String::with_capacity(64);
    for byte in digest {
        let _ = write!(out, "{byte:02x}");
    }
    out
}

// ── helpers ────────────────────────────────────────────────────────────────

/// `e.Type()&os.ModeSymlink != 0`
fn is_symlink(entry: &std::fs::DirEntry) -> bool {
    entry.file_type().map(|t| t.is_symlink()).unwrap_or(false)
}

fn is_dir(entry: &std::fs::DirEntry) -> bool {
    entry.file_type().map(|t| t.is_dir()).unwrap_or(false)
}

/// Go's `ignoredDirs` skip: symlinks always, ignored directory names always.
fn skipped(entry: &std::fs::DirEntry) -> bool {
    if is_symlink(entry) {
        return true;
    }
    is_dir(entry) && IGNORED_DIRS.contains(&entry.file_name().to_string_lossy().as_ref())
}

fn line_ending(data: &[u8]) -> &'static str {
    let text = String::from_utf8_lossy(data);
    if !text.contains('\n') {
        return "none";
    }
    if text.contains("\r\n") {
        let without_crlf = text.replace("\r\n", "");
        if without_crlf.contains('\n') {
            return "mixed";
        }
        return "crlf";
    }
    "lf"
}

fn entries(dir: &Path) -> Result<Vec<std::fs::DirEntry>, String> {
    let read = std::fs::read_dir(dir).map_err(|e| e.to_string())?;
    let mut out = Vec::new();
    for entry in read {
        out.push(entry.map_err(|e| e.to_string())?);
    }
    Ok(out)
}

fn run(root: &Path, command: &str, args: &Value) -> Result<Value, String> {
    let root_path = text(args, "rootPath");
    let input = text(args, "path");

    // File-tree mutations are scoped to an opened workspace folder. Resolve
    // through the same symlink-aware guard as reads before touching disk.
    if matches!(
        command,
        "create_file_tree_entry"
            | "rename_file_tree_entry"
            | "move_file_tree_entry"
            | "delete_file_tree_entry"
    ) {
        return mutate_tree_entry(root, command, args);
    }
    if command == "create_directory" {
        return create_directory(root, args);
    }

    if command == "get_home_directory" {
        let resolved = resolve(root, "")?;
        return Ok(Value::String(resolved.to_string_lossy().into_owned()));
    }

    let path = if matches!(
        command,
        "list_directory_entries" | "list_directory_with_files"
    ) {
        // The folder picker may list outside the workspace only on an explicit
        // switch, mirroring `diskBrowseAllowed`.
        let mut target = input.clone();
        if target.is_empty() {
            target = root_path.clone();
        }
        if target.is_empty() {
            target = root.to_string_lossy().into_owned();
        }
        let target = if Path::new(&target).is_absolute() {
            PathBuf::from(&target)
        } else {
            root.join(&target)
        };
        let resolved = resolve(root, &target.to_string_lossy())?;
        if !disk_browse_allowed() {
            let allowed = readable_roots(root).iter().any(|base| {
                resolve(root, &base.to_string_lossy())
                    .map(|base| within_root(&base, &resolved))
                    .unwrap_or(false)
            });
            if !allowed {
                return Err(format!("目录超出工作区范围：{}", resolved.display()));
            }
        }
        resolved
    } else {
        // `rootPath` is the folder the caller is browsing; `path` is relative to it.
        let folder = if root_path.is_empty() {
            root.to_path_buf()
        } else {
            resolve(root, &root_path).unwrap_or_else(|_| root.to_path_buf())
        };
        resolve_file(root, &folder, &input)?
    };

    match command {
        "list_directory_entries" | "list_directory_with_files" => {
            let mut out = Vec::new();
            for entry in entries(&path)? {
                if is_symlink(&entry) {
                    continue;
                }
                let dir = is_dir(&entry);
                if !dir && command == "list_directory_entries" {
                    continue;
                }
                let full = path.join(entry.file_name());
                let mut children = false;
                if dir {
                    if let Ok(list) = std::fs::read_dir(&full) {
                        for child in list.flatten() {
                            if is_dir(&child) {
                                children = true;
                                break;
                            }
                        }
                    }
                }
                let mut row = serde_json::Map::new();
                row.insert("name".into(), json!(entry.file_name().to_string_lossy()));
                row.insert("path".into(), json!(full.to_string_lossy()));
                row.insert("hasChildren".into(), json!(children));
                if command == "list_directory_with_files" {
                    row.insert("isDir".into(), json!(dir));
                    if let Ok(meta) = entry.metadata() {
                        row.insert("size".into(), json!(meta.len()));
                    }
                }
                out.push(Value::Object(row));
            }
            Ok(Value::Array(out))
        }
        "get_file_tree" | "list_workspace_files" => {
            let mut max_depth = number(args, "maxDepth");
            if max_depth <= 0 {
                max_depth = 8;
            }
            if max_depth > 32 {
                max_depth = 32;
            }
            let mut flat: Vec<Value> = Vec::new();
            let nodes = walk_tree(
                &path,
                &path,
                1,
                max_depth,
                command == "list_workspace_files",
                &mut flat,
            )?;
            if command == "list_workspace_files" {
                return Ok(Value::Array(flat));
            }
            Ok(Value::Array(nodes))
        }
        "read_file_preview"
        | "read_file_for_edit"
        | "read_file_base64"
        | "read_workspace_file_base64"
        | "save_file_content"
        | "save_file_copy" => {
            let meta = std::fs::metadata(&path);
            let exists = meta.is_ok();
            if let Err(err) = &meta {
                if err.kind() != std::io::ErrorKind::NotFound {
                    return Err(err.to_string());
                }
            }
            if let Ok(meta) = &meta {
                if !meta.is_file() || meta.len() > MAX_FILE {
                    return Err("仅支持小于 8 MiB 的普通文件".into());
                }
            }
            let mut data = Vec::new();
            if exists {
                data = std::fs::read(&path).map_err(|e| e.to_string())?;
            }
            let saving = command.starts_with("save_");
            if saving {
                let expected = text(args, "expectedEtag");
                if !expected.is_empty() && expected != sha256_hex(&data) {
                    return Err("file_conflict: 文件已被其他程序修改，请重新读取后再保存".into());
                }
                if command == "save_file_copy" && exists {
                    return Err("目标文件已存在".into());
                }
                data = text(args, "content").into_bytes();
                if data.len() as u64 > MAX_FILE {
                    return Err("文件超过 8 MiB".into());
                }
                std::fs::write(&path, &data).map_err(|e| e.to_string())?;
            } else if !exists {
                // Go returns `os.ErrNotExist` verbatim.
                return Err("file does not exist".into());
            }

            if command.contains("base64") {
                use base64::Engine;
                return Ok(Value::String(
                    base64::engine::general_purpose::STANDARD.encode(&data),
                ));
            }
            if command == "read_file_preview" {
                return Ok(
                    json!({"path": path.to_string_lossy(), "content": String::from_utf8_lossy(&data)}),
                );
            }

            let meta = std::fs::metadata(&path).map_err(|e| e.to_string())?;
            let mtime_ms = meta
                .modified()
                .ok()
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|d| d.as_millis() as i64)
                .unwrap_or(0);
            #[cfg(unix)]
            let readonly = {
                use std::os::unix::fs::PermissionsExt;
                meta.permissions().mode() & 0o200 == 0
            };
            #[cfg(not(unix))]
            let readonly = meta.permissions().readonly();
            let mut out = serde_json::Map::new();
            out.insert("path".into(), json!(path.to_string_lossy()));
            out.insert("etag".into(), json!(sha256_hex(&data)));
            out.insert("mtime_ms".into(), json!(mtime_ms));
            out.insert("readonly".into(), json!(readonly));
            out.insert("line_ending".into(), json!(line_ending(&data)));
            if command == "read_file_for_edit" {
                out.insert("content".into(), json!(String::from_utf8_lossy(&data)));
            }
            Ok(Value::Object(out))
        }
        "get_git_head" => {
            let repo = path.to_string_lossy().to_string();
            match git_string(Path::new(&repo), &["rev-parse", "--abbrev-ref", "HEAD"]) {
                Ok(name) => Ok(Value::String(name.trim().to_string())),
                Err(_) => Ok(Value::Null),
            }
        }
        _ => Err(format!("文件操作未实现：{command}")),
    }
}

fn single_component(value: &str) -> bool {
    !value.is_empty()
        && value != "."
        && value != ".."
        && !value.contains('/')
        && !value.contains('\\')
        && !value.contains('\0')
}

fn relative_to_root(root: &Path, path: &Path) -> String {
    path.strip_prefix(root)
        .unwrap_or(path)
        .to_string_lossy()
        .replace('\\', "/")
}

fn create_directory(root: &Path, args: &Value) -> Result<Value, String> {
    let parent = text(args, "parentPath");
    let name = text(args, "name");
    if !single_component(&name) {
        return Err("文件夹名称无效".into());
    }
    let parent = resolve(root, &parent)?;
    if !parent.is_dir() {
        return Err("父目录不存在".into());
    }
    let target = parent.join(name);
    std::fs::create_dir(&target).map_err(|error| match error.kind() {
        std::io::ErrorKind::AlreadyExists => "目标已存在".to_string(),
        _ => error.to_string(),
    })?;
    Ok(Value::String(target.to_string_lossy().into_owned()))
}

fn mutate_tree_entry(root: &Path, command: &str, args: &Value) -> Result<Value, String> {
    let folder = text(args, "rootPath");
    let folder = if folder.is_empty() {
        root.to_path_buf()
    } else {
        resolve(root, &folder)?
    };
    let input = text(args, "path");
    let source_input = if command == "move_file_tree_entry" {
        text(args, "sourcePath")
    } else {
        input.clone()
    };
    let source = resolve_file(root, &folder, &source_input)?;
    if command != "create_file_tree_entry" && source == folder {
        return Err("不能对工作区根目录执行此操作".into());
    }
    let metadata = std::fs::symlink_metadata(&source).map_err(|error| error.to_string())?;
    if metadata.file_type().is_symlink() {
        return Err("不允许操作符号链接".into());
    }

    let target = match command {
        "create_file_tree_entry" => {
            let name = text(args, "name");
            if !single_component(&name) {
                return Err("文件或文件夹名称无效".into());
            }
            if !source.is_dir() {
                return Err("父目录不存在".into());
            }
            let target = source.join(name);
            if args.get("kind").and_then(Value::as_str) == Some("dir") {
                std::fs::create_dir(&target).map_err(|error| error.to_string())?;
            } else if args.get("kind").and_then(Value::as_str) == Some("file") {
                std::fs::OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .open(&target)
                    .map_err(|error| error.to_string())?;
            } else {
                return Err("创建类型必须是 file 或 dir".into());
            }
            target
        }
        "rename_file_tree_entry" => {
            let name = text(args, "newName");
            if !single_component(&name) {
                return Err("新名称无效".into());
            }
            let target = source.parent().ok_or("源路径无效")?.join(name);
            if target.exists() {
                return Err("目标已存在".into());
            }
            std::fs::rename(&source, &target).map_err(|error| error.to_string())?;
            target
        }
        "move_file_tree_entry" => {
            let dest = text(args, "destDir");
            let destination_dir = if dest.is_empty() {
                folder.clone()
            } else {
                resolve_file(root, &folder, &dest)?
            };
            if !destination_dir.is_dir() {
                return Err("目标目录不存在".into());
            }
            let target = destination_dir.join(source.file_name().ok_or("源路径无效")?);
            if target == source {
                return Ok(Value::String(relative_to_root(&folder, &target)));
            }
            if target.exists() {
                return Err("目标已存在".into());
            }
            if metadata.is_dir() && within_root(&source, &destination_dir) {
                return Err("不能把文件夹移动到自身内部".into());
            }
            std::fs::rename(&source, &target).map_err(|error| error.to_string())?;
            target
        }
        "delete_file_tree_entry" => {
            if metadata.is_dir() {
                std::fs::remove_dir_all(&source).map_err(|error| error.to_string())?;
            } else {
                std::fs::remove_file(&source).map_err(|error| error.to_string())?;
            }
            return Ok(Value::Null);
        }
        _ => return Err(format!("文件操作未实现：{command}")),
    };
    Ok(Value::String(relative_to_root(&folder, &target)))
}

/// Depth-limited tree walk. `flat_only` keeps descending past `max_depth` for
/// `list_workspace_files`, which wants the whole tree flattened.
fn walk_tree(
    base: &Path,
    dir: &Path,
    depth: i64,
    max_depth: i64,
    flat_only: bool,
    flat: &mut Vec<Value>,
) -> Result<Vec<Value>, String> {
    let mut list = entries(dir)?;
    // Dirs first, stable (Go's sort.SliceStable over IsDir).
    list.sort_by_key(|entry| !is_dir(entry));
    let mut nodes = Vec::new();
    for entry in list {
        if skipped(&entry) {
            continue;
        }
        let full = dir.join(entry.file_name());
        let rel = lexical_rel(base, &full).unwrap_or_default();
        let kind = if is_dir(&entry) { "dir" } else { "file" };
        flat.push(json!({
            "name": entry.file_name().to_string_lossy(),
            "path": rel,
            "kind": kind,
        }));
        let mut node = serde_json::Map::new();
        node.insert("kind".into(), json!(kind));
        node.insert("name".into(), json!(entry.file_name().to_string_lossy()));
        node.insert("path".into(), json!(rel));
        if kind == "dir" {
            let children = if depth < max_depth || flat_only {
                walk_tree(base, &full, depth + 1, max_depth, flat_only, flat)?
            } else {
                Vec::new()
            };
            node.insert("children".into(), Value::Array(children));
        }
        nodes.push(Value::Object(node));
    }
    Ok(nodes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn temp_root(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "codepluse-files-{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(dir.join(".data")).unwrap();
        dir
    }

    fn call(root: &Path, command: &str, args: Value) -> Value {
        handle(root, command, &args).unwrap().unwrap()
    }

    #[test]
    fn sha256_matches_known_digests() {
        assert_eq!(
            sha256_hex(b""),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
        assert_eq!(
            sha256_hex(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        assert_eq!(
            sha256_hex(b"hello\n"),
            "5891b5b522d5df086d0ff0b110fbd9d21bb4fc7163af34d08286a2e846f6be03"
        );
    }

    #[test]
    fn line_endings_are_classified() {
        assert_eq!(line_ending(b"abc"), "none");
        assert_eq!(line_ending(b"a\nb"), "lf");
        assert_eq!(line_ending(b"a\r\nb"), "crlf");
        assert_eq!(line_ending(b"a\r\nb\nc"), "mixed");
    }

    #[test]
    fn read_edit_save_roundtrip_with_etag() {
        let root = temp_root("edit");
        fs::write(root.join("a.txt"), "hello\n").unwrap();
        let read = call(&root, "read_file_for_edit", json!({"path": "a.txt"}));
        assert_eq!(read["line_ending"], "lf");
        assert_eq!(read["readonly"], false);
        let etag = read["etag"].as_str().unwrap().to_string();

        // 陈旧 etag 必须拒绝写入
        let err = handle(
            &root,
            "save_file_content",
            &json!({"path": "a.txt", "content": "x", "expectedEtag": "stale"}),
        )
        .unwrap()
        .unwrap_err();
        assert!(err.starts_with("file_conflict:"), "{err}");

        let saved = call(
            &root,
            "save_file_content",
            json!({"path": "a.txt", "content": "next", "expectedEtag": etag}),
        );
        assert_eq!(saved["line_ending"], "none");
        assert_eq!(fs::read_to_string(root.join("a.txt")).unwrap(), "next");
    }

    #[test]
    fn file_tree_mutations_create_rename_move_and_delete_entries() {
        let root = temp_root("tree-mutations");
        fs::create_dir(root.join("sub")).unwrap();

        let created = call(
            &root,
            "create_file_tree_entry",
            json!({"rootPath": root, "path": "", "name": "draft.txt", "kind": "file"}),
        );
        assert_eq!(created, json!("draft.txt"));
        assert!(root.join("draft.txt").is_file());

        let renamed = call(
            &root,
            "rename_file_tree_entry",
            json!({"rootPath": root, "path": "draft.txt", "newName": "renamed.txt"}),
        );
        assert_eq!(renamed, json!("renamed.txt"));
        assert!(!root.join("draft.txt").exists());

        let moved = call(
            &root,
            "move_file_tree_entry",
            json!({"rootPath": root, "sourcePath": "renamed.txt", "destDir": "sub"}),
        );
        assert_eq!(moved, json!("sub/renamed.txt"));
        assert!(root.join("sub/renamed.txt").is_file());

        let deleted = call(
            &root,
            "delete_file_tree_entry",
            json!({"rootPath": root, "path": "sub/renamed.txt"}),
        );
        assert_eq!(deleted, Value::Null);
        assert!(!root.join("sub/renamed.txt").exists());
    }

    #[test]
    fn file_tree_mutations_reject_traversal_and_overwrite() {
        let root = temp_root("tree-guard");
        fs::write(root.join("existing.txt"), "keep").unwrap();
        let invalid = handle(
            &root,
            "create_file_tree_entry",
            &json!({"rootPath": root, "path": "", "name": "../escape", "kind": "file"}),
        )
        .unwrap()
        .unwrap_err();
        assert_eq!(invalid, "文件或文件夹名称无效");

        let overwrite = handle(
            &root,
            "rename_file_tree_entry",
            &json!({"rootPath": root, "path": "existing.txt", "newName": "existing.txt"}),
        )
        .unwrap()
        .unwrap_err();
        assert_eq!(overwrite, "目标已存在");
        assert_eq!(
            fs::read_to_string(root.join("existing.txt")).unwrap(),
            "keep"
        );
    }

    #[test]
    fn missing_file_and_copy_guards() {
        let root = temp_root("missing");
        // Go's resolveExistingPath collapses a non-existent tail to its parent
        // directory, so a missing file reaches os.Stat as a directory and fails
        // the regular-file check — same message on both sides.
        let err = handle(&root, "read_file_preview", &json!({"path": "nope.txt"}))
            .unwrap()
            .unwrap_err();
        assert_eq!(err, "仅支持小于 8 MiB 的普通文件");
        fs::write(root.join("here.txt"), "x").unwrap();
        let err = handle(
            &root,
            "save_file_copy",
            &json!({"path": "here.txt", "content": "y"}),
        )
        .unwrap()
        .unwrap_err();
        assert_eq!(err, "目标文件已存在");
    }

    #[test]
    fn directory_listing_separates_dirs_and_files() {
        let root = temp_root("list");
        fs::create_dir_all(root.join("dir/sub")).unwrap();
        fs::write(root.join("dir/file.txt"), "x").unwrap();
        fs::write(root.join("top.txt"), "y").unwrap();

        let dirs_only = call(&root, "list_directory_entries", json!({"path": ""}));
        let names: Vec<&str> = dirs_only
            .as_array()
            .unwrap()
            .iter()
            .map(|r| r["name"].as_str().unwrap())
            .collect();
        // `.data` comes from the temporary workspace root; both dirs are listed.
        assert!(names.contains(&"dir"));
        assert!(!names.contains(&"top.txt"), "只有目录: {names:?}");
        let dir_row = dirs_only
            .as_array()
            .unwrap()
            .iter()
            .find(|r| r["name"] == "dir")
            .unwrap();
        assert_eq!(dir_row["hasChildren"], true);

        let with_files = call(&root, "list_directory_with_files", json!({"path": ""}));
        assert_eq!(with_files.as_array().unwrap().len(), 3);
        let file_row = with_files
            .as_array()
            .unwrap()
            .iter()
            .find(|r| r["name"] == "top.txt")
            .unwrap();
        assert_eq!(file_row["isDir"], false);
        assert_eq!(file_row["size"], 1);
    }

    #[test]
    fn tree_skips_ignored_dirs_and_sorts_dirs_first() {
        let root = temp_root("tree");
        fs::create_dir_all(root.join("node_modules/pkg")).unwrap();
        fs::create_dir_all(root.join("src")).unwrap();
        fs::write(root.join("src/main.rs"), "").unwrap();
        fs::write(root.join("README.md"), "").unwrap();

        let tree = call(&root, "get_file_tree", json!({}));
        let names: Vec<&str> = tree
            .as_array()
            .unwrap()
            .iter()
            .map(|n| n["name"].as_str().unwrap())
            .collect();
        assert_eq!(names, vec!["src", "README.md"]);
        assert_eq!(tree[0]["children"][0]["path"], "src/main.rs");

        let flat = call(&root, "list_workspace_files", json!({}));
        let paths: Vec<&str> = flat
            .as_array()
            .unwrap()
            .iter()
            .map(|n| n["path"].as_str().unwrap())
            .collect();
        assert!(paths.contains(&"src/main.rs"));
        assert!(!paths.iter().any(|p| p.contains("node_modules")));
    }

    #[test]
    fn reads_outside_the_workspace_are_rejected() {
        let root = temp_root("boundary");
        let other = temp_root("outside");
        let err = handle(
            &root,
            "read_file_preview",
            &json!({"path": other.join("x.txt").to_string_lossy()}),
        )
        .unwrap()
        .unwrap_err();
        assert!(err.contains("工作目录必须位于"), "{err}");
    }

    #[test]
    fn base64_and_preview_shapes() {
        let root = temp_root("b64");
        fs::write(root.join("a.txt"), "hello\n").unwrap();
        assert_eq!(
            call(&root, "read_file_base64", json!({"path": "a.txt"})),
            json!("aGVsbG8K")
        );
        let preview = call(&root, "read_file_preview", json!({"path": "a.txt"}));
        assert_eq!(preview["content"], "hello\n");
    }
}
