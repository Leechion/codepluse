//! Local environment diagnostics consumed by the Agent Settings dialog.

use std::path::Path;

use serde_json::{json, Value};

use crate::backend::acp::agents;

pub fn owns(command: &str) -> bool {
    command == "acp_env_diagnostics"
}

pub fn handle(root: &Path, command: &str, args: &Value) -> Option<Result<Value, String>> {
    if !owns(command) {
        return None;
    }
    Some(Ok(diagnose(root, args)))
}

fn probe(program: &str) -> Option<(String, String)> {
    #[cfg(windows)]
    let output = {
        use std::os::windows::process::CommandExt;
        std::process::Command::new(program)
            .arg("--version")
            .creation_flags(0x0800_0000)
            .output()
    };
    #[cfg(not(windows))]
    let output = std::process::Command::new(program)
        .arg("--version")
        .output();
    let output = output.ok()?;
    if !output.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&output.stdout)
        .lines()
        .next()
        .unwrap_or("")
        .trim()
        .to_string();
    if text.is_empty() {
        return None;
    }
    Some((text, program.to_string()))
}

fn check(label: &str, value: String, status: &str, hint: Option<&str>) -> Value {
    json!({
        "label": label,
        "value": value,
        "status": status,
        "hint": hint,
    })
}

fn diagnose(root: &Path, args: &Value) -> Value {
    let wanted = args
        .get("agentType")
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty());
    let node = probe("node");
    let npm = probe("npm");
    let mut runtime_checks = vec![check(
        "node",
        node.as_ref()
            .map(|(version, path)| format!("{version} ({path})"))
            .unwrap_or_else(|| "NOT RESOLVED".into()),
        if node.is_some() { "ok" } else { "fail" },
        node.is_none()
            .then_some("Install Node.js and make sure it is on the app PATH."),
    )];
    runtime_checks.push(check(
        "npm",
        npm.as_ref()
            .map(|(version, path)| format!("{version} ({path})"))
            .unwrap_or_else(|| "NOT RESOLVED".into()),
        if npm.is_some() { "ok" } else { "fail" },
        npm.is_none()
            .then_some("Install npm or repair the app PATH."),
    ));

    let mut sections = vec![json!({"title":"Node / npm","checks":runtime_checks})];
    let mut verdict_code = if node.is_none() {
        "node_missing"
    } else if npm.is_none() {
        "npm_missing"
    } else {
        "ok"
    };
    let mut verdict_level = if verdict_code == "ok" { "ok" } else { "fail" };
    let mut verdict_summary = match verdict_code {
        "node_missing" => "Node.js was not found on the application PATH.",
        "npm_missing" => "npm was not found on the application PATH.",
        _ => "Node.js and npm are available on the application PATH.",
    };

    if let Some(agent_type) = wanted {
        let agent = agents::pick(root, agent_type);
        let agent_status = agent
            .as_ref()
            .map(|value| agents::agent_available(root, value))
            .unwrap_or(false);
        let installed_version = agent
            .as_ref()
            .map(|value| agents::installed_version(root, value))
            .filter(|version| !version.is_empty());
        let agent_check = if agent_status {
            check(
                &format!("{agent_type} executable"),
                installed_version
                    .clone()
                    .unwrap_or_else(|| "Installed; version could not be resolved".into()),
                if installed_version.is_some() {
                    "ok"
                } else {
                    "warn"
                },
                installed_version
                    .is_none()
                    .then_some("The app detected the agent but could not read its version."),
            )
        } else {
            check(
                &format!("{agent_type} executable"),
                "NOT RESOLVED".into(),
                "fail",
                Some("Install or configure this agent in Agent Settings."),
            )
        };
        sections.push(json!({"title":"Selected agent","checks":[agent_check]}));
        if !agent_status {
            verdict_code = "not_installed";
            verdict_level = "fail";
            verdict_summary = "This agent does not appear to be installed.";
        } else if installed_version.is_none() && verdict_code == "ok" {
            verdict_code = "installed_but_unresolved";
            verdict_level = "warn";
            verdict_summary =
                "The agent is recorded as installed, but the app cannot locate its executable.";
        }
    }

    let generated_at = time::OffsetDateTime::now_utc()
        .format(&time::format_description::well_known::Rfc3339)
        .unwrap_or_else(|_| "unknown".into());
    let plain_text = format!(
        "===== Codeg environment diagnostics =====\nverdict [{verdict_code}] {verdict_summary}\nnode: {}\nnpm: {}",
        node.as_ref().map(|(version, path)| format!("{version} ({path})")).unwrap_or_else(|| "NOT RESOLVED".into()),
        npm.as_ref().map(|(version, path)| format!("{version} ({path})")).unwrap_or_else(|| "NOT RESOLVED".into()),
    );
    json!({
        "generated_at": generated_at,
        "agent_type": wanted,
        "verdict": {
            "level": verdict_level,
            "code": verdict_code,
            "summary": verdict_summary,
        },
        "sections": sections,
        "plain_text": plain_text,
    })
}
