use crate::ansi::{graphemes, sanitize_text};
use crate::types::{
    AgyStdinData, PlannerStepTiming, SpeedAnchorPoint, SubagentEntry, SubagentStatus,
    TRANSCRIPT_CURSOR_VERSION, TodoItem, TodoStatus, ToolEntry, ToolStatus, TranscriptCursor,
    TranscriptSummary,
};
use serde::Deserialize;
use serde_json::{Map, Value};
use std::collections::{BTreeSet, HashMap};

#[derive(Debug, Deserialize)]
struct RawToolCall {
    name: Option<String>,
    args: Option<Map<String, Value>>,
}

#[derive(Debug, Deserialize)]
struct RawStep {
    step_index: Option<usize>,
    #[serde(rename = "type")]
    step_type: Option<String>,
    status: Option<String>,
    created_at: Option<String>,
    input_tokens: Option<u64>,
    cache_read_tokens: Option<u64>,
    cache_creation_tokens: Option<u64>,
    output_tokens: Option<u64>,
    thinking: Option<String>,
    content: Option<Value>,
    tool_calls: Option<Vec<RawToolCall>>,
}

fn estimate_json_value_bytes(val: &Value) -> usize {
    match val {
        Value::Null => 4,
        Value::Bool(_) => 4,
        Value::Number(_) => 8,
        Value::String(s) => s.len(),
        Value::Array(items) => items.iter().map(estimate_json_value_bytes).sum(),
        Value::Object(map) => map
            .iter()
            .map(|(k, v)| k.len() + estimate_json_value_bytes(v))
            .sum(),
    }
}

fn estimate_step_tokens(step: &RawStep) -> u64 {
    let mut bytes: usize = step.thinking.as_deref().map_or(0, str::len);
    if let Some(Value::String(s)) = step.content.as_ref() {
        bytes = bytes.saturating_add(s.len());
    }
    if let Some(tcs) = step.tool_calls.as_ref() {
        for tc in tcs {
            bytes = bytes.saturating_add(tc.name.as_deref().map_or(0, str::len));
            if let Some(args) = tc.args.as_ref() {
                for (k, v) in args {
                    bytes = bytes
                        .saturating_add(k.len())
                        .saturating_add(estimate_json_value_bytes(v));
                }
            }
        }
    }
    (bytes / 4) as u64
}

/// Antigravity `transcript.jsonl` often serializes tool argument values as JSON-encoded strings
/// (e.g. `"\"/path/to/file\""`). Unwrap safely so targets display cleanly.
pub fn unwrap_arg_value(raw: Option<&Value>) -> String {
    let Some(val) = raw else {
        return String::new();
    };
    match val {
        Value::Number(n) => n.to_string(),
        Value::Bool(b) => b.to_string(),
        Value::String(s) => {
            let trimmed = s.trim();
            if trimmed.starts_with('"')
                && trimmed.ends_with('"')
                && let Ok(Value::String(decoded)) = serde_json::from_str::<Value>(trimmed)
            {
                return sanitize_text(&decoded);
            }
            sanitize_text(trimmed)
        }
        _ => String::new(),
    }
}

/// Pure Howard Hinnant civil-to-epoch-days conversion for RFC 3339 / ISO 8601 timestamps.
fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let y = year - i64::from(month <= 2);
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let doy = (153 * (month + if month > 2 { -3 } else { 9 }) + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// Parses an ISO 8601 / RFC 3339 timestamp into Unix epoch milliseconds.
pub fn parse_iso_ms(iso: &str) -> Option<i64> {
    let s = iso.trim();
    if s.len() < 19 {
        return None;
    }
    let bytes = s.as_bytes();
    if bytes[4] != b'-' || bytes[7] != b'-' || !matches!(bytes[10], b'T' | b't' | b' ') {
        return None;
    }
    if bytes[13] != b':' || bytes[16] != b':' {
        return None;
    }

    let year: i64 = s[0..4].parse().ok()?;
    let month: i64 = s[5..7].parse().ok()?;
    let day: i64 = s[8..10].parse().ok()?;
    let hour: i64 = s[11..13].parse().ok()?;
    let min: i64 = s[14..16].parse().ok()?;
    let sec: i64 = s[17..19].parse().ok()?;
    if !(1..=12).contains(&month)
        || !(1..=31).contains(&day)
        || !(0..=23).contains(&hour)
        || !(0..=59).contains(&min)
        || !(0..=60).contains(&sec)
    {
        return None;
    }

    let mut rest = &s[19..];
    let mut millis: i64 = 0;
    if let Some(after_dot) = rest.strip_prefix('.') {
        let frac_len = after_dot.bytes().take_while(u8::is_ascii_digit).count();
        let frac_str = &after_dot[..frac_len];
        if !frac_str.is_empty() {
            let mut digits = [b'0'; 3];
            for (idx, b) in frac_str.bytes().take(3).enumerate() {
                digits[idx] = b;
            }
            millis = ((digits[0] - b'0') as i64) * 100
                + ((digits[1] - b'0') as i64) * 10
                + ((digits[2] - b'0') as i64);
        }
        rest = &after_dot[frac_len..];
    }

    let offset_sec: i64 = if rest.is_empty() || rest.eq_ignore_ascii_case("Z") {
        0
    } else if rest.starts_with('+') || rest.starts_with('-') {
        let sign: i64 = if rest.starts_with('-') { -1 } else { 1 };
        let tz = &rest[1..];
        let (tz_h, tz_m) = if let Some((h, m)) = tz.split_once(':') {
            (h.parse::<i64>().ok()?, m.parse::<i64>().ok()?)
        } else if tz.len() == 4 {
            (tz[0..2].parse::<i64>().ok()?, tz[2..4].parse::<i64>().ok()?)
        } else {
            return None;
        };
        sign * (tz_h * 3_600 + tz_m * 60)
    } else {
        return None;
    };

    let days = days_from_civil(year, month, day);
    let epoch_sec = days * 86_400 + hour * 3_600 + min * 60 + sec - offset_sec;
    Some(epoch_sec * 1_000 + millis)
}

fn grapheme_slice(s: &str, max_clusters: usize) -> String {
    graphemes(s)
        .take(max_clusters)
        .collect::<String>()
        .trim_end()
        .to_string()
}

fn file_basename(path_str: &str) -> &str {
    let norm = path_str.trim_end_matches(['/', '\\']);
    norm.rsplit(['/', '\\'])
        .next()
        .filter(|b| !b.is_empty())
        .unwrap_or(path_str)
}

fn summarize_tool_target(name: &str, args: Option<&Map<String, Value>>) -> Option<String> {
    let args = args?;
    let file_arg = ["Path", "TargetFile", "File", "DirectoryPath"]
        .iter()
        .map(|k| unwrap_arg_value(args.get(*k)))
        .find(|v| !v.is_empty());
    if let Some(f) = file_arg {
        return Some(file_basename(&f).to_string());
    }
    if name == "run_command" {
        let summary = unwrap_arg_value(args.get("toolSummary"));
        if !summary.is_empty() {
            return Some(grapheme_slice(&summary, 20));
        }
        let cmd = unwrap_arg_value(args.get("CommandLine"));
        if !cmd.is_empty() {
            let short = cmd.split_whitespace().take(2).collect::<Vec<_>>().join(" ");
            return Some(grapheme_slice(&short, 20));
        }
        return None;
    }
    if name == "search_web" {
        let q = unwrap_arg_value(args.get("query"));
        return (!q.is_empty()).then(|| grapheme_slice(&q, 24));
    }
    if name == "grep_search" || name == "find_by_name" {
        let q = unwrap_arg_value(args.get("Query").or_else(|| args.get("Pattern")));
        return (!q.is_empty()).then(|| grapheme_slice(&q, 24));
    }
    if name == "call_mcp_tool" {
        let server = unwrap_arg_value(args.get("ServerName"));
        let tool = unwrap_arg_value(args.get("ToolName"));
        let combined = match (!server.is_empty(), !tool.is_empty()) {
            (true, true) => format!("{server}:{tool}"),
            (true, false) => server,
            (false, true) => tool,
            (false, false) => String::new(),
        };
        return (!combined.is_empty()).then_some(combined);
    }
    if name == "invoke_subagent" {
        let role = unwrap_arg_value(args.get("Role").or_else(|| args.get("TypeName")));
        return (!role.is_empty()).then(|| grapheme_slice(&role, 26));
    }
    None
}

fn detect_skill_from_path(file_path: &str) -> Option<String> {
    let normalized = file_path.replace('\\', "/");
    let parts: Vec<&str> = normalized.split('/').collect();
    if parts.len() >= 3 && parts[parts.len() - 1].eq_ignore_ascii_case("SKILL.md") {
        let parent_skills = parts[parts.len() - 3];
        let skill_name = parts[parts.len() - 2];
        if parent_skills.eq_ignore_ascii_case("skills") && !skill_name.is_empty() {
            let clean = sanitize_text(skill_name);
            if !clean.is_empty() {
                return Some(clean);
            }
        }
    }
    None
}

fn extract_user_request_body(input: &str) -> &str {
    const OPEN: &str = "<USER_REQUEST>";
    const CLOSE: &str = "</USER_REQUEST>";
    if let Some(start_idx) = input.rfind(OPEN) {
        let after_open = &input[start_idx + OPEN.len()..];
        if let Some(end_idx) = after_open.find(CLOSE) {
            return after_open[..end_idx].trim();
        }
        return after_open.trim();
    }
    input.trim()
}

fn extract_slash_command(input: &str) -> Option<String> {
    let body = extract_user_request_body(input);
    let first_token = body.split_whitespace().next()?;
    let cmd_raw = first_token.strip_prefix('/')?;
    let len = cmd_raw.len();
    if !(2..=33).contains(&len) {
        return None;
    }
    let bytes = cmd_raw.as_bytes();
    if !bytes[0].is_ascii_alphanumeric() {
        return None;
    }
    if !bytes
        .iter()
        .all(|b| b.is_ascii_alphanumeric() || *b == b'_' || *b == b'-')
    {
        return None;
    }
    let lower = cmd_raw.to_ascii_lowercase();
    if matches!(
        lower.as_str(),
        "exit"
            | "quit"
            | "help"
            | "clear"
            | "config"
            | "model"
            | "statusline"
            | "usage"
            | "compact"
    ) {
        return None;
    }
    Some(lower)
}

fn extract_created_at_prefix(raw_line: &str) -> Option<i64> {
    let prefix_end = raw_line
        .char_indices()
        .take_while(|&(idx, _)| idx < 1024)
        .map(|(idx, ch)| idx + ch.len_utf8())
        .last()
        .unwrap_or(raw_line.len());
    let window = &raw_line[..prefix_end];
    let key_idx = window.find("\"created_at\"")?;
    let after_key = window[key_idx + "\"created_at\"".len()..].trim_start();
    let after_colon = after_key.strip_prefix(':')?.trim_start();
    let after_quote = after_colon.strip_prefix('"')?;
    let end_quote = after_quote.find('"')?;
    parse_iso_ms(&after_quote[..end_quote])
}

fn strip_markdown_inline(raw: &str) -> String {
    // Strip `**bold**` and `` `code` `` markers while preserving inner text
    let mut no_bold = String::with_capacity(raw.len());
    let mut rest = raw;
    while let Some(open) = rest.find("**") {
        no_bold.push_str(&rest[..open]);
        let after_open = &rest[open + 2..];
        if let Some(close) = after_open.find("**")
            && close > 0
            && !after_open[..close].contains('*')
        {
            no_bold.push_str(&after_open[..close]);
            rest = &after_open[close + 2..];
        } else {
            no_bold.push_str("**");
            rest = after_open;
        }
    }
    no_bold.push_str(rest);

    let mut out = String::with_capacity(no_bold.len());
    let mut rest2 = no_bold.as_str();
    while let Some(open) = rest2.find('`') {
        out.push_str(&rest2[..open]);
        let after_open = &rest2[open + 1..];
        if let Some(close) = after_open.find('`')
            && close > 0
        {
            out.push_str(&after_open[..close]);
            rest2 = &after_open[close + 1..];
        } else {
            out.push('`');
            rest2 = after_open;
        }
    }
    out.push_str(rest2);
    sanitize_text(&out)
}

pub fn parse_task_markdown(task_md_content: Option<&str>) -> Vec<TodoItem> {
    let Some(content) = task_md_content else {
        return Vec::new();
    };
    let mut todos = Vec::new();
    for raw_line in content.lines() {
        let trimmed = raw_line.trim_start();
        let after_bullet = trimmed
            .strip_prefix("- ")
            .or_else(|| trimmed.strip_prefix("* "));
        let Some(after_bullet) = after_bullet else {
            continue;
        };
        let after_bullet = after_bullet.trim_start();
        if !after_bullet.starts_with('[') || after_bullet.len() < 4 {
            continue;
        }
        let bytes = after_bullet.as_bytes();
        if bytes[2] != b']' || !bytes[3].is_ascii_whitespace() {
            continue;
        }
        let marker = bytes[1] as char;
        let status = match marker {
            ' ' => TodoStatus::Pending,
            '/' => TodoStatus::InProgress,
            'x' | 'X' => TodoStatus::Completed,
            _ => continue,
        };
        let text = strip_markdown_inline(&after_bullet[4..]);
        if !text.is_empty() {
            todos.push(TodoItem { text, status });
        }
    }
    todos
}

fn has_nonzero_exit_code(content: &str) -> bool {
    let needle = "Command exited with code ";
    if let Some(idx) = content.find(needle) {
        let after = &content[idx + needle.len()..];
        if let Some(&first_byte) = after.as_bytes().first() {
            return (b'1'..=b'9').contains(&first_byte);
        }
    }
    false
}

pub(crate) const MIN_VALID_TPS_X10: u32 = 1;
pub(crate) const MAX_VALID_TPS_X10: u32 = 20_000;

pub(crate) fn update_min_max_x10(
    min_slot: &mut Option<u32>,
    max_slot: &mut Option<u32>,
    sample_x10: u32,
) {
    if !(MIN_VALID_TPS_X10..=MAX_VALID_TPS_X10).contains(&sample_x10) {
        return;
    }
    *min_slot = Some(min_slot.map_or(sample_x10, |m| m.min(sample_x10)));
    *max_slot = Some(max_slot.map_or(sample_x10, |m| m.max(sample_x10)));
}

pub(crate) fn merge_min_max_pair_x10(
    min_slot: &mut Option<u32>,
    max_slot: &mut Option<u32>,
    other_min: Option<u32>,
    other_max: Option<u32>,
) {
    if let Some(v) = other_min {
        update_min_max_x10(min_slot, max_slot, v);
    }
    if let Some(v) = other_max {
        update_min_max_x10(min_slot, max_slot, v);
    }
}

pub fn compute_tps_x10(output_tokens: u64, dt_ms: i64) -> Option<u32> {
    if output_tokens < 20 || !(400..=180_000).contains(&dt_ms) {
        return None;
    }
    let tps = (output_tokens as f64) / ((dt_ms as f64) / 1000.0);
    if !(1.0..=500.0).contains(&tps) {
        return None;
    }
    Some((tps * 10.0).round() as u32)
}

pub fn update_speed_anchors(
    short: &mut Option<SpeedAnchorPoint>,
    long: &mut Option<SpeedAnchorPoint>,
    tokens: u64,
    dt_ms: i64,
) {
    if !(40..=50_000).contains(&tokens) || !(1_000..=180_000).contains(&dt_ms) {
        return;
    }
    if tokens <= 400 {
        *short = Some(SpeedAnchorPoint { tokens, dt_ms });
    } else if tokens >= 600 && long.is_none_or(|existing| tokens >= (existing.tokens * 3) / 4) {
        *long = Some(SpeedAnchorPoint { tokens, dt_ms });
    }
}

pub fn estimate_ttft_overhead_ms(
    short: Option<SpeedAnchorPoint>,
    long: Option<SpeedAnchorPoint>,
) -> i64 {
    let (Some(s), Some(l)) = (short, long) else {
        return 0;
    };
    if s.tokens > 400
        || l.tokens < 600
        || l.tokens < s.tokens.saturating_mul(3)
        || l.dt_ms <= s.dt_ms + 3_000
    {
        return 0;
    }
    let ms_per_tok = ((l.dt_ms - s.dt_ms) as f64) / ((l.tokens - s.tokens) as f64);
    let implied_tps = 1000.0 / ms_per_tok;
    if !(10.0..=400.0).contains(&implied_tps) {
        return 0;
    }
    let overhead = ((s.dt_ms as f64) - (s.tokens as f64) * ms_per_tok).round() as i64;
    if overhead < 800 {
        0
    } else {
        overhead.clamp(0, 25_000)
    }
}

pub fn adjust_coarse_dt_ms(raw_dt_ms: i64, overhead_ms: i64) -> i64 {
    if overhead_ms <= 0 {
        raw_dt_ms
    } else {
        (raw_dt_ms - overhead_ms)
            .max((raw_dt_ms * 25) / 100)
            .max(400)
    }
}

pub fn blend_tps_ewma_x10(
    prev_x10: Option<u32>,
    sample_x10: u32,
    output_tokens: u64,
    ttft_stripped: bool,
) -> u32 {
    let Some(prev) = prev_x10.filter(|&p| (10..=5_000).contains(&p)) else {
        return sample_x10;
    };
    let tok = output_tokens as f64;
    let alpha = if ttft_stripped {
        (tok / (tok + 120.0)).clamp(0.25, 0.90)
    } else if output_tokens >= 1_200 && sample_x10 >= prev {
        (tok / (tok + 400.0)).clamp(0.70, 0.92)
    } else if output_tokens < 400 && sample_x10 < prev {
        (tok / (tok + 1_800.0)).clamp(0.05, 0.18)
    } else {
        (tok / (tok + 900.0)).clamp(0.10, 0.85)
    };
    let blended = (prev as f64) * (1.0 - alpha) + (sample_x10 as f64) * alpha;
    blended.round().clamp(10.0, 5_000.0) as u32
}

pub fn parse_subagent_status(raw: Option<&str>) -> SubagentStatus {
    let Some(s) = raw.map(str::trim).filter(|s| !s.is_empty()) else {
        return SubagentStatus::Completed;
    };
    if s.eq_ignore_ascii_case("running")
        || s.eq_ignore_ascii_case("starting")
        || s.eq_ignore_ascii_case("in_progress")
    {
        SubagentStatus::Running
    } else if s.eq_ignore_ascii_case("blocked")
        || s.eq_ignore_ascii_case("waiting")
        || s.eq_ignore_ascii_case("paused")
    {
        SubagentStatus::Blocked
    } else if s.eq_ignore_ascii_case("error")
        || s.eq_ignore_ascii_case("failed")
        || s.eq_ignore_ascii_case("cancelled")
        || s.eq_ignore_ascii_case("canceled")
        || s.eq_ignore_ascii_case("killed")
    {
        SubagentStatus::Error
    } else {
        SubagentStatus::Completed
    }
}

struct IndexedStep {
    line_start_offset: u64,
    step: RawStep,
}

/// Folds chronological tool invocations newest-first: keeps active `Running` entries distinct,
/// supersedes historical `Error` calls when a newer invocation of the same tool succeeded or is
/// currently running, and folds unresolved `Error` or `Completed` counts by tool name while
/// preserving the most recent target summary.
fn fold_tools_newest_first(raw_tools: Vec<ToolEntry>, sort_by_status: bool) -> Vec<ToolEntry> {
    let mut aggregated: Vec<ToolEntry> = Vec::new();
    let mut completed_idx_by_name: HashMap<String, usize> = HashMap::new();
    let mut error_idx_by_name: HashMap<String, usize> = HashMap::new();
    let mut seen_newer_active_or_ok: BTreeSet<String> = BTreeSet::new();

    for item in raw_tools.into_iter().rev() {
        match item.status {
            ToolStatus::Running => {
                seen_newer_active_or_ok.insert(item.name.clone());
                aggregated.push(item);
            }
            ToolStatus::Completed => {
                seen_newer_active_or_ok.insert(item.name.clone());
                if let Some(&agg_idx) = completed_idx_by_name.get(&item.name) {
                    let existing = &mut aggregated[agg_idx];
                    existing.count = existing.count.saturating_add(item.count);
                    if existing.target.is_none() && item.target.is_some() {
                        existing.target = item.target;
                    }
                } else {
                    let idx = aggregated.len();
                    completed_idx_by_name.insert(item.name.clone(), idx);
                    aggregated.push(item);
                }
            }
            ToolStatus::Error => {
                if seen_newer_active_or_ok.contains(&item.name) {
                    if let Some(&agg_idx) = completed_idx_by_name.get(&item.name) {
                        aggregated[agg_idx].count =
                            aggregated[agg_idx].count.saturating_add(item.count);
                    }
                } else if let Some(&err_idx) = error_idx_by_name.get(&item.name) {
                    let existing = &mut aggregated[err_idx];
                    existing.count = existing.count.saturating_add(item.count);
                    if existing.target.is_none() && item.target.is_some() {
                        existing.target = item.target;
                    }
                } else {
                    let idx = aggregated.len();
                    error_idx_by_name.insert(item.name.clone(), idx);
                    aggregated.push(item);
                }
            }
        }
    }

    if sort_by_status {
        aggregated.sort_by_key(|t| t.status);
    } else {
        aggregated.reverse();
    }
    aggregated
}

#[derive(Clone)]
struct StepAccumulator {
    session_started_at: Option<i64>,
    skills_vec: Vec<String>,
    skills_seen: BTreeSet<String>,
    mcp_vec: Vec<String>,
    mcp_seen: BTreeSet<String>,
    mcp_last_status: HashMap<String, ToolStatus>,
    tools: Vec<ToolEntry>,
    subagents: Vec<SubagentEntry>,
    active_slash_mode: Option<String>,
    completed_step_tps_x10: Option<u32>,
    session_min_tps_x10: Option<u32>,
    session_max_tps_x10: Option<u32>,
    short_anchor: Option<SpeedAnchorPoint>,
    long_anchor: Option<SpeedAnchorPoint>,
    latest_planner_step: Option<PlannerStepTiming>,
    last_step_at: Option<i64>,
    max_step_idx: usize,
    step_lines_seen: usize,
    compaction_count: u32,
    last_compaction_tokens: Option<u64>,
}

impl StepAccumulator {
    fn from_cursor(cursor: Option<&TranscriptCursor>, head_started_at: Option<i64>) -> Self {
        let Some(c) = cursor else {
            return Self {
                session_started_at: head_started_at,
                skills_vec: Vec::new(),
                skills_seen: BTreeSet::new(),
                mcp_vec: Vec::new(),
                mcp_seen: BTreeSet::new(),
                mcp_last_status: HashMap::new(),
                tools: Vec::new(),
                subagents: Vec::new(),
                active_slash_mode: None,
                completed_step_tps_x10: None,
                session_min_tps_x10: None,
                session_max_tps_x10: None,
                short_anchor: None,
                long_anchor: None,
                latest_planner_step: None,
                last_step_at: None,
                max_step_idx: 0,
                step_lines_seen: 0,
                compaction_count: 0,
                last_compaction_tokens: None,
            };
        };

        let skills_seen: BTreeSet<String> = c.skills.iter().cloned().collect();
        let mcp_seen: BTreeSet<String> = c.mcp_servers.iter().cloned().collect();
        let err_set: BTreeSet<&str> = c.mcp_last_error.iter().map(String::as_str).collect();
        let mut mcp_last_status = HashMap::new();
        for srv in &c.mcp_servers {
            let st = if err_set.contains(srv.as_str()) {
                ToolStatus::Error
            } else {
                ToolStatus::Completed
            };
            mcp_last_status.insert(srv.clone(), st);
        }

        let mut session_min_tps_x10 = None;
        let mut session_max_tps_x10 = None;
        merge_min_max_pair_x10(
            &mut session_min_tps_x10,
            &mut session_max_tps_x10,
            c.session_min_tps_x10,
            c.session_max_tps_x10,
        );

        Self {
            session_started_at: c.session_started_at.or(head_started_at),
            skills_vec: c.skills.clone(),
            skills_seen,
            mcp_vec: c.mcp_servers.clone(),
            mcp_seen,
            mcp_last_status,
            tools: c.folded_tools.clone(),
            subagents: c.subagents.clone(),
            active_slash_mode: c.active_slash_mode.clone(),
            completed_step_tps_x10: c.completed_step_tps_x10,
            session_min_tps_x10,
            session_max_tps_x10,
            short_anchor: c.short_anchor,
            long_anchor: c.long_anchor,
            latest_planner_step: c.latest_planner_step.clone(),
            last_step_at: c.last_step_at,
            max_step_idx: c.max_step_idx,
            step_lines_seen: c.step_lines_seen,
            compaction_count: c.compaction_count,
            last_compaction_tokens: c.last_compaction_tokens,
        }
    }

    fn ingest_steps(&mut self, steps: &[IndexedStep], unmatched_is_completed: bool) {
        if self.session_started_at.is_none()
            && let Some(first) = steps.first()
            && let Some(iso) = first.step.created_at.as_deref()
        {
            self.session_started_at = parse_iso_ms(iso);
        }

        let base_seen = self.step_lines_seen;
        self.step_lines_seen = self.step_lines_seen.saturating_add(steps.len());

        for i in 0..steps.len() {
            let step = &steps[i].step;
            let fallback_idx = base_seen + i;
            let ts = step.created_at.as_deref().and_then(parse_iso_ms);
            if ts.is_some() {
                self.last_step_at = ts;
            }
            if let Some(idx) = step.step_index
                && idx + 1 > self.max_step_idx
            {
                self.max_step_idx = idx + 1;
            }

            let step_type = step.step_type.as_deref().unwrap_or("");
            if step_type.eq_ignore_ascii_case("CHECKPOINT") {
                self.compaction_count = self.compaction_count.saturating_add(1);
                let prompt_tok = step
                    .input_tokens
                    .unwrap_or(0)
                    .saturating_add(step.cache_read_tokens.unwrap_or(0))
                    .saturating_add(step.cache_creation_tokens.unwrap_or(0));
                if prompt_tok > 0 {
                    self.last_compaction_tokens = Some(prompt_tok);
                }
            }
            if step_type == "USER_INPUT" {
                if let Some(ref mut lp) = self.latest_planner_step {
                    lp.has_trailing_user_input = true;
                }
                if let Some(Value::String(text)) = step.content.as_ref() {
                    if let Some(cmd) = extract_slash_command(text) {
                        if cmd == "plan" {
                            self.active_slash_mode = Some("plan".to_string());
                        } else {
                            self.active_slash_mode = None;
                        }
                        if self.skills_seen.insert(cmd.clone()) {
                            self.skills_vec.push(cmd);
                        }
                    } else {
                        self.active_slash_mode = None;
                    }
                }
            }

            if step_type == "PLANNER_RESPONSE"
                && let Some(start_ms) = ts
            {
                let out_tok = step.output_tokens.unwrap_or(0);
                let est_tok = estimate_step_tokens(step);
                let is_running = step
                    .status
                    .as_deref()
                    .is_some_and(|s| s.eq_ignore_ascii_case("RUNNING"));
                let is_done = !is_running
                    && (step
                        .status
                        .as_deref()
                        .is_some_and(|s| s.eq_ignore_ascii_case("DONE"))
                        || out_tok > 0);
                let ended_at_ms = if is_done {
                    steps.get(i + 1).and_then(|next_item| {
                        if next_item.step.step_type.as_deref() == Some("GENERIC") {
                            next_item.step.created_at.as_deref().and_then(parse_iso_ms)
                        } else {
                            None
                        }
                    })
                } else {
                    None
                };

                if is_done
                    && out_tok > 0
                    && let Some(end_ms) = ended_at_ms
                {
                    let raw_dt = end_ms - start_ms;
                    update_speed_anchors(
                        &mut self.short_anchor,
                        &mut self.long_anchor,
                        out_tok,
                        raw_dt,
                    );
                    let overhead_ms =
                        estimate_ttft_overhead_ms(self.short_anchor, self.long_anchor);
                    let adj_dt = adjust_coarse_dt_ms(raw_dt, overhead_ms);
                    if let Some(sample_x10) = compute_tps_x10(out_tok, adj_dt) {
                        self.completed_step_tps_x10 = Some(blend_tps_ewma_x10(
                            self.completed_step_tps_x10,
                            sample_x10,
                            out_tok,
                            false,
                        ));
                        update_min_max_x10(
                            &mut self.session_min_tps_x10,
                            &mut self.session_max_tps_x10,
                            sample_x10,
                        );
                    }
                }

                if !is_done || out_tok > 0 || est_tok > 0 {
                    self.latest_planner_step = Some(PlannerStepTiming {
                        step_index: step.step_index.unwrap_or(fallback_idx),
                        output_tokens: out_tok,
                        estimated_tokens: est_tok,
                        started_at_ms: start_ms,
                        ended_at_ms,
                        has_trailing_user_input: false,
                        is_done,
                    });
                }
            }

            if step_type == "PLANNER_RESPONSE"
                && let Some(tool_calls) = step.tool_calls.as_ref()
                && !tool_calls.is_empty()
            {
                let mut results: Vec<&RawStep> = Vec::new();
                for next_item in &steps[i + 1..] {
                    if results.len() >= tool_calls.len() {
                        break;
                    }
                    let nt = next_item.step.step_type.as_deref().unwrap_or("");
                    if nt == "PLANNER_RESPONSE" || nt == "USER_INPUT" {
                        break;
                    }
                    if nt == "GENERIC" {
                        results.push(&next_item.step);
                    }
                }

                for (idx, tc) in tool_calls.iter().enumerate() {
                    let raw_name = {
                        let n = sanitize_text(tc.name.as_deref().unwrap_or("tool"));
                        if n.is_empty() { "tool".to_string() } else { n }
                    };
                    let args = tc.args.as_ref();
                    let result_step = results.get(idx).copied();
                    let ended_at = result_step
                        .and_then(|r| r.created_at.as_deref())
                        .and_then(parse_iso_ms);

                    let status = if let Some(res) = result_step {
                        let content_str =
                            res.content.as_ref().and_then(Value::as_str).unwrap_or("");
                        let trimmed_content = content_str.trim_start();
                        let is_err = res.status.as_deref() == Some("ERROR")
                            || (trimmed_content.len() >= 6
                                && trimmed_content[..6].eq_ignore_ascii_case("error:"))
                            || has_nonzero_exit_code(content_str);
                        if is_err {
                            ToolStatus::Error
                        } else {
                            ToolStatus::Completed
                        }
                    } else if unmatched_is_completed {
                        ToolStatus::Completed
                    } else {
                        ToolStatus::Running
                    };

                    if raw_name == "read_file"
                        && let Some(a) = args
                        && let Some(skill) =
                            detect_skill_from_path(&unwrap_arg_value(a.get("Path")))
                        && self.skills_seen.insert(skill.clone())
                    {
                        self.skills_vec.push(skill);
                    }

                    let mcp_server = if raw_name == "call_mcp_tool" {
                        args.map(|a| unwrap_arg_value(a.get("ServerName")))
                            .unwrap_or_default()
                    } else if let Some(after_mcp) = raw_name.strip_prefix("mcp_") {
                        after_mcp.split('_').next().unwrap_or("").to_string()
                    } else {
                        String::new()
                    };
                    if !mcp_server.is_empty() {
                        if self.mcp_seen.insert(mcp_server.clone()) {
                            self.mcp_vec.push(mcp_server.clone());
                        }
                        self.mcp_last_status.insert(mcp_server, status);
                    }

                    if raw_name == "invoke_subagent"
                        && let Some(a) = args
                    {
                        let name = {
                            let t = unwrap_arg_value(a.get("TypeName"));
                            if t.is_empty() {
                                "subagent".to_string()
                            } else {
                                t
                            }
                        };
                        let role = {
                            let r = unwrap_arg_value(a.get("Role"));
                            (!r.is_empty()).then_some(r)
                        };
                        let model = {
                            let m = unwrap_arg_value(a.get("Model"));
                            (!m.is_empty()).then_some(m)
                        };
                        self.subagents.push(SubagentEntry {
                            name,
                            role,
                            model,
                            status: if status == ToolStatus::Running {
                                SubagentStatus::Running
                            } else {
                                SubagentStatus::Completed
                            },
                            started_at: ts,
                            ended_at,
                        });
                    }

                    let target = summarize_tool_target(&raw_name, args);
                    self.tools.push(ToolEntry {
                        id: format!("{}:{idx}", step.step_index.unwrap_or(fallback_idx)),
                        name: raw_name,
                        target,
                        status,
                        count: 1,
                        started_at: ts,
                        ended_at,
                    });
                }
            }
        }
    }

    fn to_cursor(&self, offset: u64) -> TranscriptCursor {
        let mcp_last_error: Vec<String> = self
            .mcp_vec
            .iter()
            .filter(|srv| self.mcp_last_status.get(*srv) == Some(&ToolStatus::Error))
            .cloned()
            .collect();
        let folded_tools = fold_tools_newest_first(self.tools.clone(), false);
        let subagents = if self.subagents.len() > 64 {
            self.subagents[self.subagents.len() - 64..].to_vec()
        } else {
            self.subagents.clone()
        };
        TranscriptCursor {
            version: TRANSCRIPT_CURSOR_VERSION,
            offset,
            session_started_at: self.session_started_at,
            skills: self.skills_vec.clone(),
            mcp_servers: self.mcp_vec.clone(),
            mcp_last_error,
            active_slash_mode: self.active_slash_mode.clone(),
            completed_step_tps_x10: self.completed_step_tps_x10,
            session_min_tps_x10: self.session_min_tps_x10,
            session_max_tps_x10: self.session_max_tps_x10,
            short_anchor: self.short_anchor,
            long_anchor: self.long_anchor,
            latest_planner_step: self.latest_planner_step.clone(),
            last_step_at: self.last_step_at,
            max_step_idx: self.max_step_idx,
            step_lines_seen: self.step_lines_seen,
            compaction_count: self.compaction_count,
            last_compaction_tokens: self.last_compaction_tokens,
            folded_tools,
            subagents,
        }
    }

    fn into_summary(self, todos: Vec<TodoItem>, stdin: Option<&AgyStdinData>) -> TranscriptSummary {
        let aggregated = fold_tools_newest_first(self.tools, true);
        let mcp_err_vec: Vec<String> = self
            .mcp_vec
            .iter()
            .filter(|srv| self.mcp_last_status.get(*srv) == Some(&ToolStatus::Error))
            .cloned()
            .collect();

        // Merge authoritative live `stdin.subagents` with transcript subagent timestamps/models
        let mut merged_subagents = Vec::new();
        if let Some(live_agents) = stdin.and_then(|s| s.subagents.as_ref())
            && !live_agents.is_empty()
        {
            for (idx, sa) in live_agents.iter().enumerate() {
                let live_clean_name = sa
                    .name
                    .as_deref()
                    .filter(|s| !s.trim().is_empty())
                    .map(sanitize_text)
                    .filter(|s| !s.is_empty());
                let matched = match live_clean_name.as_deref() {
                    Some(target_name) => self
                        .subagents
                        .get(idx)
                        .filter(|m| m.name == target_name)
                        .or_else(|| self.subagents.iter().rev().find(|m| m.name == target_name))
                        .or_else(|| self.subagents.get(idx)),
                    None => self.subagents.get(idx),
                };
                let name = live_clean_name
                    .or_else(|| matched.map(|m| m.name.clone()))
                    .unwrap_or_else(|| "subagent".to_string());
                let role = sa
                    .role
                    .as_deref()
                    .filter(|s| !s.trim().is_empty())
                    .map(sanitize_text)
                    .filter(|s| !s.is_empty())
                    .or_else(|| matched.and_then(|m| m.role.clone()));
                let model = sa
                    .model
                    .as_deref()
                    .filter(|s| !s.trim().is_empty())
                    .map(sanitize_text)
                    .filter(|s| !s.is_empty())
                    .or_else(|| matched.and_then(|m| m.model.clone()));
                merged_subagents.push(SubagentEntry {
                    name,
                    role,
                    model,
                    status: parse_subagent_status(sa.status.as_deref()),
                    started_at: matched.and_then(|m| m.started_at),
                    ended_at: matched.and_then(|m| m.ended_at),
                });
            }
        } else {
            merged_subagents = self.subagents;
        }

        TranscriptSummary {
            tools: aggregated,
            skills: self.skills_vec,
            mcp_servers: self.mcp_vec,
            mcp_errors: mcp_err_vec,
            subagents: merged_subagents,
            todos,
            active_slash_mode: self.active_slash_mode,
            completed_step_tps_x10: self.completed_step_tps_x10,
            resolved_tps_x10: None,
            session_min_tps_x10: self.session_min_tps_x10,
            session_max_tps_x10: self.session_max_tps_x10,
            short_anchor: self.short_anchor,
            long_anchor: self.long_anchor,
            latest_planner_step: self.latest_planner_step,
            session_started_at: self.session_started_at,
            last_step_at: self.last_step_at,
            step_count: if self.max_step_idx > 0 {
                self.max_step_idx
            } else {
                self.step_lines_seen
            },
            compaction_count: self.compaction_count,
            last_compaction_tokens: self.last_compaction_tokens,
        }
    }
}

/// Incremental `transcript.jsonl` parser supporting byte-offset cursor resumption (`O(Δ)`).
/// Advances `TranscriptCursor::offset` strictly up to the byte offset immediately preceding the
/// latest `PLANNER_RESPONSE` or `USER_INPUT` step so live in-flight tool calls and speed timings
/// remain re-evaluated on every tick while historical turns stay folded in `TranscriptCursor`.
pub fn parse_transcript_with_cursor(
    cursor: Option<&TranscriptCursor>,
    head_line: Option<&str>,
    delta_text: Option<&str>,
    base_offset: u64,
    task_md_content: Option<&str>,
    stdin: Option<&AgyStdinData>,
) -> (TranscriptSummary, Option<TranscriptCursor>) {
    let todos = parse_task_markdown(task_md_content);
    let head_started_at = head_line
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .and_then(|l| {
            serde_json::from_str::<RawStep>(l)
                .ok()
                .and_then(|s| s.created_at)
                .and_then(|iso| parse_iso_ms(&iso))
                .or_else(|| extract_created_at_prefix(l))
        });

    let valid_cursor =
        cursor.filter(|c| c.version == TRANSCRIPT_CURSOR_VERSION && c.offset == base_offset);

    let mut parsed_steps = Vec::new();
    if let Some(delta) = delta_text {
        let bytes = delta.as_bytes();
        let mut pos = 0usize;
        while pos < bytes.len() {
            let line_start = pos;
            let (line_end, next_pos) = match delta[pos..].find('\n') {
                Some(rel_nl) => (pos + rel_nl, pos + rel_nl + 1),
                None => (bytes.len(), bytes.len()),
            };
            pos = next_pos;
            let trimmed = delta[line_start..line_end].trim();
            if trimmed.is_empty() {
                continue;
            }
            if let Ok(step) = serde_json::from_str::<RawStep>(trimmed) {
                parsed_steps.push(IndexedStep {
                    line_start_offset: base_offset.saturating_add(line_start as u64),
                    step,
                });
            }
        }
    }

    let is_idle = stdin
        .and_then(|s| s.agent_state.as_deref())
        .is_some_and(|st| st.eq_ignore_ascii_case("idle"));

    let last_turn_idx = parsed_steps.iter().rposition(|item| {
        matches!(
            item.step.step_type.as_deref(),
            Some("PLANNER_RESPONSE" | "USER_INPUT")
        )
    });
    let commit_idx = last_turn_idx.unwrap_or(0);

    let mut acc = StepAccumulator::from_cursor(valid_cursor, head_started_at);
    let mut next_cursor = None;
    if commit_idx > 0 {
        // Check if a trailing USER_INPUT at `commit_idx` closes a planner step right before `commit_idx`
        acc.ingest_steps(&parsed_steps[..commit_idx], true);
        let commit_offset = parsed_steps[commit_idx].line_start_offset;
        let candidate = acc.to_cursor(commit_offset);
        if valid_cursor != Some(&candidate) {
            next_cursor = Some(candidate);
        }
    }
    acc.ingest_steps(&parsed_steps[commit_idx..], is_idle);

    (acc.into_summary(todos, stdin), next_cursor)
}

/// Pure parser for Antigravity `transcript.jsonl` (head first-line + bounded tail lines) and `task.md`.
pub fn parse_transcript_and_tasks(
    head_line: Option<&str>,
    tail_text: Option<&str>,
    task_md_content: Option<&str>,
    stdin: Option<&AgyStdinData>,
) -> TranscriptSummary {
    parse_transcript_with_cursor(None, head_line, tail_text, 0, task_md_content, stdin).0
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::SubagentStdin;
    use serde_json::json;

    #[test]
    fn unwrap_arg_value_decodes_double_serialized_strings() {
        assert_eq!(
            unwrap_arg_value(Some(&json!(
                "\"/Users/max/.gemini/config/skills/hud/SKILL.md\""
            ))),
            "/Users/max/.gemini/config/skills/hud/SKILL.md"
        );
        assert_eq!(unwrap_arg_value(Some(&json!("plain-value"))), "plain-value");
        assert_eq!(unwrap_arg_value(Some(&json!(5000))), "5000");
    }

    #[test]
    fn parse_iso_ms_handles_utc_fractional_and_offsets() {
        assert_eq!(
            parse_iso_ms("2026-11-08T13:00:00Z"),
            Some(1_794_142_800_000)
        );
        assert_eq!(
            parse_iso_ms("2026-11-08T21:00:00.250+08:00"),
            Some(1_794_142_800_250)
        );
    }

    #[test]
    fn parses_transcript_tools_skills_mcp_subagents_and_todos() {
        let steps = [
            json!({
                "step_index": 0,
                "source": "USER_EXPLICIT",
                "type": "USER_INPUT",
                "status": "DONE",
                "created_at": "2026-11-08T13:00:00Z",
                "content": "/boost Build the CLI HUD"
            }),
            json!({
                "step_index": 1,
                "source": "MODEL",
                "type": "PLANNER_RESPONSE",
                "status": "DONE",
                "created_at": "2026-11-08T13:00:02Z",
                "tool_calls": [
                    { "name": "read_file", "args": { "Path": "\"/Users/dev/.gemini/skills/hud/SKILL.md\"" } },
                    { "name": "read_file", "args": { "Path": "\"/Users/dev/project/README.md\"" } },
                    { "name": "call_mcp_tool", "args": { "ServerName": "\"context7\"", "ToolName": "\"query-docs\"" } },
                    { "name": "invoke_subagent", "args": { "TypeName": "\"research\"", "Role": "\"Spec Reader\"" } }
                ]
            }),
            json!({ "step_index": 2, "source": "MODEL", "type": "GENERIC", "status": "DONE", "created_at": "2026-11-08T13:00:03Z", "content": "ok" }),
            json!({ "step_index": 3, "source": "MODEL", "type": "GENERIC", "status": "DONE", "created_at": "2026-11-08T13:00:03Z", "content": "ok" }),
            json!({ "step_index": 4, "source": "MODEL", "type": "GENERIC", "status": "ERROR", "created_at": "2026-11-08T13:00:04Z", "content": "Error: timeout" }),
            json!({ "step_index": 5, "source": "MODEL", "type": "GENERIC", "status": "DONE", "created_at": "2026-11-08T13:00:05Z", "content": "spawned" }),
            json!({
                "step_index": 6,
                "source": "MODEL",
                "type": "PLANNER_RESPONSE",
                "status": "DONE",
                "created_at": "2026-11-08T13:01:00Z",
                "tool_calls": [
                    { "name": "edit_file", "args": { "Path": "\"/Users/dev/project/render.rs\"" } },
                    { "name": "run_command", "args": { "CommandLine": "\"cargo test\"", "toolSummary": "\"Run unit tests\"" } }
                ]
            }),
            json!({ "step_index": 7, "source": "MODEL", "type": "GENERIC", "status": "DONE", "created_at": "2026-11-08T13:01:01Z", "content": "edited" }),
        ];

        let jsonl = steps
            .iter()
            .map(serde_json::to_string)
            .collect::<Result<Vec<_>, _>>()
            .unwrap()
            .join("\n");
        let head = jsonl.lines().next();
        let task_md = "# Tasks\n- [x] **Parse** `stdin` JSON\n- [/] Parse transcript.jsonl\n- [ ] Validate strict schema";
        let stdin = AgyStdinData {
            agent_state: Some("tool_use".to_string()),
            subagents: Some(vec![SubagentStdin {
                name: Some("research".to_string()),
                role: Some("Spec Reader".to_string()),
                status: Some("running".to_string()),
                model: None,
            }]),
            ..Default::default()
        };

        let summary = parse_transcript_and_tasks(head, Some(&jsonl), Some(task_md), Some(&stdin));
        assert_eq!(summary.session_started_at, Some(1_794_142_800_000));
        assert_eq!(summary.skills, vec!["boost", "hud"]);
        assert_eq!(summary.mcp_servers, vec!["context7"]);
        assert_eq!(summary.mcp_errors, vec!["context7"]);

        assert_eq!(summary.tools[0].name, "run_command");
        assert_eq!(summary.tools[0].status, ToolStatus::Running);
        assert_eq!(summary.tools[0].target.as_deref(), Some("Run unit tests"));

        let err_tool = summary
            .tools
            .iter()
            .find(|t| t.name == "call_mcp_tool")
            .unwrap();
        assert_eq!(err_tool.status, ToolStatus::Error);

        let read_tool = summary
            .tools
            .iter()
            .find(|t| t.name == "read_file")
            .unwrap();
        assert_eq!(read_tool.count, 2);
        assert_eq!(read_tool.target.as_deref(), Some("README.md"));

        assert_eq!(summary.subagents.len(), 1);
        assert_eq!(summary.subagents[0].status, SubagentStatus::Running);
        assert_eq!(summary.subagents[0].role.as_deref(), Some("Spec Reader"));

        assert_eq!(summary.todos.len(), 3);
        assert_eq!(summary.todos[0].text, "Parse stdin JSON");
        assert_eq!(summary.todos[0].status, TodoStatus::Completed);
        assert_eq!(summary.todos[1].status, TodoStatus::InProgress);
        assert_eq!(summary.todos[2].status, TodoStatus::Pending);
    }

    #[test]
    fn user_request_slash_mode_and_truncated_head_timestamp_recovery() {
        let truncated_head = r#"{"step_index":0,"source":"USER_EXPLICIT","type":"USER_INPUT","status":"DONE","created_at":"2026-11-09T01:41:18Z","content":"<USER_REQUEST>\nvery long..."#;
        let tail_steps = [
            json!({
                "step_index": 10,
                "source": "USER_EXPLICIT",
                "type": "USER_INPUT",
                "status": "DONE",
                "created_at": "2026-11-09T02:10:00Z",
                "content": "<USER_REQUEST>\nMissing /hub-update command in logs\n</USER_REQUEST>"
            }),
            json!({
                "step_index": 11,
                "source": "USER_EXPLICIT",
                "type": "USER_INPUT",
                "status": "DONE",
                "created_at": "2026-11-09T02:45:00Z",
                "content": "<CONTEXT_SUMMARY>\nOld /boost summary\n</CONTEXT_SUMMARY>\n<USER_REQUEST>\n/plan Compare screenshots and fix\n</USER_REQUEST>"
            }),
        ];
        let tail = tail_steps
            .iter()
            .map(serde_json::to_string)
            .collect::<Result<Vec<_>, _>>()
            .unwrap()
            .join("\n");

        let summary = parse_transcript_and_tasks(Some(truncated_head), Some(&tail), None, None);
        assert_eq!(
            summary.session_started_at,
            parse_iso_ms("2026-11-09T01:41:18Z")
        );
        assert_eq!(summary.skills, vec!["plan"]);
        assert_eq!(summary.active_slash_mode.as_deref(), Some("plan"));

        // Subsequent non-plan prompt clears `active_slash_mode`
        let follow_up = format!(
            "{tail}\n{}",
            json!({
                "step_index": 12,
                "source": "USER_EXPLICIT",
                "type": "USER_INPUT",
                "status": "DONE",
                "created_at": "2026-11-09T02:56:56Z",
                "content": "<USER_REQUEST>\nApprove and proceed\n</USER_REQUEST>"
            })
        );
        let summary_after =
            parse_transcript_and_tasks(Some(truncated_head), Some(&follow_up), None, None);
        assert_eq!(summary_after.active_slash_mode, None);
        assert_eq!(summary_after.skills, vec!["plan"]);
    }

    #[test]
    fn superseded_tool_errors_and_recovered_mcp_servers_fold_cleanly() {
        let steps = [
            json!({
                "step_index": 0,
                "source": "MODEL",
                "type": "PLANNER_RESPONSE",
                "status": "DONE",
                "created_at": "2026-11-09T02:00:00Z",
                "tool_calls": [
                    { "name": "run_command", "args": { "CommandLine": "\"cargo test\"", "toolSummary": "\"Failing compile\"" } },
                    { "name": "call_mcp_tool", "args": { "ServerName": "\"context7\"", "ToolName": "\"query-docs\"" } }
                ]
            }),
            json!({ "step_index": 1, "source": "MODEL", "type": "GENERIC", "status": "DONE", "created_at": "2026-11-09T02:00:02Z", "content": "Command exited with code 101." }),
            json!({ "step_index": 2, "source": "MODEL", "type": "GENERIC", "status": "ERROR", "created_at": "2026-11-09T02:00:03Z", "content": "Error: timeout" }),
            json!({
                "step_index": 3,
                "source": "MODEL",
                "type": "PLANNER_RESPONSE",
                "status": "DONE",
                "created_at": "2026-11-09T02:01:00Z",
                "tool_calls": [
                    { "name": "run_command", "args": { "CommandLine": "\"cargo test\"", "toolSummary": "\"Passing tests\"" } },
                    { "name": "call_mcp_tool", "args": { "ServerName": "\"context7\"", "ToolName": "\"query-docs\"" } }
                ]
            }),
            json!({ "step_index": 4, "source": "MODEL", "type": "GENERIC", "status": "DONE", "created_at": "2026-11-09T02:01:05Z", "content": "Command exited with code 0." }),
            json!({ "step_index": 5, "source": "MODEL", "type": "GENERIC", "status": "DONE", "created_at": "2026-11-09T02:01:06Z", "content": "docs ok" }),
        ];
        let jsonl = steps
            .iter()
            .map(serde_json::to_string)
            .collect::<Result<Vec<_>, _>>()
            .unwrap()
            .join("\n");
        let summary = parse_transcript_and_tasks(jsonl.lines().next(), Some(&jsonl), None, None);
        assert!(summary.mcp_errors.is_empty());
        assert_eq!(summary.mcp_servers, vec!["context7"]);
        let run_tool = summary
            .tools
            .iter()
            .find(|t| t.name == "run_command")
            .unwrap();
        assert_eq!(run_tool.status, ToolStatus::Completed);
        assert_eq!(run_tool.count, 2);
        assert_eq!(run_tool.target.as_deref(), Some("Passing tests"));
    }

    #[test]
    fn checkpoint_steps_accumulate_compaction_count_and_survive_cursor_upgrades() {
        let initial_steps = [
            json!({
                "step_index": 0,
                "source": "USER_EXPLICIT",
                "type": "USER_INPUT",
                "status": "DONE",
                "created_at": "2026-11-09T03:00:00Z",
                "content": "Start long session"
            }),
            json!({
                "step_index": 1,
                "source": "SYSTEM",
                "type": "CHECKPOINT",
                "status": "DONE",
                "created_at": "2026-11-09T03:10:00Z",
                "input_tokens": 3512,
                "cache_read_tokens": 253_403,
                "output_tokens": 3763,
                "content": "# Resuming from a compaction"
            }),
            json!({
                "step_index": 2,
                "source": "MODEL",
                "type": "PLANNER_RESPONSE",
                "status": "DONE",
                "created_at": "2026-11-09T03:10:05Z",
                "output_tokens": 200,
                "content": "Continuing work"
            }),
        ];
        let part1 = initial_steps
            .iter()
            .map(serde_json::to_string)
            .collect::<Result<Vec<_>, _>>()
            .unwrap()
            .join("\n")
            + "\n";
        let head = part1.lines().next();

        let (summary1, cursor1) =
            parse_transcript_with_cursor(None, head, Some(&part1), 0, None, None);
        assert_eq!(summary1.compaction_count, 1);
        assert_eq!(summary1.last_compaction_tokens, Some(256_915));
        let cur1 = cursor1.expect("cursor emitted before latest turn");
        assert_eq!(cur1.version, TRANSCRIPT_CURSOR_VERSION);
        assert_eq!(cur1.compaction_count, 1);
        assert_eq!(cur1.last_compaction_tokens, Some(256_915));

        // Append a second CHECKPOINT + trailing turn and resume from `cur1.offset`
        let more_steps = [
            json!({
                "step_index": 3,
                "source": "SYSTEM",
                "type": "CHECKPOINT",
                "status": "DONE",
                "created_at": "2026-11-09T03:25:00Z",
                "input_tokens": 4100,
                "cache_read_tokens": 254_706,
                "output_tokens": 3100,
                "content": "# Resuming from a compaction"
            }),
            json!({
                "step_index": 4,
                "source": "USER_EXPLICIT",
                "type": "USER_INPUT",
                "status": "DONE",
                "created_at": "2026-11-09T03:26:00Z",
                "content": "Next task"
            }),
        ];
        let full = part1.clone()
            + &more_steps
                .iter()
                .map(serde_json::to_string)
                .collect::<Result<Vec<_>, _>>()
                .unwrap()
                .join("\n")
            + "\n";
        let delta = &full[cur1.offset as usize..];
        let (summary2, cursor2) =
            parse_transcript_with_cursor(Some(&cur1), head, Some(delta), cur1.offset, None, None);
        assert_eq!(summary2.compaction_count, 2);
        assert_eq!(summary2.last_compaction_tokens, Some(258_806));
        assert_eq!(cursor2.unwrap().compaction_count, 2);

        // Stale v1 cursor must be rejected on upgrade
        let mut stale_v1 = cur1;
        stale_v1.version = 1;
        let (summary_stale, _) =
            parse_transcript_with_cursor(Some(&stale_v1), head, Some(&full), 0, None, None);
        assert_eq!(summary_stale.compaction_count, 2);
    }

    #[test]
    fn completed_steps_track_session_min_max_and_cursor_roundtrip() {
        let part1_steps = [
            json!({
                "step_index": 0,
                "source": "USER_EXPLICIT",
                "type": "USER_INPUT",
                "status": "DONE",
                "created_at": "2026-11-09T04:00:00Z",
                "content": "Measure step speed bounds"
            }),
            json!({
                "step_index": 1,
                "source": "MODEL",
                "type": "PLANNER_RESPONSE",
                "status": "DONE",
                "created_at": "2026-11-09T04:00:01Z",
                "output_tokens": 420
            }),
            json!({
                "step_index": 2,
                "source": "MODEL",
                "type": "GENERIC",
                "status": "DONE",
                "created_at": "2026-11-09T04:00:11Z",
                "content": "ok"
            }),
            json!({
                "step_index": 3,
                "source": "MODEL",
                "type": "PLANNER_RESPONSE",
                "status": "DONE",
                "created_at": "2026-11-09T04:00:12Z",
                "output_tokens": 1120
            }),
            json!({
                "step_index": 4,
                "source": "MODEL",
                "type": "GENERIC",
                "status": "DONE",
                "created_at": "2026-11-09T04:00:22Z",
                "content": "ok"
            }),
            json!({
                "step_index": 5,
                "source": "MODEL",
                "type": "PLANNER_RESPONSE",
                "status": "RUNNING",
                "created_at": "2026-11-09T04:00:23Z",
                "content": "Streaming response in progress"
            }),
        ];
        let part1 = part1_steps
            .iter()
            .map(serde_json::to_string)
            .collect::<Result<Vec<_>, _>>()
            .unwrap()
            .join("\n")
            + "\n";
        let head = part1.lines().next();

        let (summary1, cursor1) =
            parse_transcript_with_cursor(None, head, Some(&part1), 0, None, None);
        assert_eq!(summary1.session_min_tps_x10, Some(420));
        assert_eq!(summary1.session_max_tps_x10, Some(1120));

        let cur1 = cursor1.expect("cursor emitted before in-flight step 5");
        assert_eq!(cur1.session_min_tps_x10, Some(420));
        assert_eq!(cur1.session_max_tps_x10, Some(1120));

        let cursor_json = serde_json::to_string(&cur1).unwrap();
        let decoded_cur1: TranscriptCursor = serde_json::from_str(&cursor_json).unwrap();
        assert_eq!(decoded_cur1, cur1);

        // Legacy cursor JSON without sessionMinTpsX10/sessionMaxTpsX10 deserializes cleanly
        let legacy_json = r#"{"version":2,"offset":0}"#;
        let legacy_cur: TranscriptCursor = serde_json::from_str(legacy_json).unwrap();
        assert_eq!(legacy_cur.session_min_tps_x10, None);
        assert_eq!(legacy_cur.session_max_tps_x10, None);

        // Resume incrementally from `decoded_cur1.offset` with a middle-speed step (80 tok/s)
        // and a faster step (150 tok/s), plus an in-flight step that must not affect min/max.
        let committed_prefix = &part1[..decoded_cur1.offset as usize];
        let tail_steps = [
            json!({
                "step_index": 5,
                "source": "MODEL",
                "type": "PLANNER_RESPONSE",
                "status": "DONE",
                "created_at": "2026-11-09T04:00:23Z",
                "output_tokens": 800
            }),
            json!({
                "step_index": 6,
                "source": "MODEL",
                "type": "GENERIC",
                "status": "DONE",
                "created_at": "2026-11-09T04:00:33Z",
                "content": "ok"
            }),
            json!({
                "step_index": 7,
                "source": "MODEL",
                "type": "PLANNER_RESPONSE",
                "status": "DONE",
                "created_at": "2026-11-09T04:00:34Z",
                "output_tokens": 1500
            }),
            json!({
                "step_index": 8,
                "source": "MODEL",
                "type": "GENERIC",
                "status": "DONE",
                "created_at": "2026-11-09T04:00:44Z",
                "content": "ok"
            }),
            json!({
                "step_index": 9,
                "source": "MODEL",
                "type": "PLANNER_RESPONSE",
                "status": "RUNNING",
                "created_at": "2026-11-09T04:00:45Z",
                "content": "Still streaming"
            }),
        ];
        let delta = tail_steps
            .iter()
            .map(serde_json::to_string)
            .collect::<Result<Vec<_>, _>>()
            .unwrap()
            .join("\n")
            + "\n";
        let full = format!("{committed_prefix}{delta}");
        let (summary2, cursor2) = parse_transcript_with_cursor(
            Some(&decoded_cur1),
            head,
            Some(&full[decoded_cur1.offset as usize..]),
            decoded_cur1.offset,
            None,
            None,
        );
        assert_eq!(summary2.session_min_tps_x10, Some(420));
        assert_eq!(summary2.session_max_tps_x10, Some(1500));
        let cur2 = cursor2.expect("cursor advances past completed steps 5..=8");
        assert_eq!(cur2.session_min_tps_x10, Some(420));
        assert_eq!(cur2.session_max_tps_x10, Some(1500));
    }
}
