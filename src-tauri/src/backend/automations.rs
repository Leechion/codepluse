//! 定时任务：`server-go/internal/codeg/automations.go` 的整流迁移（11 条
//! `automation_*` 命令 + 30 秒调度器）。存储是两个独立 JSON 文件
//! （`.data/codeg-automations.json` / `codeg-automation-runs.json`），写入
//! 走 tmp+rename，字段序按 Go struct 声明序（[`goformat::marshal_preserve`]），
//! 嵌套 map 值（提示词块 / config_values / label_snapshot）按 Go 的 map
//! marshal 排序（[`goformat::canon`]）。
//!
//! cron 数学复刻 `internal/orch` 的 5 字段解析器（[`parse_cron`]），按本地
//! 时区逐分钟前扫一年。
//!
//! 运行执行需要 ACP：C 组的运行时还在 Go 侧，这里经 [`Bridge`] 把
//! `acp_connect` / `acp_prompt` / `acp_disconnect` / `acp_cancel` POST 回
//! 前门自身（Rust 已接管的命令由 Rust 应答，其余照旧转发给 Go）——C 组
//! 落地后本模块不用改。调度器由 mod.rs 在 serve() 时启动；Go 侧的
//! `TickAutomations` 调用已随本批删除，避免双份触发。

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use serde_json::{json, Map, Value};
use time::macros::format_description;
use time::OffsetDateTime;

use crate::backend::{emit_event, goformat, store_file, store_writes};

pub fn owns(command: &str) -> bool {
    command.starts_with("automation_")
}

/// 同步入口；`runtime` 用来把运行执行任务挂回 tokio（本函数跑在阻塞池）。
pub fn handle(
    root: &Path,
    command: &str,
    args: &Value,
    runtime: &tokio::runtime::Handle,
) -> Option<Result<Value, String>> {
    if !owns(command) {
        return None;
    }
    Some(run(root, command, args, runtime))
}

fn txt(args: &Value, key: &str) -> String {
    value_text(args.get(key).unwrap_or(&Value::Null))
}

fn value_text(v: &Value) -> String {
    v.as_str().unwrap_or("").to_string()
}

/// Go `number()`。
fn num(args: &Value, key: &str) -> i64 {
    number_value(args.get(key).unwrap_or(&Value::Null))
}

fn number_value(v: &Value) -> i64 {
    match v.as_f64() {
        Some(f) if f.is_finite() => {
            if f >= 9.223_372_036_854_776e18 {
                i64::MAX
            } else if f <= -9.223_372_036_854_776e18 {
                i64::MIN
            } else {
                f as i64
            }
        }
        _ => 0,
    }
}

// ── cron（internal/orch 的 5 字段解析器）────────────────────────────────────

const FIELD_RANGES: [(i64, i64); 5] = [(0, 59), (0, 23), (1, 31), (1, 12), (0, 6)];
const MAX_SCAN_MINUTES: usize = 366 * 24 * 60;

#[derive(Clone, Copy, Debug, PartialEq)]
struct CronFields {
    minute: [bool; 60],
    hour: [bool; 24],
    day_of_month: [bool; 32],
    month: [bool; 13],
    day_of_week: [bool; 7],
}

impl CronFields {
    fn matches(&self, t: &OffsetDateTime) -> bool {
        let minute = t.minute() as usize;
        let hour = t.hour() as usize;
        let day = t.day() as usize;
        let month = u8::from(t.month()) as usize;
        // Go Weekday()：0 = 周日。
        let weekday = t.weekday().number_days_from_sunday() as usize;
        self.minute[minute]
            && self.hour[hour]
            && self.day_of_month[day]
            && self.month[month]
            && self.day_of_week[weekday]
    }
}

/// Go `fmt.Sscanf("%d", …)` 的近似：跳过前导空白，可选符号 + 数字；数字后
/// 的多余输入不报错（Sscanf 的行为）。
fn sscanf_int(s: &str) -> Option<(i64, &str)> {
    let rest = s.trim_start();
    let (sign, digits_start) = match rest.as_bytes().first() {
        Some(b'-') => (-1i64, 1usize),
        Some(b'+') => (1, 1),
        _ => (1, 0),
    };
    let digits: &str = &rest[digits_start..];
    let end = digits.find(|c: char| !c.is_ascii_digit()).unwrap_or(digits.len());
    if end == 0 {
        return None;
    }
    let value: i64 = digits[..end].parse().ok()?;
    Some((sign * value, &rest[digits_start + end..]))
}

fn parse_field(spec: &str, index: usize) -> Result<Vec<bool>, String> {
    let (min, max) = FIELD_RANGES[index];
    let mut values = vec![false; (max as usize + 1).max(8)];
    for part in spec.split(',') {
        if part.is_empty() {
            return Err(format!("cron 字段无效：{spec}"));
        }
        let (range_raw, step_raw) = match part.find('/') {
            Some(i) => (&part[..i], &part[i + 1..]),
            None => (part, ""),
        };
        let mut step: i64 = 1;
        if !step_raw.is_empty() {
            match sscanf_int(step_raw) {
                Some((v, _)) if v >= 1 => step = v,
                _ => return Err(format!("cron 步长无效：{part}")),
            }
        }
        let (lo, hi) = if range_raw == "*" {
            (min, max)
        } else if range_raw.contains('-') {
            let (a, after) = match sscanf_int(range_raw) {
                Some(v) => v,
                None => return Err(format!("cron 取值无效：{part}（字段范围 {min}-{max}）")),
            };
            let after = match after.strip_prefix('-') {
                Some(v) => v,
                None => return Err(format!("cron 取值无效：{part}（字段范围 {min}-{max}）")),
            };
            let (b, _) = match sscanf_int(after) {
                Some(v) => v,
                None => return Err(format!("cron 取值无效：{part}（字段范围 {min}-{max}）")),
            };
            (a, b)
        } else {
            match sscanf_int(range_raw) {
                Some((v, _)) => (v, v),
                None => return Err(format!("cron 取值无效：{part}（字段范围 {min}-{max}）")),
            }
        };
        if lo < min || hi > max || lo > hi {
            return Err(format!("cron 取值无效：{part}（字段范围 {min}-{max}）"));
        }
        let mut v = lo;
        while v <= hi {
            values[v as usize] = true;
            v += step;
        }
    }
    Ok(values)
}

/// Go `orch.ParseCron`。
pub fn parse_cron(cron: &str) -> Result<CronFields, String> {
    let fields: Vec<&str> = cron.split_whitespace().collect();
    if fields.len() != 5 {
        return Err("cron 需要 5 个字段：分 时 日 月 周".into());
    }
    let sets: Vec<Vec<bool>> = fields
        .iter()
        .enumerate()
        .map(|(index, field)| parse_field(field, index))
        .collect::<Result<_, _>>()?;
    let mut out = CronFields {
        minute: [false; 60],
        hour: [false; 24],
        day_of_month: [false; 32],
        month: [false; 13],
        day_of_week: [false; 7],
    };
    for (i, v) in sets[0].iter().enumerate() {
        if *v && i < 60 {
            out.minute[i] = true;
        }
    }
    for (i, v) in sets[1].iter().enumerate() {
        if *v && i < 24 {
            out.hour[i] = true;
        }
    }
    for (i, v) in sets[2].iter().enumerate() {
        if *v && i < 32 {
            out.day_of_month[i] = true;
        }
    }
    for (i, v) in sets[3].iter().enumerate() {
        if *v && i < 13 {
            out.month[i] = true;
        }
    }
    for (i, v) in sets[4].iter().enumerate() {
        if *v && i < 7 {
            out.day_of_week[i] = true;
        }
    }
    // 7 表示周日（crontab 约定），归并到 0。
    if sets[4].get(7).copied().unwrap_or(false) {
        out.day_of_week[0] = true;
    }
    Ok(out)
}

fn local_now() -> Result<OffsetDateTime, String> {
    OffsetDateTime::now_local().map_err(|e| format!("本地时间不可用：{e}"))
}

/// Go `orch.NextRun`：从 from 起逐分钟前扫（上限一年）。
pub fn next_run(cron: &CronFields, from: OffsetDateTime) -> Result<OffsetDateTime, String> {
    let mut cursor = from
        .replace_second(0)
        .and_then(|t| t.replace_nanosecond(0))
        .map_err(|e| format!("时间处理失败：{e}"))?;
    for _ in 0..MAX_SCAN_MINUTES {
        cursor += time::Duration::minutes(1);
        if cron.matches(&cursor) {
            return Ok(cursor);
        }
    }
    Err("cron 在一年内没有任何匹配时间".into())
}

/// Go `time.RFC3339`（秒精度、本地偏移）。
fn format_rfc3339_seconds(t: OffsetDateTime) -> String {
    const FMT: &[time::format_description::FormatItem<'static>] = format_description!(
        "[year]-[month]-[day]T[hour]:[minute]:[second][offset_hour sign:mandatory]:[offset_minute]"
    );
    t.format(&FMT).unwrap_or_default()
}

/// Go `iso(ms)`：RFC3339 秒精度（本地时区）；0 → null。
fn iso_ms(ms: i64) -> Option<String> {
    if ms == 0 {
        return None;
    }
    let dt = OffsetDateTime::from_unix_timestamp_nanos(ms as i128 * 1_000_000).ok()?;
    let offset = local_now().ok()?.offset();
    Some(format_rfc3339_seconds(dt.to_offset(offset)))
}

fn now_ms() -> i64 {
    OffsetDateTime::now_utc().unix_timestamp_nanos() as i64 / 1_000_000
}

// ── 存储模型 ────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Default)]
struct AutomationConfig {
    action: Option<String>,
    /// 原样保存的提示词块（Go []Row，map 键序在 marshal 时排序）。
    prompt_blocks: Value,
    display_text: String,
    mode_id: Option<String>,
    /// Go map[string]string：marshal 时按键排序。
    config_values: Value,
    label_snapshot: Value,
}

#[derive(Debug, Clone)]
struct Automation {
    id: i64,
    name: String,
    enabled: bool,
    trigger_kind: String,
    cron: Option<String>,
    timezone: String,
    next_run_at: Option<String>,
    agent_type: String,
    root_folder_id: Option<i64>,
    isolation: String,
    branch: Option<String>,
    is_remote_branch: bool,
    config: Option<AutomationConfig>,
    last_run_at: Option<String>,
    last_run_status: Option<String>,
    last_run_conversation_id: Option<i64>,
    unseen_failures: i64,
    created_at: String,
    updated_at: String,
    // json:"-"：不落盘、不回线。
    next_run_ms: i64,
    last_minute_key: i64,
}

impl Default for Automation {
    fn default() -> Self {
        Self {
            id: 0,
            name: String::new(),
            enabled: false,
            trigger_kind: String::new(),
            cron: None,
            timezone: String::new(),
            next_run_at: None,
            agent_type: String::new(),
            root_folder_id: None,
            isolation: String::new(),
            branch: None,
            is_remote_branch: false,
            config: None,
            last_run_at: None,
            last_run_status: None,
            last_run_conversation_id: None,
            unseen_failures: 0,
            created_at: String::new(),
            updated_at: String::new(),
            next_run_ms: 0,
            last_minute_key: 0,
        }
    }
}

impl Automation {
    /// Go `wire()`：刷新派生的展示字段。
    fn wire(&mut self) {
        self.next_run_at = iso_ms(self.next_run_ms);
    }
}

#[derive(Debug, Clone)]
struct AutomationRun {
    id: i64,
    automation_id: i64,
    status: String,
    trigger: String,
    scheduled_for: Option<String>,
    started_at: Option<String>,
    ended_at: Option<String>,
    conversation_id: Option<i64>,
    worktree_folder_id: Option<i64>,
    stop_reason: Option<String>,
    error: Option<String>,
    summary: Option<String>,
    created_at: String,
    // json:"-"
    connection_id: String,
}

const MAX_RUNS_PER_AUTOMATION: usize = 100;
const MAX_RUN_DURATION: Duration = Duration::from_secs(24 * 60 * 60);

struct AutoState {
    root: PathBuf,
    next_id: i64,
    next_run_id: i64,
    automations: Vec<Automation>,
    runs: Vec<AutomationRun>,
}

fn states() -> &'static Mutex<HashMap<PathBuf, Arc<Mutex<AutoState>>>> {
    static STATES: OnceLock<Mutex<HashMap<PathBuf, Arc<Mutex<AutoState>>>>> = OnceLock::new();
    STATES.get_or_init(|| Mutex::new(HashMap::new()))
}

fn state(root: &Path) -> Arc<Mutex<AutoState>> {
    let key = root.to_path_buf();
    let mut map = states().lock().unwrap();
    map.entry(key)
        .or_insert_with(|| {
            Arc::new(Mutex::new(AutoState {
                root: root.to_path_buf(),
                next_id: 1,
                next_run_id: 1,
                automations: Vec::new(),
                runs: Vec::new(),
            }))
        })
        .clone()
}

impl AutoState {
    fn automations_file(&self) -> PathBuf {
        self.root.join(".data").join("codeg-automations.json")
    }
    fn runs_file(&self) -> PathBuf {
        self.root.join(".data").join("codeg-automation-runs.json")
    }

    fn load(&mut self) {
        if let Ok(Some(Value::Array(items))) = store_file::read_json(&self.automations_file()) {
            let mut loaded = Vec::new();
            for item in items {
                if let Some(a) = automation_from_value(&item) {
                    if a.id >= self.next_id {
                        self.next_id = a.id + 1;
                    }
                    loaded.push(a);
                }
            }
            self.automations = loaded;
        }
        if let Ok(Some(Value::Array(items))) = store_file::read_json(&self.runs_file()) {
            let mut loaded = Vec::new();
            for item in items {
                if let Some(r) = run_from_value(&item) {
                    if r.id >= self.next_run_id {
                        self.next_run_id = r.id + 1;
                    }
                    loaded.push(r);
                }
            }
            self.runs = loaded;
        }
    }

    /// Go `autoStore.save`：原子写两个文件（marshal 用 struct 字段序）。
    fn save(&self) {
        let automations = Value::Array(self.automations.iter().map(automation_value).collect());
        let runs = Value::Array(self.runs.iter().map(run_value).collect());
        let _ = store_file::write_bytes(
            &self.automations_file(),
            &goformat::marshal_preserve(&automations),
        );
        let _ = store_file::write_bytes(&self.runs_file(), &goformat::marshal_preserve(&runs));
    }

    fn find(&mut self, id: i64) -> Option<&mut Automation> {
        self.automations.iter_mut().find(|a| a.id == id)
    }

    /// Go `runsFor`：倒序取该任务的最近 limit 条。
    fn runs_for(&self, id: i64, limit: usize) -> Vec<Value> {
        let mut out = Vec::new();
        for run in self.runs.iter().rev() {
            if out.len() >= limit {
                break;
            }
            if run.automation_id == id {
                out.push(run_value(run));
            }
        }
        out
    }

    fn has_active_run(&self, automation_id: i64) -> bool {
        self.runs
            .iter()
            .rev()
            .any(|r| r.automation_id == automation_id && r.status == "running")
    }

    /// Go `appendRun`：追加并把每个任务的历史封顶 100 条。
    fn append_run(&mut self, run: AutomationRun) {
        self.runs.push(run);
        let mut counts: HashMap<i64, usize> = HashMap::new();
        let mut kept: Vec<AutomationRun> = Vec::new();
        for r in self.runs.iter().rev() {
            let count = counts.entry(r.automation_id).or_insert(0);
            *count += 1;
            if *count <= MAX_RUNS_PER_AUTOMATION {
                kept.push(r.clone());
            }
        }
        kept.reverse();
        self.runs = kept;
    }

    /// Go `startRun`：建 run 记录（含 appendRun），刷新任务的最近运行字段。
    /// 调度触发时若功能被关或上一轮仍在跑则记为 skipped。
    fn start_run(&mut self, item_id: i64, trigger: &str) -> AutomationRun {
        let run_id = self.next_run_id;
        self.next_run_id += 1;
        let mut run = AutomationRun {
            id: run_id,
            automation_id: item_id,
            status: "running".into(),
            trigger: trigger.to_string(),
            scheduled_for: None,
            started_at: None,
            ended_at: None,
            conversation_id: None,
            worktree_folder_id: None,
            stop_reason: None,
            error: None,
            summary: None,
            created_at: goformat::timestamp(),
            connection_id: String::new(),
        };
        if !automations_enabled(&self.root) {
            run.status = "skipped".into();
            run.stop_reason = Some("automations disabled".into());
            run.ended_at = Some(goformat::timestamp());
        } else if self.has_active_run(item_id) {
            run.status = "skipped".into();
            run.stop_reason = Some("previous run still active".into());
            run.ended_at = Some(goformat::timestamp());
        } else {
            run.started_at = Some(goformat::timestamp());
        }
        let status = run.status.clone();
        self.append_run(run);
        let now = goformat::timestamp();
        if let Some(item) = self.find(item_id) {
            item.last_run_at = Some(now);
            item.last_run_status = Some(status.clone());
        }
        AutomationRun {
            id: run_id,
            automation_id: item_id,
            status,
            trigger: trigger.to_string(),
            scheduled_for: None,
            started_at: None,
            ended_at: None,
            conversation_id: None,
            worktree_folder_id: None,
            stop_reason: None,
            error: None,
            summary: None,
            created_at: String::new(),
            connection_id: String::new(),
        }
    }
}

// ── wire 序列化（struct 字段序；map 嵌套值按 Go marshal 排序）───────────────

fn option_string(value: &Option<String>) -> Value {
    value.clone().map(Value::String).unwrap_or(Value::Null)
}

fn option_i64(value: &Option<i64>) -> Value {
    value.map(|v| json!(v)).unwrap_or(Value::Null)
}

fn config_value(config: &AutomationConfig) -> Value {
    let mut out = Map::new();
    // Go omitempty：nil 省略。
    if let Some(action) = &config.action {
        out.insert("action".into(), json!(action));
    }
    out.insert("prompt_blocks".into(), goformat::canon(&config.prompt_blocks));
    out.insert("display_text".into(), json!(config.display_text));
    out.insert("mode_id".into(), option_string(&config.mode_id));
    out.insert("config_values".into(), goformat::canon(&config.config_values));
    // label_snapshot 带 omitempty：空对象/空值省略。
    match &config.label_snapshot {
        Value::Null => {}
        Value::Object(map) if map.is_empty() => {}
        other => {
            out.insert("label_snapshot".into(), goformat::canon(other));
        }
    }
    Value::Object(out)
}

fn automation_value(a: &Automation) -> Value {
    let mut out = Map::new();
    out.insert("id".into(), json!(a.id));
    out.insert("name".into(), json!(a.name));
    out.insert("enabled".into(), json!(a.enabled));
    out.insert("trigger_kind".into(), json!(a.trigger_kind));
    out.insert("cron".into(), option_string(&a.cron));
    out.insert("timezone".into(), json!(a.timezone));
    out.insert("next_run_at".into(), option_string(&a.next_run_at));
    out.insert("agent_type".into(), json!(a.agent_type));
    out.insert("root_folder_id".into(), option_i64(&a.root_folder_id));
    out.insert("isolation".into(), json!(a.isolation));
    out.insert("branch".into(), option_string(&a.branch));
    out.insert("is_remote_branch".into(), json!(a.is_remote_branch));
    out.insert(
        "config".into(),
        match &a.config {
            Some(config) => config_value(config),
            None => Value::Null,
        },
    );
    out.insert("last_run_at".into(), option_string(&a.last_run_at));
    out.insert("last_run_status".into(), option_string(&a.last_run_status));
    out.insert("last_run_conversation_id".into(), option_i64(&a.last_run_conversation_id));
    out.insert("unseen_failures".into(), json!(a.unseen_failures));
    out.insert("created_at".into(), json!(a.created_at));
    out.insert("updated_at".into(), json!(a.updated_at));
    Value::Object(out)
}

fn run_value(r: &AutomationRun) -> Value {
    let mut out = Map::new();
    out.insert("id".into(), json!(r.id));
    out.insert("automation_id".into(), json!(r.automation_id));
    out.insert("status".into(), json!(r.status));
    out.insert("trigger".into(), json!(r.trigger));
    out.insert("scheduled_for".into(), option_string(&r.scheduled_for));
    out.insert("started_at".into(), option_string(&r.started_at));
    out.insert("ended_at".into(), option_string(&r.ended_at));
    out.insert("conversation_id".into(), option_i64(&r.conversation_id));
    out.insert("worktree_folder_id".into(), option_i64(&r.worktree_folder_id));
    out.insert("stop_reason".into(), option_string(&r.stop_reason));
    out.insert("error".into(), option_string(&r.error));
    out.insert("summary".into(), option_string(&r.summary));
    out.insert("created_at".into(), json!(r.created_at));
    Value::Object(out)
}

// ── 反序列化（文件读取：Go json.Unmarshal → 零值缺省）───────────────────────

fn automation_from_value(v: &Value) -> Option<Automation> {
    let obj = v.as_object()?;
    let config = obj.get("config").and_then(|c| {
        c.as_object().map(|raw| AutomationConfig {
            action: raw.get("action").and_then(|s| s.as_str()).map(|s| s.to_string()),
            prompt_blocks: raw.get("prompt_blocks").cloned().unwrap_or(Value::Null),
            display_text: raw.get("display_text").and_then(|s| s.as_str()).unwrap_or("").into(),
            mode_id: raw.get("mode_id").and_then(|s| s.as_str()).map(|s| s.to_string()),
            config_values: raw.get("config_values").cloned().unwrap_or(json!({})),
            label_snapshot: raw.get("label_snapshot").cloned().unwrap_or(Value::Null),
        })
    });
    Some(Automation {
        id: obj.get("id").and_then(|n| n.as_i64()).unwrap_or(0),
        name: obj.get("name").and_then(|s| s.as_str()).unwrap_or("").into(),
        enabled: obj.get("enabled").and_then(|b| b.as_bool()).unwrap_or(false),
        trigger_kind: obj.get("trigger_kind").and_then(|s| s.as_str()).unwrap_or("").into(),
        cron: obj.get("cron").and_then(|s| s.as_str()).map(|s| s.to_string()),
        timezone: obj.get("timezone").and_then(|s| s.as_str()).unwrap_or("").into(),
        next_run_at: obj.get("next_run_at").and_then(|s| s.as_str()).map(|s| s.to_string()),
        agent_type: obj.get("agent_type").and_then(|s| s.as_str()).unwrap_or("").into(),
        root_folder_id: obj.get("root_folder_id").and_then(|n| n.as_i64()),
        isolation: obj.get("isolation").and_then(|s| s.as_str()).unwrap_or("").into(),
        branch: obj.get("branch").and_then(|s| s.as_str()).map(|s| s.to_string()),
        is_remote_branch: obj.get("is_remote_branch").and_then(|b| b.as_bool()).unwrap_or(false),
        config,
        last_run_at: obj.get("last_run_at").and_then(|s| s.as_str()).map(|s| s.to_string()),
        last_run_status: obj
            .get("last_run_status")
            .and_then(|s| s.as_str())
            .map(|s| s.to_string()),
        last_run_conversation_id: obj.get("last_run_conversation_id").and_then(|n| n.as_i64()),
        unseen_failures: obj.get("unseen_failures").and_then(|n| n.as_i64()).unwrap_or(0),
        created_at: obj.get("created_at").and_then(|s| s.as_str()).unwrap_or("").into(),
        updated_at: obj.get("updated_at").and_then(|s| s.as_str()).unwrap_or("").into(),
        next_run_ms: 0,
        last_minute_key: 0,
    })
}

fn run_from_value(v: &Value) -> Option<AutomationRun> {
    let obj = v.as_object()?;
    Some(AutomationRun {
        id: obj.get("id").and_then(|n| n.as_i64()).unwrap_or(0),
        automation_id: obj.get("automation_id").and_then(|n| n.as_i64()).unwrap_or(0),
        status: obj.get("status").and_then(|s| s.as_str()).unwrap_or("").into(),
        trigger: obj.get("trigger").and_then(|s| s.as_str()).unwrap_or("").into(),
        scheduled_for: obj.get("scheduled_for").and_then(|s| s.as_str()).map(|s| s.to_string()),
        started_at: obj.get("started_at").and_then(|s| s.as_str()).map(|s| s.to_string()),
        ended_at: obj.get("ended_at").and_then(|s| s.as_str()).map(|s| s.to_string()),
        conversation_id: obj.get("conversation_id").and_then(|n| n.as_i64()),
        worktree_folder_id: obj.get("worktree_folder_id").and_then(|n| n.as_i64()),
        stop_reason: obj.get("stop_reason").and_then(|s| s.as_str()).map(|s| s.to_string()),
        error: obj.get("error").and_then(|s| s.as_str()).map(|s| s.to_string()),
        summary: obj.get("summary").and_then(|s| s.as_str()).map(|s| s.to_string()),
        created_at: obj.get("created_at").and_then(|s| s.as_str()).unwrap_or("").into(),
        connection_id: String::new(),
    })
}

// ── Go fmt.Sprint 的近似（config_values 的值转换）───────────────────────────

fn go_sprint(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Bool(b) => b.to_string(),
        Value::Null => "<nil>".into(),
        Value::Number(n) => {
            let f = n.as_f64().unwrap_or(0.0);
            if f.fract() == 0.0 && f.abs() < 1e15 && f.is_finite() {
                format!("{}", f as i64)
            } else {
                format!("{f}")
            }
        }
        Value::Array(items) => {
            let inner: Vec<String> = items.iter().map(go_sprint).collect();
            format!("[{}]", inner.join(" "))
        }
        Value::Object(map) => {
            let mut keys: Vec<&String> = map.keys().collect();
            keys.sort();
            let inner: Vec<String> = keys
                .iter()
                .map(|k| format!("{}:{}", k, go_sprint(&map[*k])))
                .collect();
            format!("map[{}]", inner.join(" "))
        }
    }
}

// ── parseDraft ──────────────────────────────────────────────────────────────

fn parse_draft(a: &mut Automation, draft: &Value, creating: bool) -> Result<(), String> {
    let name = value_text(draft.get("name").unwrap_or(&Value::Null)).trim().to_string();
    if name.is_empty() {
        return Err("请填写定时任务名称".into());
    }
    a.name = name;
    a.enabled = draft.get("enabled").and_then(|v| v.as_bool()).unwrap_or(true);
    let mut trigger = txt(draft, "trigger_kind");
    if trigger.is_empty() {
        trigger = "schedule".into();
    }
    if trigger != "schedule" && trigger != "manual" {
        return Err(format!("未知的触发类型：{trigger}"));
    }
    a.trigger_kind = trigger;
    a.timezone = txt(draft, "timezone");
    let agent_type = txt(draft, "agent_type");
    if agent_type.is_empty() {
        return Err("请选择执行端".into());
    }
    a.agent_type = agent_type;
    let mut isolation = txt(draft, "isolation");
    if isolation.is_empty() {
        isolation = "shared_in_root".into();
    }
    if isolation != "shared_in_root" && isolation != "worktree_per_run" {
        return Err(format!("未知的隔离方式：{isolation}"));
    }
    a.isolation = isolation;
    if a.isolation == "worktree_per_run" {
        return Err("worktree 隔离暂未接通，请使用 shared_in_root".into());
    }
    if let Some(v) = draft.get("root_folder_id") {
        a.root_folder_id = if v.is_null() { None } else { Some(number_value(v)) };
    } else if creating {
        a.root_folder_id = None;
    }
    match draft.get("branch").and_then(|v| v.as_str()) {
        Some(raw) if !raw.is_empty() => {
            a.branch = Some(raw.to_string());
            a.is_remote_branch = draft
                .get("is_remote_branch")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
        }
        _ => {
            if creating {
                a.branch = None;
                a.is_remote_branch = false;
            }
        }
    }
    if let Some(raw) = draft.get("cron") {
        if raw.is_null() || value_text(raw).is_empty() {
            a.cron = None;
            a.next_run_ms = 0;
        } else {
            let cron = value_text(raw).trim().to_string();
            let parsed = parse_cron(&cron)?;
            let now = local_now()?;
            let next = next_run(&parsed, now)?;
            a.cron = Some(cron);
            a.next_run_ms = (next.unix_timestamp_nanos() / 1_000_000) as i64;
        }
    }
    if a.trigger_kind == "schedule" && a.cron.is_none() {
        return Err("计划触发的定时任务需要填写 cron".into());
    }
    if let Some(raw) = draft.get("config").filter(|c| c.is_object()) {
        let mut config = AutomationConfig {
            action: None,
            prompt_blocks: Value::Array(Vec::new()),
            display_text: String::new(),
            mode_id: None,
            config_values: json!({}),
            label_snapshot: Value::Null,
        };
        if let Some(blocks) = raw.get("prompt_blocks").and_then(|b| b.as_array()) {
            let kept: Vec<Value> = blocks.iter().filter(|b| b.is_object()).cloned().collect();
            config.prompt_blocks = Value::Array(kept);
        }
        config.display_text = txt(raw, "display_text");
        if let Some(v) = raw.get("mode_id").and_then(|v| v.as_str()) {
            if !v.is_empty() {
                config.mode_id = Some(v.to_string());
            }
        }
        if let Some(values) = raw.get("config_values").and_then(|v| v.as_object()) {
            let mut converted = Map::new();
            for (k, v) in values {
                converted.insert(k.clone(), json!(go_sprint(v)));
            }
            config.config_values = Value::Object(converted);
        }
        if let Some(label) = raw.get("label_snapshot").filter(|l| l.is_object()) {
            config.label_snapshot = label.clone();
        }
        if let Some(action) = raw.get("action").and_then(|v| v.as_str()) {
            if !action.is_empty() {
                config.action = Some(action.to_string());
            }
        }
        a.config = Some(config);
    }
    if creating && a.config.is_none() {
        return Err("请填写任务提示词".into());
    }
    Ok(())
}

// ── Go HTTP 桥接（ACP 仍在 Go 侧时的运行执行通道）───────────────────────────

#[derive(Clone)]
pub(crate) struct Bridge {
    pub client: reqwest::Client,
    /// 前门自身（Rust 已接管的命令本地应答，其余转发 Go）。
    pub self_http: String,
    pub root: PathBuf,
}

static BRIDGE: OnceLock<Mutex<Option<Arc<Bridge>>>> = OnceLock::new();

/// mod.rs 在 serve() 时登记桥接（进程级单例：单工作区应用）。
pub(crate) fn set_bridge(bridge: Bridge) {
    BRIDGE
        .get_or_init(|| Mutex::new(None))
        .lock()
        .unwrap()
        .replace(Arc::new(bridge));
}

fn bridge() -> Option<Arc<Bridge>> {
    BRIDGE.get_or_init(|| Mutex::new(None)).lock().unwrap().clone()
}

impl Bridge {
    /// POST 一条命令并取回结果；409（turn_in_progress）按 Go 内部错误文案
    /// 还原，4xx/5xx 取 envelope 的 message。reqwest 未开 json feature，
    /// 手动编解码。
    async fn call(&self, command: &str, args: Value) -> Result<Value, String> {
        let url = format!("{}/api/{command}", self.self_http);
        let body = serde_json::to_vec(&args).map_err(|e| e.to_string())?;
        let response = self
            .client
            .post(url)
            .header("Content-Type", "application/json")
            .body(body)
            .send()
            .await
            .map_err(|e| e.to_string())?;
        let status = response.status().as_u16();
        let bytes = response.bytes().await.map_err(|e| e.to_string())?;
        let parsed: Value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
        if status == 409 {
            return Err("turn_in_progress".into());
        }
        if status >= 400 {
            return Err(parsed
                .get("message")
                .and_then(|m| m.as_str())
                .unwrap_or("命令执行失败")
                .to_string());
        }
        Ok(parsed)
    }
}

/// 读取工作区 JSON 的小助手（只读；写入走 store_writes 的跨进程锁）。
fn workspace_doc(root: &Path) -> Option<Value> {
    store_file::read_json(&root.join(".data").join("codeg-workspace.json")).ok()?
}

fn automations_enabled(root: &Path) -> bool {
    workspace_doc(root)
        .and_then(|doc| {
            doc.get("settings")?
                .get("automations_enabled")
                .and_then(|v| v.as_bool())
        })
        .unwrap_or(true)
}

/// Go `finishAutomationRun` 的第一步：解析任务绑定的文件夹。
fn folder_for(root: &Path, root_folder_id: Option<i64>) -> Result<(i64, String), String> {
    let Some(id) = root_folder_id else {
        return Ok((0, root.to_string_lossy().to_string()));
    };
    let doc = workspace_doc(root).ok_or("项目文件夹不存在")?;
    let folder = doc
        .get("folders")
        .and_then(|f| f.as_array())
        .and_then(|list| {
            list.iter()
                .find(|f| f.get("id").and_then(|n| n.as_i64()) == Some(id))
                .cloned()
        })
        .ok_or("项目文件夹不存在")?;
    let path = folder.get("path").and_then(|p| p.as_str()).unwrap_or("").to_string();
    Ok((id, path))
}

/// 运行提示词（Go `runPrompt`）。
fn run_prompt(config: Option<&AutomationConfig>) -> String {
    let Some(config) = config else { return String::new() };
    if !config.display_text.is_empty() {
        return config.display_text.clone();
    }
    let mut out = String::new();
    if let Some(blocks) = config.prompt_blocks.as_array() {
        for block in blocks {
            out.push_str(block.get("text").and_then(|t| t.as_str()).unwrap_or(""));
        }
    }
    out
}

/// Go：`title[:80]` 按字节截断，json.Marshal 再把非法字节换成 U+FFFD。
fn truncate_bytes_go(s: &str, limit: usize) -> String {
    if s.len() <= limit {
        return s.to_string();
    }
    crate::backend::terminal::go_lossy_bytes(&s.as_bytes()[..limit])
}

// ── 命令分发（automations.go:automationCommand）─────────────────────────────

fn run(
    root: &Path,
    command: &str,
    args: &Value,
    runtime: &tokio::runtime::Handle,
) -> Result<Value, String> {
    let state = state(root);
    let mut st = state.lock().unwrap();
    match command {
        "automation_list" => {
            for a in &mut st.automations {
                a.wire();
            }
            let items: Vec<Value> = st.automations.iter().map(automation_value).collect();
            Ok(Value::Array(items))
        }
        "automation_get" => {
            let id = num(args, "id");
            let value;
            {
                let item = st.find(id).ok_or("定时任务不存在")?;
                item.wire();
                value = automation_value(item);
            }
            Ok(value)
        }
        "automation_runs" => {
            let mut limit = num(args, "limit");
            if limit <= 0 || limit > 500 {
                limit = 100;
            }
            let id = num(args, "automationId");
            Ok(Value::Array(st.runs_for(id, limit as usize)))
        }
        "automation_create" => {
            let draft = args
                .get("draft")
                .filter(|d| d.is_object())
                .ok_or("draft 无效")?
                .clone();
            let mut item = Automation {
                id: st.next_id,
                created_at: goformat::timestamp(),
                updated_at: goformat::timestamp(),
                ..Automation::default()
            };
            parse_draft(&mut item, &draft, true)?;
            st.next_id += 1;
            let id = item.id;
            st.automations.push(item);
            st.save();
            emit_event("automation://changed", json!({"kind": "created", "id": id}));
            let value;
            {
                let item = st.find(id).expect("刚插入的任务");
                item.wire();
                value = automation_value(item);
            }
            Ok(value)
        }
        "automation_update" => {
            let id = num(args, "id");
            let draft = args.get("draft").cloned().unwrap_or(json!({}));
            {
                let item = st.find(id).ok_or("定时任务不存在")?;
                parse_draft(item, &draft, false)?;
                item.updated_at = goformat::timestamp();
            }
            st.save();
            emit_event("automation://changed", json!({"kind": "updated", "id": id}));
            let value;
            {
                let item = st.find(id).ok_or("定时任务不存在")?;
                item.wire();
                value = automation_value(item);
            }
            Ok(value)
        }
        "automation_set_enabled" => {
            let id = num(args, "id");
            {
                let item = st.find(id).ok_or("定时任务不存在")?;
                item.enabled = args.get("enabled").and_then(|v| v.as_bool()).unwrap_or(false);
                if item.enabled && item.trigger_kind == "schedule" {
                    if let Some(cron) = item.cron.clone() {
                        if let Ok(parsed) = parse_cron(&cron) {
                            if let Ok(now) = local_now() {
                                if let Ok(next) = next_run(&parsed, now) {
                                    item.next_run_ms = (next.unix_timestamp_nanos() / 1_000_000) as i64;
                                }
                            }
                        }
                    }
                }
                item.updated_at = goformat::timestamp();
            }
            st.save();
            emit_event("automation://changed", json!({"kind": "updated", "id": id}));
            let value;
            {
                let item = st.find(id).ok_or("定时任务不存在")?;
                item.wire();
                value = automation_value(item);
            }
            Ok(value)
        }
        "automation_delete" => {
            let id = num(args, "id");
            let found = st.automations.iter().any(|a| a.id == id);
            st.automations.retain(|a| a.id != id);
            st.runs.retain(|r| r.automation_id != id);
            if !found {
                return Err("定时任务不存在".into());
            }
            st.save();
            emit_event("automation://changed", json!({"kind": "deleted", "id": id}));
            Ok(Value::Null)
        }
        "automation_mark_seen" => {
            for item in &mut st.automations {
                item.unseen_failures = 0;
            }
            st.save();
            Ok(Value::Null)
        }
        "automation_compute_next_run" => {
            let cron = txt(args, "cron").trim().to_string();
            let parsed = parse_cron(&cron)?;
            let now = local_now()?;
            match next_run(&parsed, now) {
                Ok(next) => Ok(json!(format_rfc3339_seconds(next))),
                Err(_) => Ok(Value::Null),
            }
        }
        "automation_run_now" => {
            let id = num(args, "automationId");
            let run;
            {
                st.find(id).ok_or("定时任务不存在")?;
                if st.has_active_run(id) {
                    return Err("上一次运行尚未结束".into());
                }
                run = st.start_run(id, "manual");
                st.save();
                let item = st.find(id).expect("刚校验过的任务");
                item.wire();
            }
            emit_event(
                "automation://changed",
                json!({"kind": "run_started", "id": id, "runId": run.id}),
            );
            if run.status != "skipped" {
                spawn_finish(runtime, root, id, run.id);
            }
            Ok(json!(run.id))
        }
        "automation_cancel_run" => {
            let run_id = num(args, "runId");
            let mut connection_id = String::new();
            let mut automation_id = 0i64;
            {
                let target = st
                    .runs
                    .iter_mut()
                    .rev()
                    .find(|r| r.id == run_id)
                    .ok_or("运行记录不存在")?;
                if target.status != "running" {
                    return Ok(Value::Null);
                }
                connection_id = target.connection_id.clone();
                automation_id = target.automation_id;
                target.status = "cancelled".into();
                target.ended_at = Some(goformat::timestamp());
                target.stop_reason = Some("cancelled by user".into());
            }
            if !connection_id.is_empty() {
                if let Some(bridge) = bridge() {
                    // Go 忽略取消失败（`_ = s.cancel`）；锁内不能 await，
                    // 交给独立任务。
                    runtime.spawn(async move {
                        let _ = bridge
                            .call("acp_cancel", json!({"connectionId": connection_id}))
                            .await;
                    });
                }
            }
            st.save();
            emit_event(
                "automation://changed",
                json!({"kind": "run_updated", "automationId": automation_id, "runId": run_id}),
            );
            Ok(Value::Null)
        }
        _ => Err(format!("未知的定时任务命令：{command}")),
    }
}

fn spawn_finish(
    runtime: &tokio::runtime::Handle,
    root: &Path,
    automation_id: i64,
    run_id: i64,
) {
    let Some(bridge) = bridge() else { return };
    let root = root.to_path_buf();
    runtime.spawn(async move {
        finish_automation_run(bridge, root, automation_id, run_id).await;
    });
}

// ── 调度器与运行执行 ────────────────────────────────────────────────────────

/// mod.rs 在 serve() 时调用：30 秒一轮，等价 Go 的 TickAutomations。
pub(crate) fn spawn_scheduler(bridge: Bridge) {
    set_bridge(bridge);
    tokio::spawn(async move {
        loop {
            tokio::time::sleep(Duration::from_secs(30)).await;
            tick().await;
        }
    });
}

async fn tick() {
    let Some(bridge) = bridge() else { return };
    let root = bridge.root.clone();
    let state = state(&root);
    let minute_key = OffsetDateTime::now_utc().unix_timestamp() / 60;
    let items: Vec<i64> = {
        let st = state.lock().unwrap();
        st.automations.iter().map(|a| a.id).collect()
    };
    let mut started: Vec<(i64, i64)> = Vec::new();
    for id in items {
        // Go：每条任务一个持锁闭包，判定 + 起跑在一个临界区里。
        let outcome = {
            let mut st = state.lock().unwrap();
            let now = match local_now() {
                Ok(n) => n,
                Err(err) => {
                    if let Some(item) = st.find(id) {
                        item.last_run_status = Some(format!("error: {err}"));
                    }
                    continue;
                }
            };
            let Some(item) = st.find(id) else { continue };
            if !item.enabled || item.trigger_kind != "schedule" {
                continue;
            }
            let Some(cron) = item.cron.clone() else { continue };
            let parsed = match parse_cron(&cron) {
                Ok(p) => p,
                Err(err) => {
                    item.last_run_status = Some(format!("error: {err}"));
                    continue;
                }
            };
            if item.next_run_ms == 0 {
                let next = match next_run(&parsed, now) {
                    Ok(n) => n,
                    Err(err) => {
                        item.last_run_status = Some(format!("error: {err}"));
                        continue;
                    }
                };
                item.next_run_ms = (next.unix_timestamp_nanos() / 1_000_000) as i64;
            }
            if now_ms() < item.next_run_ms {
                continue;
            }
            let next = match next_run(&parsed, now) {
                Ok(n) => n,
                Err(err) => {
                    item.last_run_status = Some(format!("error: {err}"));
                    continue;
                }
            };
            item.next_run_ms = (next.unix_timestamp_nanos() / 1_000_000) as i64;
            if item.last_minute_key == minute_key {
                continue;
            }
            item.last_minute_key = minute_key;
            if st.has_active_run(id) {
                continue;
            }
            let run = st.start_run(id, "schedule");
            st.save();
            if let Some(item) = st.find(id) {
                item.wire();
            }
            if run.status == "running" {
                Some(run.id)
            } else {
                None
            }
        };
        if let Some(run_id) = outcome {
            started.push((id, run_id));
        }
    }
    // Go：先 go finish 再 emit；这里逐条 spawn（并发），emit 保持同样的
    // run_started 事件形状。
    for (id, run_id) in started {
        emit_event(
            "automation://changed",
            json!({"kind": "run_started", "id": id, "runId": run_id}),
        );
        let bridge = bridge.clone();
        let root = root.clone();
        tokio::spawn(async move {
            finish_automation_run(bridge, root, id, run_id).await;
        });
    }
}

/// Go `completeAutomationRun`。
fn complete_automation_run(root: &Path, automation_id: i64, run_id: i64, status: &str, err_msg: &str) {
    let state = state(root);
    let mut st = state.lock().unwrap();
    let item_exists = st.automations.iter().any(|a| a.id == automation_id);
    let mut run_found = false;
    for r in &mut st.runs {
        if r.id == run_id {
            run_found = true;
            r.status = status.to_string();
            r.ended_at = Some(goformat::timestamp());
            if !err_msg.is_empty() {
                r.error = Some(err_msg.to_string());
            }
        }
    }
    if item_exists {
        for a in &mut st.automations {
            if a.id == automation_id {
                a.last_run_status = Some(status.to_string());
                if status == "failed" {
                    a.unseen_failures += 1;
                }
            }
        }
    }
    if run_found {
        st.save();
    }
    if run_found && item_exists {
        emit_event(
            "automation://changed",
            json!({"kind": "run_finished", "id": automation_id, "runId": run_id, "status": status}),
        );
    }
}

/// Go `autoTouch`：后台变更后刷新派生字段。
fn auto_touch(root: &Path, id: i64) {
    let state = state(root);
    let mut st = state.lock().unwrap();
    if let Some(item) = st.find(id) {
        item.wire();
    }
}

/// Go `finishAutomationRun`：连接 → 建会话 → 发提示词 → 轮询会话状态。
async fn finish_automation_run(bridge: Arc<Bridge>, root: PathBuf, automation_id: i64, run_id: i64) {
    let snapshot = {
        let st = state(&root);
        let st = st.lock().unwrap();
        st.automations
            .iter()
            .find(|a| a.id == automation_id)
            .map(|a| (a.agent_type.clone(), a.root_folder_id, a.config.clone(), a.name.clone()))
    };
    let Some((agent_type, root_folder_id, config, name)) = snapshot else {
        return;
    };

    // Go 的 defer：无论成败最后都刷一次派生字段。
    let touch_root = root.clone();
    let _guard = Defer::new(move || auto_touch(&touch_root, automation_id));

    let (folder_id, folder_path) = match folder_for(&root, root_folder_id) {
        Ok(pair) => pair,
        Err(message) => {
            complete_automation_run(&root, automation_id, run_id, "failed", &message);
            return;
        }
    };
    let mut connect_args = Map::new();
    connect_args.insert("agentType".into(), json!(agent_type));
    connect_args.insert("workingDir".into(), json!(folder_path));
    if let Some(config) = &config {
        if let Some(mode_id) = &config.mode_id {
            connect_args.insert("preferredModeId".into(), json!(mode_id));
        }
        connect_args.insert("preferredConfigValues".into(), goformat::canon(&config.config_values));
    }
    let cid = match bridge.call("acp_connect", Value::Object(connect_args)).await {
        Ok(Value::String(cid)) if !cid.is_empty() => cid,
        Ok(_) => {
            complete_automation_run(&root, automation_id, run_id, "failed", "连接执行端失败");
            return;
        }
        Err(message) => {
            complete_automation_run(&root, automation_id, run_id, "failed", &message);
            return;
        }
    };
    {
        let st = state(&root);
        let mut st = st.lock().unwrap();
        if let Some(r) = st.runs.iter_mut().find(|r| r.id == run_id) {
            r.connection_id = cid.clone();
        }
        st.save();
    }

    let mut title = name;
    if let Some(config) = &config {
        if !config.display_text.is_empty() {
            title = config.display_text.clone();
        }
    }
    let title = truncate_bytes_go(&title, 80);
    let conv_id: i64 = {
        // Go 直接 createConversationLocked（不 emit、不立即落盘）；Rust 的
        // 工作区状态只有文件，所以经 store_writes 立即持久化并照常发
        // conversation://changed（与手工新建会话的 UI 行为一致）。
        let root_for_store = root.clone();
        let args = json!({"folderId": folder_id, "agentType": agent_type, "title": title});
        let outcome =
            tokio::task::spawn_blocking(move || {
                store_writes::handle(&root_for_store, "create_conversation", &args)
            })
            .await;
        match outcome {
            Ok(Some(outcome)) => match outcome.result {
                Ok(id_value) => id_value.as_i64().unwrap_or(0),
                Err(message) => {
                    let _ = bridge.call("acp_disconnect", json!({"connectionId": cid})).await;
                    complete_automation_run(&root, automation_id, run_id, "failed", &message);
                    return;
                }
            },
            Ok(None) => {
                let _ = bridge.call("acp_disconnect", json!({"connectionId": cid})).await;
                complete_automation_run(&root, automation_id, run_id, "failed", "会话创建不可用");
                return;
            }
            Err(join) => {
                let _ = bridge.call("acp_disconnect", json!({"connectionId": cid})).await;
                complete_automation_run(&root, automation_id, run_id, "failed", &format!("会话创建失败：{join}"));
                return;
            }
        }
    };
    {
        let st = state(&root);
        let mut st = st.lock().unwrap();
        if let Some(r) = st.runs.iter_mut().find(|r| r.id == run_id) {
            r.conversation_id = Some(conv_id);
        }
        if let Some(a) = st.automations.iter_mut().find(|a| a.id == automation_id) {
            a.last_run_conversation_id = Some(conv_id);
        }
        st.save();
    }

    let text = run_prompt(config.as_ref());
    let prompt_args = json!({
        "connectionId": cid,
        "conversationId": conv_id,
        "folderId": folder_id,
        "blocks": [{"type": "text", "text": text}],
    });
    if let Err(message) = bridge.call("acp_prompt", prompt_args).await {
        let _ = bridge.call("acp_disconnect", json!({"connectionId": cid})).await;
        complete_automation_run(&root, automation_id, run_id, "failed", &message);
        return;
    }

    // 对话本身异步推进：轮询会话状态直到结束（24 小时上限）。
    let deadline = std::time::Instant::now() + MAX_RUN_DURATION;
    loop {
        tokio::time::sleep(Duration::from_secs(2)).await;
        let status = tokio::task::spawn_blocking({
            let root = root.clone();
            move || {
                workspace_doc(&root).and_then(|doc| {
                    doc.get("conversations")
                        .and_then(|c| c.as_array())
                        .and_then(|list| {
                            list.iter()
                                .find(|c| c.get("id").and_then(|n| n.as_i64()) == Some(conv_id))
                                .cloned()
                        })
                        .and_then(|c| c.get("status").and_then(|s| s.as_str()).map(|s| s.to_string()))
                })
            }
        })
        .await
        .unwrap_or(None);
        match status.as_deref() {
            Some("pending_review") => {
                let _ = bridge.call("acp_disconnect", json!({"connectionId": cid})).await;
                complete_automation_run(&root, automation_id, run_id, "succeeded", "");
                return;
            }
            Some("cancelled") | Some("failed") => {
                let _ = bridge.call("acp_disconnect", json!({"connectionId": cid})).await;
                complete_automation_run(&root, automation_id, run_id, "failed", "会话执行失败或被取消");
                return;
            }
            _ => {}
        }
        if std::time::Instant::now() > deadline {
            let _ = bridge.call("acp_cancel", json!({"connectionId": cid})).await;
            let _ = bridge.call("acp_disconnect", json!({"connectionId": cid})).await;
            complete_automation_run(&root, automation_id, run_id, "failed", "运行超过 24 小时上限");
            return;
        }
    }
}

/// Go defer 的近似：Drop 时执行收尾。
struct Defer<F: FnMut()> {
    f: F,
}

impl<F: FnMut()> Defer<F> {
    fn new(f: F) -> Self {
        Self { f }
    }
}

impl<F: FnMut()> Drop for Defer<F> {
    fn drop(&mut self) {
        (self.f)();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::fs;

    fn temp_root(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "codepluse-auto-{tag}-{}",
            goformat::hex_id()
        ));
        fs::create_dir_all(dir.join(".data")).unwrap();
        dir
    }

    #[test]
    fn cron_parses_like_orch() {
        // 字段数不对。
        assert_eq!(
            parse_cron("* * *").err().as_deref(),
            Some("cron 需要 5 个字段：分 时 日 月 周")
        );
        // 范围越界。
        assert!(parse_cron("60 * * * *").err().unwrap().contains("cron 取值无效"));
        // 步长非法。
        assert_eq!(
            parse_cron("*/0 * * * *").err().as_deref(),
            Some("cron 步长无效：*/0")
        );
        // "7" 被范围检查拒绝（Go 的字段范围 0-6；json:"-" 解析里的 7→周日
        // 合并分支实际不可达——保留只为结构对齐）。
        assert_eq!(
            parse_cron("0 0 * * 7").err().as_deref(),
            Some("cron 取值无效：7（字段范围 0-6）")
        );
        let parsed = parse_cron("*/15 9-17 * * 1-5").unwrap();
        assert!(parsed.minute[0] && parsed.minute[15] && parsed.minute[45]);
        assert!(parsed.hour[9] && parsed.hour[17] && !parsed.hour[18]);
        assert!(parsed.day_of_week[1] && parsed.day_of_week[5] && !parsed.day_of_week[6]);
    }

    #[test]
    fn next_run_scans_forward() {
        // 每分钟都命中 → 下一分钟。
        let every = parse_cron("* * * * *").unwrap();
        let now = local_now().unwrap();
        let next = next_run(&every, now).unwrap();
        // next = 下一个整分边界（Truncate 到分后 +1 分钟再匹配），now 带
        // 秒内零头时差值向下取整，所以只断言边界与区间。
        assert_eq!(next.second(), 0, "命中下一个整分");
        assert!(next > now, "必须晚于 from");
        assert!(
            (next.unix_timestamp_nanos() - now.unix_timestamp_nanos()) <= 60_000_000_000,
            "一分钟内可达"
        );
        // 一年内无匹配。
        let feb30 = parse_cron("0 0 30 2 *").unwrap();
        assert_eq!(
            next_run(&feb30, now).err().as_deref(),
            Some("cron 在一年内没有任何匹配时间")
        );
    }

    #[test]
    fn lifecycle_round_trip_matches_go_contract() {
        let root = temp_root("lifecycle");
        let runtime = tokio::runtime::Handle::try_current()
            .unwrap_or_else(|_| tokio::runtime::Runtime::new().unwrap().handle().clone());

        // create：缺名称报错文案与 Go 一致。
        let bad = handle(
            &root,
            "automation_create",
            &json!({"draft": {"name": " ", "agent_type": "codex",
                "config": {"prompt_blocks": [], "display_text": "hi", "config_values": {}}}}),
            &runtime,
        )
        .unwrap()
        .unwrap_err();
        assert_eq!(bad, "请填写定时任务名称");

        let draft = json!({
            "name": "晨报",
            "trigger_kind": "schedule",
            "cron": "0 9 * * 1-5",
            "timezone": "Asia/Shanghai",
            "agent_type": "codex",
            "isolation": "shared_in_root",
            "root_folder_id": Value::Null,
            "config": {
                "prompt_blocks": [{"type": "text", "text": "早"}],
                "display_text": " morning ",
                "config_values": {"depth": 3, "flag": true},
                "label_snapshot": {"agent": "Codex"},
            },
        });
        let created = handle(&root, "automation_create", &json!({"draft": draft}), &runtime)
            .unwrap()
            .unwrap();
        assert_eq!(created["id"], json!(1));
        assert_eq!(created["enabled"], json!(true));
        assert_eq!(created["trigger_kind"], json!("schedule"));
        // schedule 草稿在 create 时就算出 next_run_ms，wire 后回线（文件里的
        // next_run_at 仍是 null——save 发生在 wire 之前，与 Go 一致）。
        assert!(created["next_run_at"].as_str().unwrap_or("").contains("T09:00:00"), "{created}");
        // config_values 的值经 fmt.Sprint 转字符串。
        assert_eq!(created["config"]["config_values"]["depth"], json!("3"));
        assert_eq!(created["config"]["config_values"]["flag"], json!("true"));

        // set_enabled false。
        let updated = handle(
            &root,
            "automation_set_enabled",
            &json!({"id": 1, "enabled": false}),
            &runtime,
        )
        .unwrap()
        .unwrap();
        assert_eq!(updated["enabled"], json!(false));

        // runs 为空数组（非 null）。
        let runs = handle(
            &root,
            "automation_runs",
            &json!({"automationId": 1, "limit": 10}),
            &runtime,
        )
        .unwrap()
        .unwrap();
        assert_eq!(runs, json!([]));

        // delete。
        handle(&root, "automation_delete", &json!({"id": 1}), &runtime)
            .unwrap()
            .unwrap();
        let list = handle(&root, "automation_list", &json!({}), &runtime)
            .unwrap()
            .unwrap();
        assert_eq!(list, json!([]));

        // 文件落盘（create 后 delete 前写过两次）。
        let saved = fs::read(root.join(".data").join("codeg-automations.json")).unwrap_or_default();
        let _ = saved;
        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn state_reloads_from_files() {
        let root = temp_root("reload");
        let runtime = tokio::runtime::Runtime::new().unwrap().handle().clone();
        let draft = json!({
            "name": "任务",
            "trigger_kind": "manual",
            "agent_type": "codex",
            "config": {"prompt_blocks": [], "display_text": "go", "config_values": {}},
        });
        handle(&root, "automation_create", &json!({"draft": draft}), &runtime)
            .unwrap()
            .unwrap();

        // 新进程语义：同一个 root 的状态从文件恢复（id 续号）。
        let created = handle(
            &root,
            "automation_create",
            &json!({"draft": json!({
                "name": "任务2", "trigger_kind": "manual", "agent_type": "codex",
                "config": {"prompt_blocks": [], "display_text": "go2", "config_values": {}}})}),
            &runtime,
        )
        .unwrap()
        .unwrap();
        assert_eq!(created["id"], json!(2), "next_id 从文件续号");
        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn schedule_requires_cron_and_valid_isolation() {
        let root = temp_root("guards");
        let runtime = tokio::runtime::Runtime::new().unwrap().handle().clone();
        let err = handle(
            &root,
            "automation_create",
            &json!({"draft": json!({
                "name": "x", "trigger_kind": "schedule", "agent_type": "codex",
                "config": {"prompt_blocks": [], "display_text": "t", "config_values": {}}})}),
            &runtime,
        )
        .unwrap()
        .unwrap_err();
        assert_eq!(err, "计划触发的定时任务需要填写 cron");

        let err = handle(
            &root,
            "automation_create",
            &json!({"draft": json!({
                "name": "x", "trigger_kind": "manual", "agent_type": "codex",
                "isolation": "worktree_per_run",
                "config": {"prompt_blocks": [], "display_text": "t", "config_values": {}}})}),
            &runtime,
        )
        .unwrap()
        .unwrap_err();
        assert_eq!(err, "worktree 隔离暂未接通，请使用 shared_in_root");
        fs::remove_dir_all(root).ok();
    }
}
