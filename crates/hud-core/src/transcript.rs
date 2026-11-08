use crate::ansi::sanitize_text;
use crate::types::{
    AgyStdinData, SubagentEntry, SubagentStatus, TodoItem, TodoStatus, ToolEntry, ToolStatus,
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
    content: Option<Value>,
    tool_calls: Option<Vec<RawToolCall>>,
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

fn char_slice(s: &str, max_chars: usize) -> String {
    s.chars().take(max_chars).collect()
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
            return Some(char_slice(&summary, 28));
        }
        let cmd = unwrap_arg_value(args.get("CommandLine"));
        if !cmd.is_empty() {
            let short = cmd.split_whitespace().take(2).collect::<Vec<_>>().join(" ");
            return Some(char_slice(&short, 24));
        }
        return None;
    }
    if name == "search_web" {
        let q = unwrap_arg_value(args.get("query"));
        return (!q.is_empty()).then(|| char_slice(&q, 24));
    }
    if name == "grep_search" || name == "find_by_name" {
        let q = unwrap_arg_value(args.get("Query").or_else(|| args.get("Pattern")));
        return (!q.is_empty()).then(|| char_slice(&q, 24));
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
        return (!role.is_empty()).then(|| char_slice(&role, 26));
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

fn extract_slash_command(input: &str) -> Option<String> {
    for token in input.split_whitespace() {
        if let Some(cmd_raw) = token.strip_prefix('/') {
            let len = cmd_raw.len();
            if !(2..=33).contains(&len) {
                continue;
            }
            let bytes = cmd_raw.as_bytes();
            if !bytes[0].is_ascii_alphanumeric() {
                continue;
            }
            if !bytes
                .iter()
                .all(|b| b.is_ascii_alphanumeric() || *b == b'_' || *b == b'-')
            {
                continue;
            }
            let lower = cmd_raw.to_ascii_lowercase();
            if !matches!(
                lower.as_str(),
                "exit" | "quit" | "help" | "clear" | "config" | "model" | "statusline"
            ) {
                return Some(lower);
            }
        }
    }
    None
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

/// Pure parser for Antigravity `transcript.jsonl` (head first-line + bounded tail lines) and `task.md`.
pub fn parse_transcript_and_tasks(
    head_line: Option<&str>,
    tail_text: Option<&str>,
    task_md_content: Option<&str>,
    stdin: Option<&AgyStdinData>,
) -> TranscriptSummary {
    let todos = parse_task_markdown(task_md_content);

    let mut session_started_at = head_line
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .and_then(|l| serde_json::from_str::<RawStep>(l).ok())
        .and_then(|s| s.created_at)
        .and_then(|iso| parse_iso_ms(&iso));

    let mut steps = Vec::new();
    if let Some(tail) = tail_text {
        for line in tail.lines() {
            let trimmed = line.trim();
            if trimmed.is_empty() {
                continue;
            }
            if let Ok(step) = serde_json::from_str::<RawStep>(trimmed) {
                steps.push(step);
            }
        }
    }

    if session_started_at.is_none()
        && let Some(first) = steps.first()
        && let Some(iso) = first.created_at.as_deref()
    {
        session_started_at = parse_iso_ms(iso);
    }

    let mut skills_vec = Vec::new();
    let mut skills_seen = BTreeSet::new();
    let mut mcp_vec = Vec::new();
    let mut mcp_seen = BTreeSet::new();
    let mut mcp_err_vec = Vec::new();
    let mut mcp_err_seen = BTreeSet::new();

    let mut raw_tools: Vec<ToolEntry> = Vec::new();
    let mut transcript_agents: Vec<SubagentEntry> = Vec::new();
    let mut last_step_at: Option<i64> = None;
    let mut max_step_idx: usize = 0;

    let is_idle = stdin
        .and_then(|s| s.agent_state.as_deref())
        .is_some_and(|st| st.eq_ignore_ascii_case("idle"));

    for i in 0..steps.len() {
        let step = &steps[i];
        let ts = step.created_at.as_deref().and_then(parse_iso_ms);
        if ts.is_some() {
            last_step_at = ts;
        }
        if let Some(idx) = step.step_index
            && idx + 1 > max_step_idx
        {
            max_step_idx = idx + 1;
        }

        let step_type = step.step_type.as_deref().unwrap_or("");
        if step_type == "USER_INPUT"
            && let Some(Value::String(text)) = step.content.as_ref()
            && let Some(cmd) = extract_slash_command(text)
            && skills_seen.insert(cmd.clone())
        {
            skills_vec.push(cmd);
        }

        if step_type == "PLANNER_RESPONSE"
            && let Some(tool_calls) = step.tool_calls.as_ref()
            && !tool_calls.is_empty()
        {
            let mut results: Vec<&RawStep> = Vec::new();
            for next_step in &steps[i + 1..] {
                if results.len() >= tool_calls.len() {
                    break;
                }
                let nt = next_step.step_type.as_deref().unwrap_or("");
                if nt == "PLANNER_RESPONSE" || nt == "USER_INPUT" {
                    break;
                }
                if nt == "GENERIC" {
                    results.push(next_step);
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
                    let content_str = res.content.as_ref().and_then(Value::as_str).unwrap_or("");
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
                } else if is_idle {
                    ToolStatus::Completed
                } else {
                    ToolStatus::Running
                };

                if raw_name == "read_file"
                    && let Some(a) = args
                    && let Some(skill) = detect_skill_from_path(&unwrap_arg_value(a.get("Path")))
                    && skills_seen.insert(skill.clone())
                {
                    skills_vec.push(skill);
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
                    if mcp_seen.insert(mcp_server.clone()) {
                        mcp_vec.push(mcp_server.clone());
                    }
                    if status == ToolStatus::Error && mcp_err_seen.insert(mcp_server.clone()) {
                        mcp_err_vec.push(mcp_server);
                    }
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
                    transcript_agents.push(SubagentEntry {
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
                raw_tools.push(ToolEntry {
                    id: format!("{}:{idx}", step.step_index.unwrap_or(i)),
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

    // Fold completed tools by name (iterating newest-first) while keeping running/error prominent
    let mut aggregated: Vec<ToolEntry> = Vec::new();
    let mut completed_idx_by_name: HashMap<String, usize> = HashMap::new();

    for item in raw_tools.into_iter().rev() {
        if matches!(item.status, ToolStatus::Running | ToolStatus::Error) {
            aggregated.push(item);
        } else if let Some(&agg_idx) = completed_idx_by_name.get(&item.name) {
            let existing = &mut aggregated[agg_idx];
            existing.count += 1;
            if existing.target.is_some() && existing.target != item.target {
                existing.target = None;
            }
        } else {
            let idx = aggregated.len();
            completed_idx_by_name.insert(item.name.clone(), idx);
            aggregated.push(item);
        }
    }

    aggregated.sort_by_key(|t| t.status);

    // Merge authoritative live `stdin.subagents` with transcript subagent timestamps/models
    let mut merged_subagents = Vec::new();
    if let Some(live_agents) = stdin.and_then(|s| s.subagents.as_ref())
        && !live_agents.is_empty()
    {
        for (idx, sa) in live_agents.iter().enumerate() {
            let matched = transcript_agents.get(idx);
            let name = {
                let candidate = sa
                    .name
                    .as_deref()
                    .filter(|s| !s.trim().is_empty())
                    .or_else(|| matched.map(|m| m.name.as_str()))
                    .unwrap_or("subagent");
                let clean = sanitize_text(candidate);
                if clean.is_empty() {
                    "subagent".to_string()
                } else {
                    clean
                }
            };
            let role = sa
                .role
                .as_deref()
                .filter(|s| !s.trim().is_empty())
                .map(sanitize_text)
                .filter(|s| !s.is_empty())
                .or_else(|| matched.and_then(|m| m.role.clone()));
            merged_subagents.push(SubagentEntry {
                name,
                role,
                model: matched.and_then(|m| m.model.clone()),
                status: if sa.status.as_deref() == Some("running") {
                    SubagentStatus::Running
                } else {
                    SubagentStatus::Completed
                },
                started_at: matched.and_then(|m| m.started_at),
                ended_at: matched.and_then(|m| m.ended_at),
            });
        }
    } else {
        merged_subagents = transcript_agents;
    }

    TranscriptSummary {
        tools: aggregated,
        skills: skills_vec,
        mcp_servers: mcp_vec,
        mcp_errors: mcp_err_vec,
        subagents: merged_subagents,
        todos,
        session_started_at,
        last_step_at,
        step_count: if max_step_idx > 0 {
            max_step_idx
        } else {
            steps.len()
        },
    }
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
        assert_eq!(read_tool.target, None);

        assert_eq!(summary.subagents.len(), 1);
        assert_eq!(summary.subagents[0].status, SubagentStatus::Running);
        assert_eq!(summary.subagents[0].role.as_deref(), Some("Spec Reader"));

        assert_eq!(summary.todos.len(), 3);
        assert_eq!(summary.todos[0].text, "Parse stdin JSON");
        assert_eq!(summary.todos[0].status, TodoStatus::Completed);
        assert_eq!(summary.todos[1].status, TodoStatus::InProgress);
        assert_eq!(summary.todos[2].status, TodoStatus::Pending);
    }
}
