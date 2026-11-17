//! 项目脚手架与技能检测 —— 补全批次（S8，4 条）。
//!
//! 前端 `codeg-ui/src/lib/api.ts` 已经在调用这 4 条命令，但原 Go sidecar
//! （提交 76712f5）与 Rust 都没有实现过它们（全树搜不到命令字面量），契约只
//! 取自调用点参数名与 `codeg-ui/src/lib/types.ts` 的返回结构，没有可移植的
//! Go 原型。
//!
//! 命令清单：
//! * `create_shadcn_project` —— 在给定 `targetDir` 下创建 shadcn 项目骨架
//!   （目录 + 最小 `package.json`），返回项目绝对路径。**不执行** `pnpm install`
//!   或任何网络安装，避免长时间占资源与 OOM。
//! * `create_hyperframes_project` —— 同上，HyperFrames 示例骨架；`example` /
//!   `resolution` 落进项目元数据文件。
//! * `detect_hyperframes_skills` —— 按代理扫描全局技能目录，回 `[{ agent,
//!   installed }]`；纯文件系统检查。
//! * `install_hyperframes_skills` —— 把所选代理的安装意向落进 `.data/` 记录；
//!   **不**执行 GitHub 克隆，也不伪装下载成功（见下文限制）。
//!
//! 路径安全：项目名只接受不带分隔符 / `..` / 盘符的干净标识；创建前先把
//! `targetDir` 规范化为绝对路径，项目目录必须落在它之内，绝不写出工作区。
//!
//! 限制（L3）：本构建不随附 HyperFrames 技能仓库，也不在这里跑 `git clone`；
//! `install` 只记录“请求安装”的代理，`detect` 仍以真实技能目录为准，未克隆
//! 之前 `installed` 保持 `false`。

use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::{json, Value};

use crate::backend::store_file;

/// 受支持的 HyperFrames 代理及其全局技能根目录（与 `experts.rs` 的映射一致）。
const HYPERFRAMES_AGENTS: [(&str, &str); 3] = [
    ("claude_code", ".claude"),
    ("codex", ".codex"),
    ("gemini", ".gemini"),
];
/// 技能目录下的 HyperFrames 标记子目录名。
const SKILL_MARKER: &str = "hyperframes";

pub fn owns(command: &str) -> bool {
    matches!(
        command,
        "create_hyperframes_project"
            | "create_shadcn_project"
            | "detect_hyperframes_skills"
            | "install_hyperframes_skills"
    )
}

pub fn handle(root: &Path, command: &str, args: &Value) -> Option<Result<Value, String>> {
    if !owns(command) {
        return None;
    }
    Some(run(root, command, args))
}

fn run(root: &Path, command: &str, args: &Value) -> Result<Value, String> {
    match command {
        "create_shadcn_project" => create_shadcn_project(root, args),
        "create_hyperframes_project" => create_hyperframes_project(root, args),
        "detect_hyperframes_skills" => detect_hyperframes_skills(),
        "install_hyperframes_skills" => install_hyperframes_skills(root, args),
        _ => Err(format!("未知的项目/技能命令：{command}")),
    }
}

// ── 路径与参数校验 ──────────────────────────────────────────────────────

fn home_dir() -> PathBuf {
    if let Ok(fake) = std::env::var("HARNESS_HUB_FAKE_HOME") {
        if !fake.is_empty() {
            return PathBuf::from(fake);
        }
    }
    std::env::var("USERPROFILE")
        .or_else(|_| std::env::var("HOME"))
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("/"))
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_millis() as u64)
        .unwrap_or_default()
}

fn required_str(args: &Value, field: &str) -> Result<String, String> {
    args.get(field)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .ok_or_else(|| format!("缺少 {field} 参数"))
}

fn optional_str(args: &Value, field: &str) -> String {
    args.get(field)
        .and_then(Value::as_str)
        .map(str::trim)
        .unwrap_or_default()
        .to_string()
}

/// 项目名：挡掉分隔符、`..`、盘符与空白。
fn validate_project_name(name: &str) -> Result<String, String> {
    if name.is_empty()
        || name.contains('/')
        || name.contains('\\')
        || name.contains("..")
        || name.contains(':')
    {
        return Err("项目名无效：只能使用不含路径分隔符的标识".into());
    }
    Ok(name.to_string())
}

/// 规范化目标目录并返回 `targetDir/<name>`；校验项目目录不越出 `targetDir`。
fn resolve_project_dir(target_dir: &str, project_name: &str) -> Result<PathBuf, String> {
    let target = PathBuf::from(target_dir);
    if target.as_os_str().is_empty() {
        return Err("缺少 targetDir 参数".into());
    }
    fs::create_dir_all(&target).map_err(|e| format!("创建目标目录失败：{e}"))?;
    let target_canon = fs::canonicalize(&target).map_err(|e| format!("目标目录无效：{e}"))?;
    let project = target_canon.join(project_name);
    if !project.starts_with(&target_canon) {
        return Err("项目路径越出目标目录".into());
    }
    Ok(project)
}

/// `package.json` 的 name 字段：小写、非字母数字折成 `-`。
fn slugify(name: &str) -> String {
    let slug: String = name
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() {
                c.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect();
    slug.trim_matches('-').to_string()
}

// ── 项目骨架 ────────────────────────────────────────────────────────────

fn write_package_json(dir: &Path, package: &Value) -> Result<(), String> {
    let path = dir.join("package.json");
    store_file::write_json(&path, package)
}

fn create_shadcn_project(_root: &Path, args: &Value) -> Result<Value, String> {
    let project_name = validate_project_name(&required_str(args, "projectName")?)?;
    let target_dir = required_str(args, "targetDir")?;
    let template = optional_str(args, "template");
    let preset_code = optional_str(args, "presetCode");
    let package_manager = optional_str(args, "packageManager");

    let dir = resolve_project_dir(&target_dir, &project_name)?;
    fs::create_dir_all(dir.join("src/components"))
        .map_err(|e| format!("创建组件目录失败：{e}"))?;
    fs::create_dir_all(dir.join("src/lib"))
        .map_err(|e| format!("创建 lib 目录失败：{e}"))?;

    let package = json!({
        "name": slugify(&project_name),
        "private": true,
        "version": "0.1.0",
        "template": template,
        "packageManager": package_manager,
        "scripts": {
            "dev": "vite dev",
            "build": "vite build",
        },
    });
    write_package_json(&dir, &package)?;

    // presetCode 是前端带入的初始组件代码；留空则放占位 README，不跑安装。
    if preset_code.is_empty() {
        fs::write(dir.join("README.md"), format!("# {project_name}\n\nshadcn 骨架已生成；请在 IDE 中安装依赖后再开发。\n"))
            .map_err(|e| format!("写入 README 失败：{e}"))?;
    } else {
        fs::write(dir.join("src/components").join("Preset.tsx"), preset_code.as_bytes())
            .map_err(|e| format!("写入初始组件失败：{e}"))?;
    }
    Ok(Value::String(dir.to_string_lossy().into_owned()))
}

fn create_hyperframes_project(_root: &Path, args: &Value) -> Result<Value, String> {
    let project_name = validate_project_name(&required_str(args, "projectName")?)?;
    let target_dir = required_str(args, "targetDir")?;
    let example = optional_str(args, "example");
    let resolution = optional_str(args, "resolution");
    let package_manager = optional_str(args, "packageManager");

    let dir = resolve_project_dir(&target_dir, &project_name)?;
    fs::create_dir_all(dir.join("src"))
        .map_err(|e| format!("创建 src 目录失败：{e}"))?;
    fs::create_dir_all(dir.join("examples"))
        .map_err(|e| format!("创建 examples 目录失败：{e}"))?;

    let package = json!({
        "name": slugify(&project_name),
        "private": true,
        "version": "0.1.0",
        "packageManager": package_manager,
        "scripts": {
            "dev": "hyperframes dev",
            "build": "hyperframes build",
        },
    });
    write_package_json(&dir, &package)?;

    let meta = json!({
        "example": example,
        "resolution": resolution,
        "created_at_ms": now_ms(),
    });
    let meta_path = dir.join("hyperframes.json");
    store_file::write_json(&meta_path, &meta)?;

    Ok(Value::String(dir.to_string_lossy().into_owned()))
}

// ── HyperFrames 技能检测 / 安装记录 ─────────────────────────────────────

fn skill_dir(home: &Path, agent_dir: &str) -> PathBuf {
    home.join(agent_dir).join("skills").join(SKILL_MARKER)
}

fn detect_with_home(home: &Path) -> Value {
    let rows: Vec<Value> = HYPERFRAMES_AGENTS
        .iter()
        .map(|(agent, agent_dir)| {
            let dir = skill_dir(home, agent_dir);
            let installed = dir.is_dir()
                && fs::read_dir(&dir)
                    .map(|entries| entries.flatten().next().is_some())
                    .unwrap_or(false);
            json!({ "agent": agent, "installed": installed })
        })
        .collect();
    Value::Array(rows)
}

fn detect_hyperframes_skills() -> Result<Value, String> {
    Ok(detect_with_home(&home_dir()))
}

fn install_record_path(root: &Path) -> PathBuf {
    root.join(".data").join("hyperframes-skills.json")
}

fn install_hyperframes_skills(root: &Path, args: &Value) -> Result<Value, String> {
    let agents = args
        .get("agents")
        .and_then(Value::as_array)
        .ok_or_else(|| "agents 必须是数组".to_string())?;
    let mut selected: Vec<String> = Vec::new();
    for item in agents {
        let agent = item
            .as_str()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .ok_or_else(|| "agents 包含空的代理标识".to_string())?
            .to_string();
        if !HYPERFRAMES_AGENTS.iter().any(|(id, _)| *id == agent) {
            return Err(format!("未知的技能代理：{agent}"));
        }
        if !selected.contains(&agent) {
            selected.push(agent);
        }
    }
    if selected.is_empty() {
        return Err("至少选择一个代理".into());
    }

    // 只落安装意向记录：本构建不克隆仓库、不链接技能目录。记录里明确标
    // `requested`，不写 `installed: true`，避免界面误判技能已就绪。
    let record = json!({
        "agents": selected,
        "status": "requested",
        "updated_at_ms": now_ms(),
        "note": "本构建未执行 GitHub 克隆；技能目录尚未写入，待上游安装流程完成后 detect 才会显示已安装。",
    });
    let path = install_record_path(root);
    let _lock = store_file::lock(&path)?;
    store_file::write_json(&path, &record)?;
    Ok(Value::Null)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_root(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "codepluse-projects-{tag}-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
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
    fn owns_covers_the_four_documented_commands_only() {
        for command in [
            "create_hyperframes_project",
            "create_shadcn_project",
            "detect_hyperframes_skills",
            "install_hyperframes_skills",
        ] {
            assert!(owns(command), "{command} should be owned");
        }
        assert!(!owns("create_project"));
        assert!(!owns("detect_package_manager"));
        assert!(!owns("INSTALL_HYPERFRAMES_SKILLS"));
    }

    #[test]
    fn shadcn_project_creates_skeleton_inside_target_dir() {
        let root = temp_root("shadcn");
        let target = root.join("workspaces");
        fs::create_dir_all(&target).unwrap();
        let out = call(
            &root,
            "create_shadcn_project",
            json!({
                "projectName": "demo-app",
                "template": "sidebar",
                "presetCode": "export default function Demo() { return null }",
                "packageManager": "pnpm",
                "targetDir": target.to_string_lossy(),
            }),
        )
        .unwrap();
        let project = Path::new(out.as_str().unwrap());
        let target_canon = fs::canonicalize(&target).unwrap();
        assert!(project.is_dir());
        assert!(project.starts_with(&target_canon));
        assert!(project.join("src/components").is_dir());
        let package: Value =
            serde_json::from_slice(&fs::read(project.join("package.json")).unwrap()).unwrap();
        assert_eq!(package["name"], "demo-app");
        assert_eq!(package["packageManager"], "pnpm");
        assert_eq!(package["template"], "sidebar");
        assert!(project.join("src/components/Preset.tsx").is_file());
    }

    #[test]
    fn hyperframes_project_writes_metadata_and_skeleton() {
        let root = temp_root("hyperframes");
        let target = root.join("workspaces");
        let out = call(
            &root,
            "create_hyperframes_project",
            json!({
                "projectName": "Motion Demo",
                "example": "hero",
                "resolution": "1920x1080",
                "packageManager": "npm",
                "targetDir": target.to_string_lossy(),
            }),
        )
        .unwrap();
        let project = Path::new(out.as_str().unwrap());
        assert!(project.is_dir());
        let meta: Value =
            serde_json::from_slice(&fs::read(project.join("hyperframes.json")).unwrap()).unwrap();
        assert_eq!(meta["example"], "hero");
        assert_eq!(meta["resolution"], "1920x1080");
        assert!(meta["created_at_ms"].as_u64().unwrap() > 0);
    }

    #[test]
    fn project_creation_rejects_missing_or_traversing_inputs() {
        let root = temp_root("bad");
        let target = root.join("target");
        fs::create_dir_all(&target).unwrap();
        // 缺 projectName。
        assert!(call(
            &root,
            "create_shadcn_project",
            json!({ "targetDir": target.to_string_lossy(), "packageManager": "pnpm" })
        )
        .unwrap_err()
        .contains("缺少 projectName"));
        // 缺 targetDir。
        assert!(call(
            &root,
            "create_hyperframes_project",
            json!({ "projectName": "x", "targetDir": "" })
        )
        .unwrap_err()
        .contains("targetDir"));
        // 路径穿越项目名。
        for bad in ["../escape", "..\\escape", "a/b", "C:\\Windows"] {
            assert!(call(
                &root,
                "create_shadcn_project",
                json!({ "projectName": bad, "targetDir": target.to_string_lossy() })
            )
            .unwrap_err()
            .contains("项目名无效"),
            "{bad} should be rejected"
            );
        }
    }

    #[test]
    fn detect_reports_per_agent_install_state_from_real_dirs() {
        let home = temp_root("home-detect");
        // 只给 claude_code 放技能目录。
        let claude = home.join(".claude").join("skills").join(SKILL_MARKER);
        fs::create_dir_all(&claude).unwrap();
        fs::write(claude.join("README.md"), "# hyperframes").unwrap();

        let rows = detect_with_home(&home);
        let rows = rows.as_array().unwrap();
        assert_eq!(rows.len(), HYPERFRAMES_AGENTS.len());
        let by_agent: std::collections::HashMap<&str, bool> = rows
            .iter()
            .map(|row| (row["agent"].as_str().unwrap(), row["installed"].as_bool().unwrap()))
            .collect();
        assert_eq!(by_agent["claude_code"], true);
        assert_eq!(by_agent["codex"], false);
        assert_eq!(by_agent["gemini"], false);
    }

    #[test]
    fn install_records_requested_agents_without_faking_success() {
        let root = temp_root("install-record");
        call(
            &root,
            "install_hyperframes_skills",
            json!({ "agents": ["claude_code", "codex"] }),
        )
        .unwrap();
        let record: Value =
            serde_json::from_slice(&fs::read(install_record_path(&root)).unwrap()).unwrap();
        assert_eq!(record["status"], "requested");
        assert_eq!(record["agents"], json!(["claude_code", "codex"]));
        assert!(record.get("installed").is_none());

        // 未知代理拒绝；非数组拒绝；空数组拒绝。
        assert!(call(
            &root,
            "install_hyperframes_skills",
            json!({ "agents": ["nope_agent"] })
        )
        .unwrap_err()
        .contains("未知的技能代理"));
        assert!(call(&root, "install_hyperframes_skills", json!({ "agents": "claude_code" }))
            .unwrap_err()
            .contains("agents 必须是数组"));
        assert!(call(&root, "install_hyperframes_skills", json!({ "agents": [] }))
            .unwrap_err()
            .contains("至少选择一个代理"));
    }

    #[test]
    fn unknown_commands_fall_through_to_the_next_module() {
        assert!(handle(&temp_root("fallthrough"), "pet_list", &json!({})).is_none());
    }
}
