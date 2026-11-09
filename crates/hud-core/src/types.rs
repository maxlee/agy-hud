use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::collections::BTreeMap;

pub const HUD_VERSION: &str = env!("CARGO_PKG_VERSION");

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum Language {
    #[serde(rename = "en")]
    #[default]
    En,
    #[serde(rename = "zh-Hans")]
    ZhHans,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum LineLayout {
    Expanded,
    Compact,
    #[default]
    Adaptive,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum ContextValueMode {
    Percent,
    Tokens,
    Remaining,
    #[default]
    Both,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum QuotaValueMode {
    #[default]
    Used,
    Remaining,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum ModelFormatMode {
    Full,
    #[default]
    Compact,
    Short,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum EffortFormatMode {
    #[default]
    Text,
    Symbol,
    Hidden,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum GlyphMode {
    #[default]
    Unicode,
    Nerd,
    Ascii,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PathLevels {
    #[default]
    One,
    Two,
    Three,
    Full,
}

impl PathLevels {
    pub fn as_count(self) -> Option<usize> {
        match self {
            Self::One => Some(1),
            Self::Two => Some(2),
            Self::Three => Some(3),
            Self::Full => None,
        }
    }
}

impl Serialize for PathLevels {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self {
            Self::One => serializer.serialize_u64(1),
            Self::Two => serializer.serialize_u64(2),
            Self::Three => serializer.serialize_u64(3),
            Self::Full => serializer.serialize_str("full"),
        }
    }
}

impl<'de> Deserialize<'de> for PathLevels {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let v = serde_json::Value::deserialize(deserializer)?;
        match v {
            serde_json::Value::Number(n) => match n.as_u64() {
                Some(1) => Ok(Self::One),
                Some(2) => Ok(Self::Two),
                Some(3) => Ok(Self::Three),
                _ => Err(serde::de::Error::custom(
                    "pathLevels must be 1, 2, 3, or \"full\"",
                )),
            },
            serde_json::Value::String(s) if s == "full" => Ok(Self::Full),
            _ => Err(serde::de::Error::custom(
                "pathLevels must be 1, 2, 3, or \"full\"",
            )),
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ModelStdin {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub effort: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct WorkspaceStdin {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub current_dir: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub project_dir: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CurrentUsageStdin {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub input_tokens: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output_tokens: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cache_creation_input_tokens: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cache_read_input_tokens: Option<u64>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ContextWindowStdin {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub total_input_tokens: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub total_output_tokens: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub context_window_size: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub used_percentage: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub remaining_percentage: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub current_usage: Option<CurrentUsageStdin>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AgentStdin {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct RawQuotaBucket {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub remaining_fraction: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub remaining_amount: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reset_time: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reset_in_seconds: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub disabled: Option<bool>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct VcsStdin {
    #[serde(rename = "type", skip_serializing_if = "Option::is_none")]
    pub vcs_type: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub branch: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dirty: Option<bool>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SandboxStdin {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub enabled: Option<bool>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SubagentStdin {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct VimStdin {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mode: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CostStdin {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub total_usd: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub subagent_usd: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub estimated: Option<bool>,
}

/// Official Antigravity CLI (`agy`) `statusLine` stdin JSON payload schema.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AgyStdinData {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub conversation_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub conversation_title: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub transcript_path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<ModelStdin>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub workspace: Option<WorkspaceStdin>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub context_window: Option<ContextWindowStdin>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub exceeds_200k_tokens: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub agent: Option<AgentStdin>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub product: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub quota: Option<BTreeMap<String, RawQuotaBucket>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub agent_state: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub vcs: Option<VcsStdin>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cycle_mode: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sandbox: Option<SandboxStdin>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub subagents: Option<Vec<SubagentStdin>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub artifact_count: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub plan_tier: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub email: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pending_input_count: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_confirmation_pending: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub task_count: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub terminal_width: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub vim: Option<VimStdin>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cost: Option<CostStdin>,
    #[serde(flatten, skip_serializing_if = "BTreeMap::is_empty", default)]
    pub extra: BTreeMap<String, serde_json::Value>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ToolStatus {
    Running,
    Error,
    Completed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolEntry {
    pub id: String,
    pub name: String,
    pub target: Option<String>,
    pub status: ToolStatus,
    pub count: usize,
    pub started_at: Option<i64>,
    pub ended_at: Option<i64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SubagentStatus {
    Running,
    Completed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SubagentEntry {
    pub name: String,
    pub role: Option<String>,
    pub model: Option<String>,
    pub status: SubagentStatus,
    pub started_at: Option<i64>,
    pub ended_at: Option<i64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TodoStatus {
    Pending,
    InProgress,
    Completed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TodoItem {
    pub text: String,
    pub status: TodoStatus,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PlannerStepTiming {
    pub step_index: usize,
    pub output_tokens: u64,
    pub started_at_ms: i64,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub ended_at_ms: Option<i64>,
    #[serde(default)]
    pub has_trailing_user_input: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TranscriptSummary {
    pub tools: Vec<ToolEntry>,
    pub skills: Vec<String>,
    pub mcp_servers: Vec<String>,
    pub mcp_errors: Vec<String>,
    pub subagents: Vec<SubagentEntry>,
    pub todos: Vec<TodoItem>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub active_slash_mode: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub completed_step_tps_x10: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub latest_planner_step: Option<PlannerStepTiming>,
    pub session_started_at: Option<i64>,
    pub last_step_at: Option<i64>,
    pub step_count: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum VcsType {
    Git,
    Jj,
    Hg,
}

impl VcsType {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Git => "git",
            Self::Jj => "jj",
            Self::Hg => "hg",
        }
    }

    pub fn from_loose(s: &str) -> Self {
        match s.trim().to_ascii_lowercase().as_str() {
            "jj" => Self::Jj,
            "hg" => Self::Hg,
            _ => Self::Git,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VcsProbeInfo {
    #[serde(rename = "type")]
    pub vcs_type: VcsType,
    pub branch: String,
    pub dirty: bool,
    #[serde(default)]
    pub ahead: u64,
    #[serde(default)]
    pub behind: u64,
    #[serde(default)]
    pub modified: u64,
    #[serde(default)]
    pub added: u64,
    #[serde(default)]
    pub deleted: u64,
    #[serde(default)]
    pub untracked: u64,
    #[serde(default)]
    pub conflicts: u64,
    #[serde(default)]
    pub repo_url: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MemoryProbeInfo {
    pub total_bytes: u64,
    pub used_bytes: u64,
    pub used_percent: u64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct QuotaBucketView {
    pub id: String,
    pub label: String,
    pub used_percent: Option<f64>,
    pub remaining_percent: Option<f64>,
    pub remaining_amount: Option<f64>,
    pub reset_in_seconds: Option<f64>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CachedQuotaSnapshot {
    pub at: i64,
    pub source: String,
    #[serde(default)]
    pub quota: BTreeMap<String, RawQuotaBucket>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateCacheSnapshot {
    pub checked_at: i64,
    pub latest_version: String,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub release_url: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SpeedCacheSnapshot {
    pub session_key: String,
    pub step_index: usize,
    pub output_tokens: u64,
    pub started_at_ms: i64,
    pub ended_at_ms: i64,
    pub tps_x10: u32,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ProbeAnswers {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub vcs: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub memory: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub quota_cache: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub update_cache: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub speed_cache: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HeartbeatInfo {
    pub at: i64,
    pub runtime: String,
    pub path: String,
    pub outcome: String,
    pub columns: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RuntimeFacts {
    pub engine: String,
    pub version: String,
    pub exec_path: String,
    pub supported: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PluginFacts {
    pub root: String,
    pub version: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InstalledPluginFacts {
    pub version: String,
    pub path: String,
    pub enabled: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DoctorFacts {
    pub cwd: String,
    #[serde(default)]
    pub home_dir: String,
    #[serde(default)]
    pub platform: String,
    #[serde(default)]
    pub terminal: String,
    #[serde(default)]
    pub remote_session: bool,
    pub runtime: RuntimeFacts,
    pub plugin: PluginFacts,
    #[serde(default)]
    pub installed_plugin_status: String,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub installed_plugin: Option<InstalledPluginFacts>,
    pub config_path: String,
    pub heartbeat: Option<HeartbeatInfo>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LogicPaths {
    pub data: Option<String>,
    pub config: String,
    pub undo: String,
    pub heartbeat: String,
    pub last_snapshot: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub quota_cache: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub update_cache: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub speed_cache: Option<String>,
    pub cli_settings: String,
    pub transcript: Option<String>,
    pub task_md: Option<String>,
    pub cwd: Option<String>,
}

#[derive(Debug, Clone, Default)]
pub struct CommandOutput {
    pub lines: Vec<String>,
    pub failed: bool,
    pub config_to_write: Option<serde_json::Map<String, serde_json::Value>>,
}
