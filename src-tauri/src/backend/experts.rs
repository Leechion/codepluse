//! Bundled skill packs (`codeg-assets/<group>`) and the link/unlink plumbing
//! that makes a skill visible to one agent, ported from
//! `server-go/internal/codeg/skills.go`.
//!
//! Ported (10): `experts_list`, `experts_read_content`,
//! `experts_link_to_agent`, `experts_unlink_from_agent`, `experts_apply_links`
//! and the four `science_*` equivalents.
//!
//! Left in Go:
//! - `experts_list_all_install_statuses` / `science_*` — they enumerate
//!   `s.agents()`, which reads the store overlay and probes every agent's
//!   installed version.
//! - `acp_list_agent_skills` — needs the runtime registry to reject unknown
//!   agent types.
//!
//! The interesting part is the Windows fallback: creating a directory symlink
//! needs Developer Mode, so a failed `symlink_dir` falls back to copying the
//! tree and stamping `.codeg-copy` with the central path. Disable only ever
//! removes a link or a stamped copy — never a user's own directory.

use std::path::{Path, PathBuf};

use serde_json::{json, Value};

use crate::backend::files::sha256_hex;
use crate::backend::git::{eval_symlinks, strip_verbatim};

/// Marks a skill directory codepluse copied (rather than linked) into an
/// agent's skill directory.
const COPY_MARKER: &str = ".codeg-copy";

pub fn owns(command: &str) -> bool {
    matches!(
        command,
        "experts_list"
            | "experts_read_content"
            | "experts_link_to_agent"
            | "experts_unlink_from_agent"
            | "experts_apply_links"
            | "science_list"
            | "science_read_content"
            | "science_link_to_agent"
            | "science_unlink_from_agent"
            | "science_apply_links"
            | "acp_list_agent_skills"
            | "acp_list_agent_runtime_skills"
            | "experts_list_all_install_statuses"
            | "science_list_all_install_statuses"
            | "experts_get_install_status"
            | "science_get_install_status"
    )
}

pub fn handle(root: &Path, command: &str, args: &Value) -> Option<Result<Value, String>> {
    if !owns(command) {
        return None;
    }
    Some(run(root, command, args))
}

fn text(args: &Value, key: &str) -> String {
    args.get(key).and_then(|v| v.as_str()).unwrap_or("").to_string()
}

fn group_of(command: &str) -> &'static str {
    if command.starts_with("science_") {
        "science"
    } else {
        "experts"
    }
}

fn home_dir() -> PathBuf {
    std::env::var("USERPROFILE")
        .or_else(|_| std::env::var("HOME"))
        .map(PathBuf::from)
        .unwrap_or_default()
}

/// A named instance (`inst_claude2`, `inst_main_claude_<digest>`) is the same
/// harness with a pinned provider/model, so it reads and links into the BASE
/// tool's directory: giving each profile its own skill directory would leave
/// every instance-started conversation with an empty skill set.
fn skill_tool(agent_type: &str) -> &'static str {
    match agent_type {
        "claude_code" | "claude" => "claude",
        "codex" => "codex",
        "deepseek" | "dsh" => "dsh",
        _ => {
            if let Some(rest) = agent_type.strip_prefix("inst_") {
                let rest = rest.strip_prefix("main_").unwrap_or(rest);
                for tool in ["claude", "codex", "dsh", "deepseek"] {
                    if rest.starts_with(tool) {
                        return skill_tool(tool);
                    }
                }
            }
            ""
        }
    }
}

fn native_skill_root(home: &Path, agent_type: &str) -> PathBuf {
    let dir = match skill_tool(agent_type) {
        "claude" => ".claude",
        "codex" => ".codex",
        _ => ".agents",
    };
    home.join(dir).join("skills")
}

/// Go `skills()`'s agent-scope branch (skills.go:294): scan the user and
/// project skill directories for `SKILL.md` files. The runtime registry
/// (acp::agents) rejects unknown agent types.
fn list_agent_skills(home: &Path, root: &Path, args: &Value) -> Result<Value, String> {
    let agent_type = text(args, "agentType");
    if !crate::backend::acp::agents::agent_exists(root, &agent_type) {
        return Err("执行端不存在".into());
    }
    let mut candidates: Vec<(Value, PathBuf)> =
        vec![(serde_json::json!({"scope": "user"}), native_skill_root(home, &agent_type))];
    let workspace_path = text(args, "workspacePath");
    if !workspace_path.is_empty() {
        if let Ok(resolved) = crate::backend::acp::connections::resolve_workdir(root, &workspace_path) {
            candidates.push((
                serde_json::json!({"scope": "project"}),
                resolved.join(".agents").join("skills"),
            ));
        }
    }
    let mut locations: Vec<Value> = Vec::new();
    let mut items: Vec<Value> = Vec::new();
    for (scope, root_dir) in candidates {
        let exists = root_dir.is_dir();
        locations.push(serde_json::json!({
            "scope": scope.get("scope").cloned().unwrap_or(Value::Null),
            "path": root_dir.to_string_lossy(),
            "exists": exists,
        }));
        if let Ok(entries) = std::fs::read_dir(&root_dir) {
            for entry in entries.flatten() {
                let file = entry.path().join("SKILL.md");
                if !file.is_file() {
                    continue;
                }
                let name = entry.file_name().to_string_lossy().to_string();
                items.push(serde_json::json!({
                    "id": name, "name": name, "scope": scope.get("scope").cloned().unwrap_or(Value::Null),
                    "layout": "directory", "path": file.to_string_lossy(),
                    "description": Value::Null, "read_only": true,
                }));
            }
        }
    }
    Ok(serde_json::json!({
        "supported": true, "message": Value::Null, "locations": locations, "skills": items,
    }))
}

/// Go compares resolved paths with `strings.EqualFold`.
fn eq_fold(a: &str, b: &str) -> bool {
    a.to_lowercase() == b.to_lowercase()
}

fn resolved_text(path: &Path) -> String {
    strip_verbatim(eval_symlinks(path)).to_string_lossy().into_owned()
}

fn is_symlink(path: &Path) -> bool {
    std::fs::symlink_metadata(path)
        .map(|m| m.file_type().is_symlink())
        .unwrap_or(false)
}

/// One row per manifest entry: metadata plus whether the shipped copy is
/// present and unmodified.
fn bundled_skills(root: &Path, group: &str) -> Result<Vec<Value>, String> {
    let base = root.join("codeg-assets").join(group);
    let raw = std::fs::read(base.join("manifest.json")).map_err(|e| e.to_string())?;
    let metadata: Vec<Value> = serde_json::from_slice(&raw).map_err(|e| e.to_string())?;
    let mut out = Vec::with_capacity(metadata.len());
    for meta in metadata {
        let id = meta.get("id").and_then(|v| v.as_str()).unwrap_or("");
        let dir = base.join("skills").join(id);
        let file = dir.join("SKILL.md");
        let (installed, body) = match std::fs::read(&file) {
            Ok(body) => (true, body),
            Err(_) => (false, Vec::new()),
        };
        let bundled_hash = meta.get("bundled_hash").and_then(|v| v.as_str()).unwrap_or("");
        out.push(json!({
            "metadata": meta,
            "installed_centrally": installed,
            "user_modified": sha256_hex(&body) != bundled_hash,
            "central_path": dir.to_string_lossy(),
        }));
    }
    Ok(out)
}

/// The per-(skill, agent) row the settings matrix reads: enabled only when the
/// entry at the agent's own skill directory points at codeg's central copy.
fn skill_status(home: &Path, name: &str, agent_type: &str, central: &str) -> Value {
    let link = native_skill_root(home, agent_type).join(name);
    let mut state = "not_linked";
    let mut copy_mode = false;
    let mut target = Value::Null;
    if is_symlink(&link) {
        match std::fs::canonicalize(&link) {
            Ok(_) => {
                let actual = resolved_text(&link);
                target = json!(actual);
                state = if eq_fold(&actual, central) {
                    "linked_to_codeg"
                } else {
                    "linked_elsewhere"
                };
            }
            Err(_) => state = "broken",
        }
    } else if link.exists() {
        // A real directory: only ours if it carries the copy marker.
        match std::fs::read(link.join(COPY_MARKER)) {
            Ok(marker) if eq_fold(String::from_utf8_lossy(&marker).trim(), central) => {
                target = json!(String::from_utf8_lossy(&marker).trim());
                state = "linked_to_codeg";
                copy_mode = true;
            }
            _ => state = "blocked_by_real_directory",
        }
    }
    json!({
        "expertId": name,
        "agentType": agent_type,
        "state": state,
        "linkPath": link.to_string_lossy(),
        "targetPath": target,
        "expectedTargetPath": central,
        "copyMode": copy_mode,
    })
}

fn copy_dir(src: &Path, dst: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(dst)?;
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let from = entry.path();
        let to = dst.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            copy_dir(&from, &to)?;
        } else {
            std::fs::copy(&from, &to)?;
        }
    }
    Ok(())
}

#[cfg(windows)]
fn make_symlink(target: &Path, link: &Path) -> std::io::Result<()> {
    std::os::windows::fs::symlink_dir(target, link)
}

#[cfg(unix)]
fn make_symlink(target: &Path, link: &Path) -> std::io::Result<()> {
    std::os::unix::fs::symlink(target, link)
}

/// Enable or disable one bundled skill for one agent. Only links/copies codeg
/// itself put there are removed, and a same-named real directory is never
/// touched.
fn skill_link(
    root: &Path,
    home: &Path,
    group: &str,
    skill_id: &str,
    agent_type: &str,
    enable: bool,
) -> Result<Value, String> {
    let list = bundled_skills(root, group)?;
    let mut central = String::new();
    for item in &list {
        if item["metadata"]["id"].as_str() == Some(skill_id) {
            central = item["central_path"].as_str().unwrap_or("").to_string();
            break;
        }
    }
    if central.is_empty() {
        return Err(format!("技能不存在：{skill_id}"));
    }
    let central_path = PathBuf::from(&central);
    let root_dir = native_skill_root(home, agent_type);
    let link = root_dir.join(skill_id);

    if enable {
        std::fs::create_dir_all(&root_dir)
            .map_err(|e| format!("无法创建技能目录 {}：{e}", root_dir.display()))?;

        if let Ok(meta) = std::fs::symlink_metadata(&link) {
            if meta.file_type().is_symlink() {
                let actual = resolved_text(&link);
                if !eq_fold(&actual, &central) {
                    return Err(format!("已存在指向 {actual} 的链接，未改动"));
                }
                return Ok(skill_status(home, skill_id, agent_type, &central));
            }
            if let Ok(marker) = std::fs::read(link.join(COPY_MARKER)) {
                if eq_fold(String::from_utf8_lossy(&marker).trim(), &central) {
                    return Ok(skill_status(home, skill_id, agent_type, &central));
                }
            }
            return Err(format!("目标位置已有同名目录，未改动：{}", link.display()));
        }

        if let Err(err) = make_symlink(&central_path, &link) {
            // Windows without Developer Mode: copy the skill instead, which the
            // harness reads exactly like a linked directory.
            if copy_dir(&central_path, &link).is_err() {
                return Err(format!("创建技能链接失败（可开启 Windows 开发者模式）：{err}"));
            }
            if let Err(mark_err) = std::fs::write(link.join(COPY_MARKER), central.as_bytes()) {
                let _ = std::fs::remove_dir_all(&link);
                return Err(mark_err.to_string());
            }
        }
        return Ok(skill_status(home, skill_id, agent_type, &central));
    }

    let Ok(meta) = std::fs::symlink_metadata(&link) else {
        return Ok(skill_status(home, skill_id, agent_type, &central));
    };
    if !meta.file_type().is_symlink() {
        let marker = std::fs::read(link.join(COPY_MARKER));
        let ours = marker
            .as_ref()
            .map(|m| eq_fold(String::from_utf8_lossy(m).trim(), &central))
            .unwrap_or(false);
        if !ours {
            return Err(format!(
                "目标不是 codepluse 创建的链接或副本，未删除：{}",
                link.display()
            ));
        }
        std::fs::remove_dir_all(&link).map_err(|e| e.to_string())?;
        return Ok(skill_status(home, skill_id, agent_type, &central));
    }
    let actual = resolved_text(&link);
    if !eq_fold(&actual, &central) {
        return Err(format!("链接指向别处，未删除：{actual}"));
    }
    std::fs::remove_file(&link).map_err(|e| e.to_string())?;
    Ok(skill_status(home, skill_id, agent_type, &central))
}

fn run(root: &Path, command: &str, args: &Value) -> Result<Value, String> {
    run_with_home(&home_dir(), root, command, args)
}

/// Split out so tests can point the agent skill directories at a temp folder:
/// linking writes into `%USERPROFILE%\.claude\skills`, which a test must never
/// touch for real.
fn run_with_home(home: &Path, root: &Path, command: &str, args: &Value) -> Result<Value, String> {
    let group = group_of(command);
    if command == "acp_list_agent_runtime_skills" || command == "acp_list_agent_skills" {
        return list_agent_skills(home, root, args);
    }
    // 安装状态（skills.go:236-262）：每个技能 × 每个已注册执行端一行。
    if command.ends_with("_list_all_install_statuses") {
        let list = bundled_skills(root, group)?;
        let agents = crate::backend::acp::commands::agents_rows(root);
        let mut out = Vec::new();
        for item in &list {
            let name = item["metadata"]["id"].as_str().unwrap_or("");
            let central = item["central_path"].as_str().unwrap_or("");
            for agent in &agents {
                let agent_type = agent["agent_type"].as_str().unwrap_or("");
                out.push(skill_status(home, name, agent_type, central));
            }
        }
        return Ok(Value::Array(out));
    }
    if command.ends_with("_get_install_status") {
        let mut name = text(args, "expertId");
        if name.is_empty() {
            name = text(args, "skillId");
        }
        let agent_type = text(args, "agentType");
        let list = bundled_skills(root, group)?;
        for item in &list {
            if item["metadata"]["id"].as_str() == Some(name.as_str()) {
                let central = item["central_path"].as_str().unwrap_or("");
                return Ok(skill_status(home, &name, &agent_type, central));
            }
        }
        return Err(format!("技能不存在：{name}"));
    }
    match command {
        "experts_list" | "science_list" => {
            let list = bundled_skills(root, group)?;
            Ok(Value::Array(list))
        }
        "experts_read_content" | "science_read_content" => {
            let mut name = text(args, "expertId");
            if name.is_empty() {
                name = text(args, "skillId");
            }
            let list = bundled_skills(root, group)?;
            for item in list {
                if item["metadata"]["id"].as_str() == Some(name.as_str()) {
                    let file = PathBuf::from(item["central_path"].as_str().unwrap_or(""))
                        .join("SKILL.md");
                    let body = std::fs::read(&file).map_err(|e| e.to_string())?;
                    return Ok(Value::String(String::from_utf8_lossy(&body).into_owned()));
                }
            }
            Err("技能不存在".into())
        }
        "experts_link_to_agent" | "science_link_to_agent" => skill_link(
            root,
            home,
            group,
            &text(args, "expertId"),
            &text(args, "agentType"),
            true,
        ),
        "experts_unlink_from_agent" | "science_unlink_from_agent" => skill_link(
            root,
            home,
            group,
            &text(args, "expertId"),
            &text(args, "agentType"),
            false,
        ),
        "experts_apply_links" | "science_apply_links" => {
            let mut out = Vec::new();
            let ops = args.get("ops").and_then(|v| v.as_array()).cloned().unwrap_or_default();
            for op in ops {
                let enable = op.get("enable").and_then(|v| v.as_bool()).unwrap_or(false);
                let expert_id = op.get("expertId").and_then(|v| v.as_str()).unwrap_or("");
                let agent_type = op.get("agentType").and_then(|v| v.as_str()).unwrap_or("");
                let outcome = skill_link(root, home, group, expert_id, agent_type, enable);
                let mut row = json!({
                    "expertId": expert_id,
                    "agentType": agent_type,
                    "ok": outcome.is_ok(),
                    "status": Value::Null,
                    "error": Value::Null,
                });
                match outcome {
                    Err(message) => row["error"] = json!(message),
                    // Disable rows report no status: the caller only cares that
                    // the link is gone.
                    Ok(status) if enable => row["status"] = status,
                    Ok(_) => {}
                }
                out.push(row);
            }
            Ok(Value::Array(out))
        }
        _ => Err(format!("技能操作尚未接通：{command}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    struct Fixture {
        root: PathBuf,
        home: PathBuf,
    }

    fn fixture(tag: &str) -> Fixture {
        let base = std::env::temp_dir().join(format!(
            "codepluse-experts-{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let root = base.join("root");
        let home = base.join("home");
        fs::create_dir_all(&root).unwrap();
        fs::create_dir_all(&home).unwrap();
        Fixture { root, home }
    }

    /// One skill pack whose manifest hash matches the shipped file.
    fn seed(root: &Path, group: &str) {
        let base = root.join("codeg-assets").join(group);
        let dir = base.join("skills").join("one");
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("SKILL.md"), "# one\n").unwrap();
        let manifest = serde_json::json!([{
            "id": "one",
            "display_name": {"en": "One"},
            "bundled_hash": sha256_hex(b"# one\n"),
        }]);
        fs::write(base.join("manifest.json"), manifest.to_string()).unwrap();
    }

    fn call(f: &Fixture, command: &str, args: Value) -> Result<Value, String> {
        run_with_home(&f.home, &f.root, command, &args)
    }

    #[test]
    fn list_reports_installed_and_unmodified() {
        let f = fixture("list");
        seed(&f.root, "experts");
        let out = call(&f, "experts_list", serde_json::json!({})).unwrap();
        assert_eq!(out.as_array().unwrap().len(), 1);
        assert_eq!(out[0]["metadata"]["id"], "one");
        assert_eq!(out[0]["installed_centrally"], true);
        assert_eq!(out[0]["user_modified"], false);
        assert!(out[0]["central_path"]
            .as_str()
            .unwrap()
            .ends_with(&format!("experts{}skills{}one", std::path::MAIN_SEPARATOR, std::path::MAIN_SEPARATOR)));

        // Editing the file flips user_modified; deleting it flips both flags.
        fs::write(
            f.root.join("codeg-assets/experts/skills/one/SKILL.md"),
            "changed",
        )
        .unwrap();
        let out = call(&f, "experts_list", serde_json::json!({})).unwrap();
        assert_eq!(out[0]["user_modified"], true);
        fs::remove_file(f.root.join("codeg-assets/experts/skills/one/SKILL.md")).unwrap();
        let out = call(&f, "experts_list", serde_json::json!({})).unwrap();
        assert_eq!(out[0]["installed_centrally"], false);
        // Go hashes the empty slice it read, so this stays true.
        assert_eq!(out[0]["user_modified"], true);
    }

    #[test]
    fn read_content_prefers_expert_id_and_falls_back_to_skill_id() {
        let f = fixture("read");
        seed(&f.root, "science");
        assert_eq!(
            call(&f, "science_read_content", serde_json::json!({"expertId": "one"})).unwrap(),
            "# one\n"
        );
        assert_eq!(
            call(&f, "science_read_content", serde_json::json!({"skillId": "one"})).unwrap(),
            "# one\n"
        );
        assert_eq!(
            call(&f, "science_read_content", serde_json::json!({"expertId": "nope"})).unwrap_err(),
            "技能不存在"
        );
        // Groups do not bleed into each other: `experts` has no manifest here.
        assert!(call(&f, "experts_read_content", serde_json::json!({"expertId": "one"})).is_err());
    }

    #[test]
    fn link_then_unlink_roundtrip() {
        let f = fixture("link");
        seed(&f.root, "experts");
        let central = f.root.join("codeg-assets/experts/skills/one");

        let status = call(
            &f,
            "experts_link_to_agent",
            serde_json::json!({"expertId": "one", "agentType": "claude"}),
        )
        .unwrap();
        assert_eq!(status["state"], "linked_to_codeg", "{status}");
        assert_eq!(status["expertId"], "one");
        assert_eq!(status["agentType"], "claude");
        assert_eq!(status["copyMode"], !fs::symlink_metadata(native_skill_root(&f.home, "claude").join("one"))
            .map(|m| m.file_type().is_symlink())
            .unwrap_or(false));
        let link = native_skill_root(&f.home, "claude").join("one");
        assert!(link.exists());

        // Idempotent: linking twice keeps the same state.
        let again = call(
            &f,
            "experts_link_to_agent",
            serde_json::json!({"expertId": "one", "agentType": "claude"}),
        )
        .unwrap();
        assert_eq!(again["state"], "linked_to_codeg");

        let after = call(
            &f,
            "experts_unlink_from_agent",
            serde_json::json!({"expertId": "one", "agentType": "claude"}),
        )
        .unwrap();
        assert_eq!(after["state"], "not_linked");
        assert!(!link.exists());
        assert!(central.exists(), "the central copy must survive");
    }

    #[test]
    fn a_real_directory_is_never_removed() {
        let f = fixture("blocked");
        seed(&f.root, "experts");
        let link = native_skill_root(&f.home, "codex").join("one");
        fs::create_dir_all(&link).unwrap();
        fs::write(link.join("keep.txt"), "mine").unwrap();

        let err = call(
            &f,
            "experts_link_to_agent",
            serde_json::json!({"expertId": "one", "agentType": "codex"}),
        )
        .unwrap_err();
        assert!(err.starts_with("目标位置已有同名目录，未改动："), "{err}");

        let err = call(
            &f,
            "experts_unlink_from_agent",
            serde_json::json!({"expertId": "one", "agentType": "codex"}),
        )
        .unwrap_err();
        assert!(
            err.starts_with("目标不是 codepluse 创建的链接或副本，未删除："),
            "{err}"
        );
        assert!(link.join("keep.txt").exists());

        // A foreign symlink is reported instead of being deleted. Creating one
        // needs Windows Developer Mode; without the privilege the setup cannot
        // build the fixture, so the case is skipped (same concession the
        // apply_links test makes below).
        fs::remove_dir_all(&link).unwrap();
        fs::create_dir_all(f.root.join("elsewhere")).unwrap();
        if make_symlink(&f.root.join("elsewhere"), &link).is_ok() {
            let status = call(
                &f,
                "experts_unlink_from_agent",
                serde_json::json!({"expertId": "one", "agentType": "codex"}),
            );
            match status {
                Err(err) if err.starts_with("链接指向别处") => {}
                Ok(_) => panic!("a foreign link must not be removed"),
                Err(other) => assert!(other.starts_with("链接指向别处"), "{other}"),
            }
            let _ = fs::remove_dir_all(&link);
        }
    }

    #[test]
    fn apply_links_reports_per_op_rows() {
        let f = fixture("apply");
        seed(&f.root, "experts");
        let out = call(
            &f,
            "experts_apply_links",
            serde_json::json!({"ops": [
                {"expertId": "one", "agentType": "claude", "enable": true},
                {"expertId": "missing", "agentType": "claude", "enable": true},
                {"expertId": "one", "agentType": "claude", "enable": false}
            ]}),
        )
        .unwrap();
        let rows = out.as_array().unwrap();
        assert_eq!(rows.len(), 3);
        assert_eq!(rows[0]["ok"], true);
        assert_eq!(rows[0]["status"]["state"], "linked_to_codeg");
        assert_eq!(rows[0]["error"], Value::Null);
        assert_eq!(rows[1]["ok"], false);
        assert_eq!(rows[1]["error"], "技能不存在：missing");
        assert_eq!(rows[1]["status"], Value::Null);
        // Disabling leaves `status` null even when it succeeded.
        assert_eq!(rows[2]["ok"], true);
        assert_eq!(rows[2]["status"], Value::Null);
        assert_eq!(rows[2]["error"], Value::Null);
    }

    #[test]
    fn instance_names_map_to_the_base_tool() {
        assert_eq!(skill_tool("inst_claude2"), "claude");
        assert_eq!(skill_tool("inst_main_claude_abc123"), "claude");
        assert_eq!(skill_tool("inst_codex"), "codex");
        assert_eq!(skill_tool("unknown"), "");
        let home = Path::new("H:/home");
        assert!(native_skill_root(home, "inst_dsh").ends_with(".agents/skills"));
        assert!(native_skill_root(home, "claude_code").ends_with(".claude/skills"));
    }

    #[test]
    fn install_statuses_cover_every_registered_agent() {
        let f = fixture("defer");
        seed(&f.root, "experts");
        // 一个技能 × N 个已注册执行端。
        let out = handle(
            &f.root,
            "experts_list_all_install_statuses",
            &serde_json::json!({}),
        )
        .unwrap()
        .unwrap();
        assert!(out.is_array());
        let row = handle(
            &f.root,
            "experts_get_install_status",
            &serde_json::json!({"expertId": "one", "agentType": "claude"}),
        )
        .unwrap()
        .unwrap();
        assert_eq!(row["expertId"], "one");
        assert_eq!(row["agentType"], "claude");
        seed(&f.root, "science");
        let err = handle(
            &f.root,
            "science_get_install_status",
            &serde_json::json!({"expertId": "nope"}),
        )
        .unwrap()
        .unwrap_err();
        assert_eq!(err, "技能不存在：nope");
    }

    /// The agent-skill listing moved into this module once the runtime
    /// registry (acp::agents) existed: unknown agents still reject.
    #[test]
    fn agent_skills_reject_unknown_agents() {
        let f = fixture("defer");
        let outcome = handle(
            &f.root,
            "acp_list_agent_skills",
            &serde_json::json!({"agentType": "nope"}),
        )
        .expect("owned now");
        assert_eq!(outcome.unwrap_err(), "执行端不存在");
    }
}
