//! Group-C port of the ACP runtime (`docs/RUST-BACKEND-REMAINING.md` C 组).
//!
//! Go sources, all mirrored behaviour-for-behaviour:
//! `server-go/internal/acp/runtime.go` (driver pool, sessions, turns,
//! permission cards), `server-go/internal/codeg/{connections,server,fork,
//! preflight,agentconfig,custom_launch,custom_archive,question,split,
//! tool_identity,wire,history}.go` (connection layer + the 28 `acp_*`
//! commands), and the launch chain in `server-go/cmd/serve/{wiring,cli,
//! provider_profiles,official_accounts,dsh_model,anthropic_base}.go`.
//!
//! Two process-level differences the port leans on:
//! 1. The agent registry is rebuilt from its persisted sources on demand
//!    (built-in probes, `Store.CustomAgents`, `harness-instances.json`,
//!    `acp-agents.json`, enabled overlays) instead of living in a hub
//!    singleton — the boot-time seed Go does becomes a per-request read, so
//!    agents added by still-Go-served commands stay visible here.
//! 2. Conversation/turn writes go through `store_file` + `goformat` like the
//!    batch-5 store commands, relying on Go's `reloadIfChangedLocked` to pick
//!    them up — same dual-writer contract as the rest of the port.

pub mod agents;
pub mod commands;
pub mod connections;
pub mod delegation;
pub mod custom_install;
pub mod launch;
pub mod providers_core;
pub mod question;
pub mod runtime;
pub mod splits;
pub mod store_conv;
pub mod tool_identity;
pub mod wire;

/// The 28 group-C commands this module owns.
pub fn owns(command: &str) -> bool {
    commands::owns(command)
}
