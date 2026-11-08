export type Language = 'en' | 'zh-Hans';
export type LineLayout = 'expanded' | 'compact' | 'adaptive';
export type ContextValueMode = 'percent' | 'tokens' | 'remaining' | 'both';
export type QuotaValueMode = 'used' | 'remaining';
export type ModelFormatMode = 'full' | 'compact' | 'short';
export type EffortFormatMode = 'text' | 'symbol' | 'hidden';
export type PathLevels = 1 | 2 | 3 | 'full';

/**
 * Official Antigravity CLI (`agy`) statusLine stdin JSON payload schema
 * Reference: https://antigravity.google/docs/cli/statusline/
 */
export interface AgyStdinData {
  cwd?: string;
  session_id?: string;
  conversation_id?: string;
  conversation_title?: string;
  transcript_path?: string;
  model?: {
    id?: string;
    display_name?: string;
    effort?: string;
  } | null;
  workspace?: {
    current_dir?: string;
    project_dir?: string;
  } | null;
  version?: string;
  context_window?: {
    total_input_tokens?: number | null;
    total_output_tokens?: number | null;
    context_window_size?: number | null;
    used_percentage?: number | null;
    remaining_percentage?: number | null;
    current_usage?: {
      input_tokens?: number | null;
      output_tokens?: number | null;
      cache_creation_input_tokens?: number | null;
      cache_read_input_tokens?: number | null;
    } | null;
  } | null;
  exceeds_200k_tokens?: boolean | null;
  agent?: {
    name?: string;
  } | null;
  product?: string;
  quota?: Record<
    string,
    {
      remaining_fraction?: number | null;
      remaining_amount?: number | null;
      reset_time?: string | null;
      reset_in_seconds?: number | null;
      disabled?: boolean | null;
    }
  > | null;
  agent_state?:
    | 'idle'
    | 'thinking'
    | 'working'
    | 'tool_use'
    | 'reviewing'
    | 'authenticating'
    | 'initializing'
    | 'error'
    | string;
  vcs?: {
    type?: 'git' | 'jj' | 'hg' | string;
    branch?: string;
    dirty?: boolean;
  } | null;
  cycle_mode?: 'accept-edits' | 'auto' | 'plan' | string;
  sandbox?: {
    enabled?: boolean;
  } | null;
  subagents?: Array<{
    name?: string;
    role?: string;
    status?: 'running' | 'completed' | string;
  }> | null;
  artifact_count?: number;
  plan_tier?: string;
  email?: string;
  pending_input_count?: number;
  tool_confirmation_pending?: boolean;
  task_count?: number;
  terminal_width?: number;
  vim?: {
    mode?: 'NORMAL' | 'INSERT' | 'VISUAL' | 'VISUAL LINE' | string;
  } | null;
  cost?: {
    total_usd?: number | null;
    subagent_usd?: number | null;
    estimated?: boolean | null;
  } | null;
}

export interface ToolEntry {
  id: string;
  name: string;
  target?: string;
  status: 'running' | 'completed' | 'error';
  count: number;
  startedAt?: number;
  endedAt?: number;
}

export interface SubagentEntry {
  name: string;
  role?: string;
  model?: string;
  status: 'running' | 'completed';
  startedAt?: number;
  endedAt?: number;
}

export interface TodoItem {
  text: string;
  status: 'pending' | 'in_progress' | 'completed';
}

export interface TranscriptSummary {
  tools: ToolEntry[];
  skills: string[];
  mcpServers: string[];
  mcpErrors: string[];
  subagents: SubagentEntry[];
  todos: TodoItem[];
  sessionStartedAt: number | null;
  lastStepAt: number | null;
  stepCount: number;
}

export interface VcsProbeInfo {
  type: 'git' | 'jj' | 'hg';
  branch: string;
  dirty: boolean;
  ahead: number;
  behind: number;
  modified: number;
  added: number;
  deleted: number;
  untracked: number;
  conflicts: number;
  repoUrl?: string | null;
}

export interface MemoryProbeInfo {
  totalBytes: number;
  usedBytes: number;
  usedPercent: number;
}

export interface QuotaBucketView {
  id: string;
  label: string;
  usedPercent: number | null;
  remainingPercent: number | null;
  remainingAmount: number | null;
  resetInSeconds: number | null;
}

export interface LogicPaths {
  data: string | null;
  config: string;
  undo: string;
  heartbeat: string;
  lastSnapshot: string | null;
  cliSettings: string;
  transcript: string | null;
  taskMd: string | null;
  cwd: string | null;
}

export interface ProbeAnswers {
  vcs?: string | null;
  memory?: string | null;
}

export interface HeartbeatInfo {
  at: number;
  runtime: string;
  path: string;
  outcome: string;
  columns: number | null;
  durationMs?: number;
}

export interface DoctorFacts {
  cwd: string;
  runtime: {
    version: string;
    execPath: string;
    supported: boolean;
  };
  plugin: {
    root: string;
    version: string;
  };
  configPath: string;
  heartbeat: HeartbeatInfo | null;
}

export interface CageInput {
  mode: 'render' | 'command';
  stdin: AgyStdinData | null;
  now: number;
  columns: number | null;
  paths: LogicPaths;
  command?: {
    name: string;
    args: string[];
    facts: DoctorFacts;
  };
}

export interface CageOutput {
  lines: string[];
  failed?: boolean;
  configToWrite?: Record<string, unknown>;
  snapshotToWrite?: Record<string, unknown>;
}
