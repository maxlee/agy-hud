use crate::types::{
    ContextValueMode, EffortFormatMode, GlyphMode, Language, LineLayout, ModelFormatMode,
    PathLevels, QuotaValueMode,
};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

pub const MAX_CONFIG_BYTES: usize = 64 * 1024;
const MAX_DEPTH: usize = 6;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct QuotaConfig {
    #[serde(default)]
    pub background_refresh: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdatesConfig {
    pub enabled: bool,
}

impl Default for UpdatesConfig {
    fn default() -> Self {
        Self { enabled: true }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VcsConfig {
    pub enabled: bool,
    pub show_dirty: bool,
    pub show_ahead_behind: bool,
    pub show_file_stats: bool,
    pub cache_ttl_ms: u64,
    #[serde(default)]
    pub push_warning_threshold: u64,
    #[serde(default)]
    pub push_critical_threshold: u64,
}

impl Default for VcsConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            show_dirty: true,
            show_ahead_behind: true,
            show_file_stats: false,
            cache_ttl_ms: 1500,
            push_warning_threshold: 0,
            push_critical_threshold: 0,
        }
    }
}

fn default_true() -> bool {
    true
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DisplayConfig {
    pub show_state: bool,
    pub show_mode: bool,
    pub show_model: bool,
    pub model_format: ModelFormatMode,
    pub effort_format: EffortFormatMode,
    pub show_project: bool,
    #[serde(default)]
    pub show_launch_dir: bool,
    #[serde(default)]
    pub show_title: bool,
    #[serde(default)]
    pub glyph_mode: GlyphMode,
    pub show_context_bar: bool,
    pub bar_length: usize,
    pub context_value: ContextValueMode,
    pub context_warning_threshold: u64,
    pub context_critical_threshold: u64,
    pub show_cache_hit_rate: bool,
    pub show_quota: bool,
    pub quota_value: QuotaValueMode,
    pub quota_bar_enabled: bool,
    pub show_quota_reset: bool,
    #[serde(default = "default_true")]
    pub quota_pace: bool,
    #[serde(default)]
    pub quota_threshold: u64,
    pub show_cost: bool,
    #[serde(default)]
    pub show_daily_cost: bool,
    #[serde(default)]
    pub show_weekly_cost: bool,
    pub show_sandbox: bool,
    pub show_counts: bool,
    pub show_plan_tier: bool,
    #[serde(default = "default_true")]
    pub show_speed: bool,
    pub show_memory: bool,
    pub show_duration: bool,
    pub show_tools: bool,
    pub tools_max_visible: usize,
    pub show_skills_and_mcp: bool,
    pub show_subagents: bool,
    pub show_todos: bool,
    pub custom_tag: String,
    #[serde(default)]
    pub merge_groups: Vec<Vec<String>>,
    #[serde(default)]
    pub right_align: Vec<String>,
}

impl Default for DisplayConfig {
    fn default() -> Self {
        Self {
            show_state: true,
            show_mode: true,
            show_model: true,
            model_format: ModelFormatMode::Compact,
            effort_format: EffortFormatMode::Text,
            show_project: true,
            show_launch_dir: false,
            show_title: false,
            glyph_mode: GlyphMode::Unicode,
            show_context_bar: true,
            bar_length: 12,
            context_value: ContextValueMode::Both,
            context_warning_threshold: 60,
            context_critical_threshold: 85,
            show_cache_hit_rate: true,
            show_quota: true,
            quota_value: QuotaValueMode::Used,
            quota_bar_enabled: true,
            show_quota_reset: true,
            quota_pace: true,
            quota_threshold: 0,
            show_cost: true,
            show_daily_cost: false,
            show_weekly_cost: false,
            show_sandbox: true,
            show_counts: true,
            show_plan_tier: true,
            show_speed: true,
            show_memory: false,
            show_duration: true,
            show_tools: true,
            tools_max_visible: 3,
            show_skills_and_mcp: true,
            show_subagents: true,
            show_todos: true,
            custom_tag: String::new(),
            merge_groups: Vec::new(),
            right_align: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HudConfig {
    pub language: Language,
    pub line_layout: LineLayout,
    pub path_levels: PathLevels,
    pub max_width: Option<usize>,
    pub hyperlinks: bool,
    #[serde(default)]
    pub quota: QuotaConfig,
    #[serde(default)]
    pub updates: UpdatesConfig,
    pub vcs: VcsConfig,
    pub display: DisplayConfig,
}

impl Default for HudConfig {
    fn default() -> Self {
        Self {
            language: Language::En,
            line_layout: LineLayout::Adaptive,
            path_levels: PathLevels::One,
            max_width: None,
            hyperlinks: true,
            quota: QuotaConfig::default(),
            updates: UpdatesConfig::default(),
            vcs: VcsConfig::default(),
            display: DisplayConfig::default(),
        }
    }
}

pub const MERGEABLE_ROWS: &[&str] = &[
    "header",
    "context",
    "quota",
    "meta",
    "tools",
    "skills",
    "subagents",
    "todos",
];

pub const RIGHT_ALIGN_SEGMENTS: &[&str] = &[
    "duration",
    "cost",
    "dailyCost",
    "weeklyCost",
    "memory",
    "counts",
    "sandbox",
    "customTag",
    "quota",
    "meta",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SchemaValueKind {
    Boolean,
    String,
    Enum(&'static [&'static str]),
    Number { min: i64, max: i64 },
    NullableNumber { min: i64, max: i64 },
    StringArray(&'static [&'static str]),
    MergeGroups(&'static [&'static str]),
}

impl SchemaValueKind {
    pub fn type_name(self) -> &'static str {
        match self {
            Self::Boolean => "boolean",
            Self::String => "string",
            Self::Enum(_) => "enum",
            Self::Number { .. } => "number",
            Self::NullableNumber { .. } => "number|null",
            Self::StringArray(_) => "string[]",
            Self::MergeGroups(_) => "string[][]",
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub struct SchemaEntry {
    pub key: &'static str,
    pub kind: SchemaValueKind,
    pub description: &'static str,
}

pub const CONFIG_SCHEMA: &[SchemaEntry] = &[
    SchemaEntry {
        key: "language",
        kind: SchemaValueKind::Enum(&["\"en\"", "\"zh-Hans\""]),
        description: "HUD display language (\"en\" or \"zh-Hans\").",
    },
    SchemaEntry {
        key: "lineLayout",
        kind: SchemaValueKind::Enum(&["\"adaptive\"", "\"expanded\"", "\"compact\""]),
        description: "Layout mode: adaptive (auto by terminal_width), expanded (multi-line), or compact (dense).",
    },
    SchemaEntry {
        key: "pathLevels",
        kind: SchemaValueKind::Enum(&["1", "2", "3", "\"full\""]),
        description: "Number of trailing directory segments to show for the workspace path.",
    },
    SchemaEntry {
        key: "maxWidth",
        kind: SchemaValueKind::NullableNumber { min: 40, max: 400 },
        description: "Optional hard column cap (null uses live stdin.terminal_width).",
    },
    SchemaEntry {
        key: "hyperlinks",
        kind: SchemaValueKind::Boolean,
        description: "Emit clickable OSC 8 file:// links for workspace and tool targets.",
    },
    SchemaEntry {
        key: "quota.backgroundRefresh",
        kind: SchemaValueKind::Boolean,
        description: "Refresh stale quota in background via read-only `agy -p /usage` (off by default).",
    },
    SchemaEntry {
        key: "updates.enabled",
        kind: SchemaValueKind::Boolean,
        description: "Check GitHub Releases in background every 6h and show `/agy-hud:hud-update` badge when a newer version is available.",
    },
    SchemaEntry {
        key: "vcs.enabled",
        kind: SchemaValueKind::Boolean,
        description: "Show git/jj/hg branch and repository state.",
    },
    SchemaEntry {
        key: "vcs.showDirty",
        kind: SchemaValueKind::Boolean,
        description: "Show dirty indicator (*) when working tree has uncommitted changes.",
    },
    SchemaEntry {
        key: "vcs.showAheadBehind",
        kind: SchemaValueKind::Boolean,
        description: "Show ahead/behind commit arrows (↑N↓M) from VCS probe.",
    },
    SchemaEntry {
        key: "vcs.showFileStats",
        kind: SchemaValueKind::Boolean,
        description: "Show modified/added/deleted file counts (+A ~M -D).",
    },
    SchemaEntry {
        key: "vcs.cacheTtlMs",
        kind: SchemaValueKind::Number {
            min: 0,
            max: 60_000,
        },
        description: "VCS probe cache TTL in ms (0 disables caching; auto-invalidates on repo HEAD/index/dirstate mtime changes).",
    },
    SchemaEntry {
        key: "vcs.pushWarningThreshold",
        kind: SchemaValueKind::Number { min: 0, max: 500 },
        description: "Unpushed commit count (ahead) at which ↑N turns yellow (0 disables).",
    },
    SchemaEntry {
        key: "vcs.pushCriticalThreshold",
        kind: SchemaValueKind::Number { min: 0, max: 500 },
        description: "Unpushed commit count (ahead) at which ↑N turns red (0 disables).",
    },
    SchemaEntry {
        key: "display.showState",
        kind: SchemaValueKind::Boolean,
        description: "Show real-time agent_state badge (READY, WORKING, TOOL, CONFIRM?, etc.).",
    },
    SchemaEntry {
        key: "display.showMode",
        kind: SchemaValueKind::Boolean,
        description: "Show Shift+Tab cycle_mode (PLAN/AUTO/ACCEPT) and Vim mode.",
    },
    SchemaEntry {
        key: "display.showModel",
        kind: SchemaValueKind::Boolean,
        description: "Show active model name and reasoning effort.",
    },
    SchemaEntry {
        key: "display.modelFormat",
        kind: SchemaValueKind::Enum(&["\"full\"", "\"compact\"", "\"short\""]),
        description: "Model name verbosity: full (\"Gemini 4 Argon (Medium)\"), compact (\"Gemini 4 Argon\"), short (\"Argon\").",
    },
    SchemaEntry {
        key: "display.effortFormat",
        kind: SchemaValueKind::Enum(&["\"text\"", "\"symbol\"", "\"hidden\""]),
        description: "Effort display style: text (\"med\"), symbol (\"⚡⚡\"), or hidden.",
    },
    SchemaEntry {
        key: "display.showProject",
        kind: SchemaValueKind::Boolean,
        description: "Show workspace folder path.",
    },
    SchemaEntry {
        key: "display.showLaunchDir",
        kind: SchemaValueKind::Boolean,
        description: "Show launch directory breadcrumb (project_dir › current_dir) when working in a nested or external folder.",
    },
    SchemaEntry {
        key: "display.showTitle",
        kind: SchemaValueKind::Boolean,
        description: "Show sanitized conversation_title in header (up to 24 cols; dropped first on narrow terminals).",
    },
    SchemaEntry {
        key: "display.glyphMode",
        kind: SchemaValueKind::Enum(&["\"unicode\"", "\"nerd\"", "\"ascii\""]),
        description: "Icon/glyph set: unicode (default), nerd (Nerd Font icons), or ascii (7-bit text for SSH/TTY).",
    },
    SchemaEntry {
        key: "display.showContextBar",
        kind: SchemaValueKind::Boolean,
        description: "Show Unicode context window progress bar.",
    },
    SchemaEntry {
        key: "display.barLength",
        kind: SchemaValueKind::Number { min: 5, max: 30 },
        description: "Character width of progress bars (5..30).",
    },
    SchemaEntry {
        key: "display.contextValue",
        kind: SchemaValueKind::Enum(&["\"percent\"", "\"tokens\"", "\"remaining\"", "\"both\""]),
        description: "Context metric format: percent (\"14.2%\"), tokens (\"149k/1.0M\"), remaining (\"85.8% left\"), or both.",
    },
    SchemaEntry {
        key: "display.contextWarningThreshold",
        kind: SchemaValueKind::Number { min: 10, max: 99 },
        description: "Percentage at which context bar turns yellow.",
    },
    SchemaEntry {
        key: "display.contextCriticalThreshold",
        kind: SchemaValueKind::Number { min: 20, max: 100 },
        description: "Percentage at which context bar turns red.",
    },
    SchemaEntry {
        key: "display.showCacheHitRate",
        kind: SchemaValueKind::Boolean,
        description: "Show prompt cache read hit percentage when cache tokens are observed.",
    },
    SchemaEntry {
        key: "display.showQuota",
        kind: SchemaValueKind::Boolean,
        description: "Show Antigravity quota buckets from stdin.quota.",
    },
    SchemaEntry {
        key: "display.quotaValue",
        kind: SchemaValueKind::Enum(&["\"used\"", "\"remaining\""]),
        description: "Show quota as percentage used or remaining.",
    },
    SchemaEntry {
        key: "display.quotaBarEnabled",
        kind: SchemaValueKind::Boolean,
        description: "Render mini progress bar next to quota percentage.",
    },
    SchemaEntry {
        key: "display.showQuotaReset",
        kind: SchemaValueKind::Boolean,
        description: "Show countdown until quota bucket resets (e.g., \"6d 11h\").",
    },
    SchemaEntry {
        key: "display.quotaPace",
        kind: SchemaValueKind::Boolean,
        description: "Project quota burn rate against window elapsed time and highlight unsustainable pace (▲).",
    },
    SchemaEntry {
        key: "display.quotaThreshold",
        kind: SchemaValueKind::Number { min: 0, max: 99 },
        description: "Minimum used percentage before displaying a quota bucket unless pace is elevated (0 shows all).",
    },
    SchemaEntry {
        key: "display.showCost",
        kind: SchemaValueKind::Boolean,
        description: "Show estimated session USD spend when reported.",
    },
    SchemaEntry {
        key: "display.showDailyCost",
        kind: SchemaValueKind::Boolean,
        description: "Show aggregated cross-session USD spend for the current UTC day.",
    },
    SchemaEntry {
        key: "display.showWeeklyCost",
        kind: SchemaValueKind::Boolean,
        description: "Show aggregated cross-session USD spend for the current 7-day rolling window.",
    },
    SchemaEntry {
        key: "display.showSandbox",
        kind: SchemaValueKind::Boolean,
        description: "Show terminal sandbox status badge.",
    },
    SchemaEntry {
        key: "display.showCounts",
        kind: SchemaValueKind::Boolean,
        description: "Show counts of artifacts, running subagents, background tasks, and queued inputs.",
    },
    SchemaEntry {
        key: "display.showPlanTier",
        kind: SchemaValueKind::Boolean,
        description: "Show subscription tier badge (e.g. Pro, Ultra).",
    },
    SchemaEntry {
        key: "display.showSpeed",
        kind: SchemaValueKind::Boolean,
        description: "Show estimated output generation throughput (~tok/s) next to model badge.",
    },
    SchemaEntry {
        key: "display.showMemory",
        kind: SchemaValueKind::Boolean,
        description: "Show host RAM usage from system probe.",
    },
    SchemaEntry {
        key: "display.showDuration",
        kind: SchemaValueKind::Boolean,
        description: "Show elapsed conversation duration derived from transcript.",
    },
    SchemaEntry {
        key: "display.showTools",
        kind: SchemaValueKind::Boolean,
        description: "Show active and recent tool invocations line parsed from transcript.jsonl.",
    },
    SchemaEntry {
        key: "display.toolsMaxVisible",
        kind: SchemaValueKind::Number { min: 1, max: 10 },
        description: "Maximum distinct tool entries displayed on the tools activity line.",
    },
    SchemaEntry {
        key: "display.showSkillsAndMcp",
        kind: SchemaValueKind::Boolean,
        description: "Show activated skills and MCP servers line parsed from transcript.jsonl.",
    },
    SchemaEntry {
        key: "display.showSubagents",
        kind: SchemaValueKind::Boolean,
        description: "Show detailed subagent activity line when subagents are active or completed.",
    },
    SchemaEntry {
        key: "display.showTodos",
        kind: SchemaValueKind::Boolean,
        description: "Show checklist task progress parsed from conversation task.md artifact.",
    },
    SchemaEntry {
        key: "display.customTag",
        kind: SchemaValueKind::String,
        description: "Optional custom text label pinned to the header line.",
    },
    SchemaEntry {
        key: "display.mergeGroups",
        kind: SchemaValueKind::MergeGroups(MERGEABLE_ROWS),
        description: "Row groups merged side-by-side when terminal width fits (e.g. [[\"context\",\"quota\"]]); auto-stacks on narrow widths.",
    },
    SchemaEntry {
        key: "display.rightAlign",
        kind: SchemaValueKind::StringArray(RIGHT_ALIGN_SEGMENTS),
        description: "Segments or merged rows pushed flush-right when terminal width allows (e.g. [\"duration\",\"cost\"]).",
    },
];

pub fn find_schema(key: &str) -> Option<&'static SchemaEntry> {
    CONFIG_SCHEMA.iter().find(|s| s.key == key)
}

pub fn validate_setting(key: &str, value: &Value) -> Result<(), String> {
    let Some(entry) = find_schema(key) else {
        return Err(format!(
            "Unknown config key \"{key}\". Run `agy-hud schema` to list valid keys."
        ));
    };

    match entry.kind {
        SchemaValueKind::Boolean => {
            if value.is_boolean() {
                Ok(())
            } else {
                Err(format!("{key} expects a boolean (true or false)."))
            }
        }
        SchemaValueKind::String => match value.as_str() {
            Some(s) if s.chars().count() <= 80 => Ok(()),
            _ => Err(format!("{key} expects a string up to 80 characters.")),
        },
        SchemaValueKind::Enum(allowed_json) => {
            let serialized = serde_json::to_string(value).unwrap_or_default();
            if allowed_json.contains(&serialized.as_str()) {
                Ok(())
            } else {
                Err(format!(
                    "{key} must be one of: {}.",
                    allowed_json.join(", ")
                ))
            }
        }
        SchemaValueKind::Number { min, max } => {
            let Some(n) = value.as_i64() else {
                return Err(format!("{key} expects an integer."));
            };
            if n < min {
                return Err(format!("{key} must be >= {min}."));
            }
            if n > max {
                return Err(format!("{key} must be <= {max}."));
            }
            Ok(())
        }
        SchemaValueKind::NullableNumber { min, max } => {
            if value.is_null() {
                return Ok(());
            }
            let Some(n) = value.as_i64() else {
                return Err(format!("{key} expects an integer or null."));
            };
            if n < min {
                return Err(format!("{key} must be >= {min}."));
            }
            if n > max {
                return Err(format!("{key} must be <= {max}."));
            }
            Ok(())
        }
        SchemaValueKind::StringArray(allowed) => {
            let Some(arr) = value.as_array() else {
                return Err(format!("{key} expects a JSON array of strings."));
            };
            if arr.len() > allowed.len() {
                return Err(format!("{key} accepts at most {} entries.", allowed.len()));
            }
            let mut seen: Vec<&str> = Vec::with_capacity(arr.len());
            for item in arr {
                let Some(s) = item.as_str() else {
                    return Err(format!("{key} expects every array item to be a string."));
                };
                if !allowed.contains(&s) {
                    return Err(format!(
                        "{key} item \"{s}\" must be one of: {}.",
                        allowed.join(", ")
                    ));
                }
                if seen.contains(&s) {
                    return Err(format!("{key} must not contain duplicate item \"{s}\"."));
                }
                seen.push(s);
            }
            Ok(())
        }
        SchemaValueKind::MergeGroups(allowed) => {
            let Some(groups) = value.as_array() else {
                return Err(format!("{key} expects a JSON array of row groups."));
            };
            if groups.len() > 4 {
                return Err(format!("{key} accepts at most 4 merge groups."));
            }
            let mut seen_across_groups: Vec<&str> = Vec::new();
            for group_val in groups {
                let Some(group) = group_val.as_array() else {
                    return Err(format!(
                        "{key} expects each group to be an array of 2..4 row names."
                    ));
                };
                if !(2..=4).contains(&group.len()) {
                    return Err(format!(
                        "{key} expects each group to contain 2..4 row names."
                    ));
                }
                for item in group {
                    let Some(s) = item.as_str() else {
                        return Err(format!("{key} expects every row name to be a string."));
                    };
                    if !allowed.contains(&s) {
                        return Err(format!(
                            "{key} row \"{s}\" must be one of: {}.",
                            allowed.join(", ")
                        ));
                    }
                    if seen_across_groups.contains(&s) {
                        return Err(format!(
                            "{key} row \"{s}\" cannot appear more than once across merge groups."
                        ));
                    }
                    seen_across_groups.push(s);
                }
            }
            Ok(())
        }
    }
}

fn is_unsafe_key(key: &str) -> bool {
    matches!(key, "__proto__" | "prototype" | "constructor")
}

fn sanitize_map(raw: &Map<String, Value>, depth: usize) -> Map<String, Value> {
    if depth > MAX_DEPTH {
        return Map::new();
    }
    let mut clean = Map::new();
    for (k, v) in raw {
        if is_unsafe_key(k) {
            continue;
        }
        let next_v = match v {
            Value::Object(sub) => Value::Object(sanitize_map(sub, depth + 1)),
            other => other.clone(),
        };
        clean.insert(k.clone(), next_v);
    }
    clean
}

pub fn set_dot_value(root: &mut Map<String, Value>, dot_key: &str, val: Value) {
    let parts: Vec<&str> = dot_key.split('.').collect();
    if parts.is_empty() {
        return;
    }
    let mut cursor = root;
    for part in &parts[..parts.len() - 1] {
        let entry = cursor
            .entry((*part).to_string())
            .or_insert_with(|| Value::Object(Map::new()));
        if !entry.is_object() {
            *entry = Value::Object(Map::new());
        }
        cursor = entry.as_object_mut().expect("ensured object above");
    }
    cursor.insert(parts[parts.len() - 1].to_string(), val);
}

pub fn get_dot_value<'a>(root: &'a Value, dot_key: &str) -> Option<&'a Value> {
    let mut cursor = root;
    for part in dot_key.split('.') {
        cursor = cursor.as_object()?.get(part)?;
    }
    Some(cursor)
}

pub fn apply_dot_overrides(
    base: &HudConfig,
    raw_obj: &Map<String, Value>,
) -> (HudConfig, Vec<String>) {
    let mut base_val = serde_json::to_value(base).unwrap_or_else(|_| Value::Object(Map::new()));
    let mut problems = Vec::new();

    fn walk(
        node: &Map<String, Value>,
        prefix: &str,
        target: &mut Map<String, Value>,
        problems: &mut Vec<String>,
    ) {
        for (k, v) in node {
            if is_unsafe_key(k) {
                continue;
            }
            let full_key = if prefix.is_empty() {
                k.clone()
            } else {
                format!("{prefix}.{k}")
            };
            if let Value::Object(sub) = v
                && find_schema(&full_key).is_none()
            {
                walk(sub, &full_key, target, problems);
            } else if let Err(err) = validate_setting(&full_key, v) {
                problems.push(err);
            } else {
                set_dot_value(target, &full_key, v.clone());
            }
        }
    }

    let sanitized = sanitize_map(raw_obj, 0);
    if let Value::Object(ref mut map) = base_val {
        walk(&sanitized, "", map, &mut problems);
    }
    let next_cfg = serde_json::from_value::<HudConfig>(base_val).unwrap_or_else(|_| base.clone());
    (next_cfg, problems)
}

#[derive(Debug, Clone)]
pub struct LoadedConfig {
    pub config: HudConfig,
    pub raw: Map<String, Value>,
    pub problems: Vec<String>,
}

pub fn parse_config_str(raw_text: Option<&str>) -> LoadedConfig {
    let defaults = HudConfig::default();
    let Some(text_raw) = raw_text else {
        return LoadedConfig {
            config: defaults,
            raw: Map::new(),
            problems: Vec::new(),
        };
    };
    if text_raw.len() > MAX_CONFIG_BYTES {
        return LoadedConfig {
            config: defaults,
            raw: Map::new(),
            problems: vec![format!("Config file exceeds {MAX_CONFIG_BYTES} bytes.")],
        };
    }
    let text = text_raw.strip_prefix('\u{feff}').unwrap_or(text_raw);
    if text.trim().is_empty() {
        return LoadedConfig {
            config: defaults,
            raw: Map::new(),
            problems: Vec::new(),
        };
    }
    match serde_json::from_str::<Value>(text) {
        Ok(Value::Object(obj)) => {
            let sanitized = sanitize_map(&obj, 0);
            let (config, problems) = apply_dot_overrides(&defaults, &sanitized);
            LoadedConfig {
                config,
                raw: sanitized,
                problems,
            }
        }
        Ok(_) => LoadedConfig {
            config: defaults,
            raw: Map::new(),
            problems: vec!["Config root must be a JSON object.".to_string()],
        },
        Err(err) => LoadedConfig {
            config: defaults,
            raw: Map::new(),
            problems: vec![format!("Failed to parse config: {err}")],
        },
    }
}

pub fn parse_assignment_value(raw: &str) -> Value {
    serde_json::from_str::<Value>(raw).unwrap_or_else(|_| Value::String(raw.to_string()))
}

#[derive(Debug, Clone)]
pub struct ParsedSet {
    pub config: HudConfig,
    pub assignments: Vec<(String, Value)>,
    pub problems: Vec<String>,
}

pub fn parse_set_pairs(pairs: &[String], base_config: &HudConfig) -> ParsedSet {
    let mut patch = Map::new();
    let mut assignments = Vec::new();
    let mut problems = Vec::new();

    for pair in pairs {
        let Some(eq) = pair.find('=') else {
            problems.push(format!(
                "Invalid assignment \"{pair}\" (expected key=value)."
            ));
            continue;
        };
        if eq == 0 {
            problems.push(format!(
                "Invalid assignment \"{pair}\" (expected key=value)."
            ));
            continue;
        }
        let key = pair[..eq].trim().to_string();
        let val = parse_assignment_value(pair[eq + 1..].trim());
        if let Err(err) = validate_setting(&key, &val) {
            problems.push(err);
            continue;
        }
        set_dot_value(&mut patch, &key, val.clone());
        assignments.push((key, val));
    }

    let (config, merge_problems) = apply_dot_overrides(base_config, &patch);
    problems.extend(merge_problems);
    ParsedSet {
        config,
        assignments,
        problems,
    }
}
