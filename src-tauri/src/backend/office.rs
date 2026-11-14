//! Office tooling surface, ported from `server-go/internal/codeg/office.go`
//! (14 commands).
//!
//! Nothing here touches `codeg-workspace.json`: detection looks at PATH and two
//! home-directory locations, and the skill list is a directory scan of
//! `codeg-assets/` plus `.data/office-skills/`. That makes the whole file safe
//! to move command by command.
//!
//! The OfficeCLI binary is not distributed with this repository, so `install`
//! and the rendering commands answer with the same "not available" messages the
//! Go side uses — they are contract, not stubs to be filled in later.

use std::path::{Path, PathBuf};

use serde_json::{json, Value};

pub fn owns(command: &str) -> bool {
    matches!(
        command,
        "officecli_detect"
            | "officecli_install"
            | "officecli_uninstall"
            | "officecli_list_skills"
            | "officecli_sync_skills"
            | "officecli_skill_read_content"
            | "officecli_skill_list_all_install_statuses"
            | "officecli_skill_get_install_status"
            | "officecli_skill_link_to_agent"
            | "officecli_skill_unlink_from_agent"
            | "officecli_skill_apply_links"
            | "officecli_render_html"
            | "start_office_watch"
            | "stop_office_watch"
    )
}

pub fn handle(root: &Path, command: &str, args: &Value) -> Option<Result<Value, String>> {
    if !owns(command) {
        return None;
    }
    Some(run(root, command, args))
}

fn home_dir() -> PathBuf {
    std::env::var("USERPROFILE")
        .or_else(|_| std::env::var("HOME"))
        .map(PathBuf::from)
        .unwrap_or_default()
}

/// `exec.LookPath` for the two names Windows would resolve. Returns the name as
/// written (not the absolute path) because Go reports `path: name`.
fn look_path(name: &str) -> Option<String> {
    let path = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path) {
        if dir.as_os_str().is_empty() {
            continue;
        }
        for candidate in [name.to_string(), format!("{name}.exe")] {
            if dir.join(&candidate).is_file() {
                return Some(name.to_string());
            }
        }
    }
    None
}

fn detect() -> Value {
    let name = "officecli";
    if let Some(found) = look_path(name) {
        return json!({"installed": true, "path": found, "source": "PATH"});
    }
    let home = home_dir();
    for candidate in [
        home.join(".officecli").join("officecli.exe"),
        home.join(".local").join("bin").join("officecli.exe"),
    ] {
        if candidate.is_file() {
            return json!({
                "installed": true,
                "path": candidate.to_string_lossy(),
                "source": "local",
            });
        }
    }
    json!({"installed": false, "path": "", "source": ""})
}

fn skill_dirs(root: &Path) -> [PathBuf; 2] {
    [root.join("codeg-assets"), root.join(".data").join("office-skills")]
}

fn base_name(dir: &Path) -> String {
    dir.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default()
}

fn list_skills(root: &Path) -> Value {
    let mut skills: Vec<Value> = Vec::new();
    let mut seen: Vec<String> = Vec::new();
    for dir in skill_dirs(root) {
        let entries = match std::fs::read_dir(&dir) {
            Ok(entries) => entries,
            Err(_) => continue,
        };
        for entry in entries.flatten() {
            let file_name = entry.file_name().to_string_lossy().into_owned();
            let is_dir = entry.file_type().map(|t| t.is_dir()).unwrap_or(false);
            if is_dir || !file_name.ends_with(".md") {
                continue;
            }
            let name = file_name.trim_end_matches(".md").to_string();
            if seen.contains(&name) {
                continue;
            }
            seen.push(name.clone());
            skills.push(json!({
                "id": format!("office-skill-{name}"),
                "name": name,
                "source": base_name(&dir),
                "path": dir.join(&file_name).to_string_lossy(),
            }));
        }
    }
    json!({ "skills": skills })
}

/// Skill ids are flat directory entries, never paths: anything with a
/// separator, a drive letter or `..` could walk out of the skill directory.
fn valid_skill_name(name: &str) -> bool {
    let bytes = name.as_bytes();
    if bytes.is_empty() || bytes.len() > 128 {
        return false;
    }
    if !(bytes[0].is_ascii_alphanumeric()) {
        return false;
    }
    bytes
        .iter()
        .all(|b| b.is_ascii_alphanumeric() || *b == b'_' || *b == b'-')
}

fn skill_content(root: &Path, skill_id: &str) -> Result<Value, String> {
    let Some(name) = skill_id.strip_prefix("office-skill-") else {
        return Err(format!("未知的技能：{skill_id}"));
    };
    if !valid_skill_name(name) {
        return Err("技能 ID 无效".into());
    }
    for dir in skill_dirs(root) {
        if let Ok(data) = std::fs::read(dir.join(format!("{name}.md"))) {
            return Ok(json!({
                "id": skill_id,
                "name": name,
                "content": String::from_utf8_lossy(&data),
            }));
        }
    }
    Err(format!("技能文件不存在：{name}"))
}

fn run(root: &Path, command: &str, args: &Value) -> Result<Value, String> {
    match command {
        "officecli_detect" => Ok(detect()),
        "officecli_install" => {
            if detect()["installed"] == Value::Bool(true) {
                return Ok(json!({"installed": true, "message": "OfficeCLI 已安装"}));
            }
            Err("本仓库不分发 OfficeCLI 二进制：请从上游获取后放入 PATH 或 ~/.officecli/".into())
        }
        "officecli_uninstall" => Ok(json!({"uninstalled": true, "message": "未托管安装，无需卸载"})),
        "officecli_list_skills" | "officecli_sync_skills" => Ok(list_skills(root)),
        "officecli_skill_read_content" => {
            let skill_id = args.get("skillId").and_then(|v| v.as_str()).unwrap_or("");
            skill_content(root, skill_id)
        }
        // Frontend contract is `ExpertInstallStatus[]` (an array). Without the
        // OfficeCLI binary no office skill is linked to an agent, so an empty
        // array is the honest answer — an object would make the caller's
        // spread (`[...statuses]`) throw "not iterable".
        "officecli_skill_list_all_install_statuses" | "officecli_skill_get_install_status" => {
            Ok(json!([]))
        }
        "officecli_skill_link_to_agent"
        | "officecli_skill_unlink_from_agent"
        | "officecli_skill_apply_links" => Err("OfficeCLI 未安装，技能无法关联到执行端".into()),
        "officecli_render_html" | "start_office_watch" | "stop_office_watch" => {
            Err("Office 渲染需要 OfficeCLI 二进制，当前未安装".into())
        }
        _ => Err(format!("未知的 Office 命令：{command}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn temp_root(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "codepluse-office-{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
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
    fn skills_are_listed_from_both_directories_without_duplicates() {
        let root = temp_root("skills");
        let assets = root.join("codeg-assets");
        let user = root.join(".data").join("office-skills");
        fs::create_dir_all(&assets).unwrap();
        fs::create_dir_all(&user).unwrap();
        fs::write(assets.join("docx.md"), "# docx").unwrap();
        fs::write(user.join("docx.md"), "# shadowed").unwrap();
        fs::write(user.join("xlsx.md"), "# xlsx").unwrap();
        fs::create_dir_all(assets.join("nested.md")).unwrap();

        let out = call(&root, "officecli_list_skills", json!({})).unwrap();
        let skills = out["skills"].as_array().unwrap();
        assert_eq!(skills.len(), 2, "{out}");
        assert_eq!(skills[0]["id"], "office-skill-docx");
        assert_eq!(skills[0]["source"], "codeg-assets");
        assert_eq!(skills[1]["id"], "office-skill-xlsx");
        assert_eq!(skills[1]["source"], "office-skills");
        // sync_skills answers with the same payload.
        assert_eq!(
            call(&root, "officecli_sync_skills", json!({})).unwrap(),
            out
        );
    }

    #[test]
    fn skill_content_reads_the_first_match_and_validates_ids() {
        let root = temp_root("content");
        let assets = root.join("codeg-assets");
        fs::create_dir_all(&assets).unwrap();
        fs::write(assets.join("docx.md"), "# docx").unwrap();

        let out = call(
            &root,
            "officecli_skill_read_content",
            json!({"skillId": "office-skill-docx"}),
        )
        .unwrap();
        assert_eq!(out["content"], "# docx");

        for bad in [
            "officecli-docx",
            "office-skill-../secret",
            "office-skill-x/y",
            "office-skill-",
        ] {
            let err = call(
                &root,
                "officecli_skill_read_content",
                json!({"skillId": bad}),
            )
            .unwrap_err();
            assert!(
                err.starts_with("未知的技能：") || err == "技能 ID 无效",
                "{bad} -> {err}"
            );
        }

        let err = call(
            &root,
            "officecli_skill_read_content",
            json!({"skillId": "office-skill-missing"}),
        )
        .unwrap_err();
        assert_eq!(err, "技能文件不存在：missing");
    }

    #[test]
    fn install_status_is_an_array_so_callers_can_spread_it() {
        let root = temp_root("status");
        for command in [
            "officecli_skill_list_all_install_statuses",
            "officecli_skill_get_install_status",
        ] {
            let out = call(&root, command, json!({})).unwrap();
            assert_eq!(out, json!([]), "{command}");
        }
    }

    #[test]
    fn unavailable_commands_explain_themselves() {
        let root = temp_root("missing");
        assert_eq!(
            call(&root, "officecli_install", json!({})).unwrap_err(),
            "本仓库不分发 OfficeCLI 二进制：请从上游获取后放入 PATH 或 ~/.officecli/"
        );
        assert_eq!(
            call(&root, "officecli_uninstall", json!({})).unwrap(),
            json!({"uninstalled": true, "message": "未托管安装，无需卸载"})
        );
        assert_eq!(
            call(&root, "officecli_render_html", json!({})).unwrap_err(),
            "Office 渲染需要 OfficeCLI 二进制，当前未安装"
        );
        assert_eq!(
            call(
                &root,
                "officecli_skill_link_to_agent",
                json!({"skillId": "x", "agentType": "claude"})
            )
            .unwrap_err(),
            "OfficeCLI 未安装，技能无法关联到执行端"
        );
    }

    #[test]
    fn detect_reports_a_shape_even_when_absent() {
        let root = temp_root("detect");
        let out = call(&root, "officecli_detect", json!({})).unwrap();
        assert!(out.get("installed").unwrap().is_boolean());
        assert!(out.get("path").unwrap().is_string());
        assert!(out.get("source").unwrap().is_string());
    }

    #[test]
    fn only_office_commands_are_owned() {
        let root = temp_root("owns");
        assert!(handle(&root, "experts_list", &json!({})).is_none());
        assert!(handle(&root, "get_stats", &json!({})).is_none());
    }
}
