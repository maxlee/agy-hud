use crate::types::Language;

#[derive(Debug, Clone, Copy)]
pub struct StateLabels {
    pub idle: &'static str,
    pub thinking: &'static str,
    pub working: &'static str,
    pub tool_use: &'static str,
    pub reviewing: &'static str,
    pub authenticating: &'static str,
    pub initializing: &'static str,
    pub error: &'static str,
    pub confirm: &'static str,
}

#[derive(Debug, Clone, Copy)]
pub struct ModeLabels {
    pub plan: &'static str,
    pub auto: &'static str,
    pub accept_edits: &'static str,
}

#[derive(Debug, Clone, Copy)]
pub struct MetricLabels {
    pub context: &'static str,
    pub quota: &'static str,
    pub cache: &'static str,
    pub cost: &'static str,
    pub sandbox_on: &'static str,
    pub sandbox_off: &'static str,
    pub artifacts: &'static str,
    pub subagents: &'static str,
    pub tasks: &'static str,
    pub queued: &'static str,
    pub memory: &'static str,
    pub resets_in: &'static str,
    pub remaining: &'static str,
    pub tools: &'static str,
    pub agents: &'static str,
    pub todos: &'static str,
    pub skills: &'static str,
    pub mcp: &'static str,
}

#[derive(Debug, Clone, Copy)]
pub struct Labels {
    pub state: StateLabels,
    pub mode: ModeLabels,
    pub metrics: MetricLabels,
}

pub const EN: Labels = Labels {
    state: StateLabels {
        idle: "READY",
        thinking: "THINKING",
        working: "WORKING",
        tool_use: "TOOL",
        reviewing: "REVIEW",
        authenticating: "AUTH",
        initializing: "INIT",
        error: "ERROR",
        confirm: "CONFIRM?",
    },
    mode: ModeLabels {
        plan: "PLAN",
        auto: "AUTO",
        accept_edits: "ACCEPT",
    },
    metrics: MetricLabels {
        context: "Ctx",
        quota: "Quota",
        cache: "Cache",
        cost: "Cost",
        sandbox_on: "Sandbox ON",
        sandbox_off: "Sandbox off",
        artifacts: "art",
        subagents: "agents",
        tasks: "tasks",
        queued: "queued",
        memory: "RAM",
        resets_in: "resets",
        remaining: "left",
        tools: "Tools",
        agents: "Agents",
        todos: "Tasks",
        skills: "Skills",
        mcp: "MCP",
    },
};

pub const ZH_HANS: Labels = Labels {
    state: StateLabels {
        idle: "就绪",
        thinking: "思考中",
        working: "执行中",
        tool_use: "调用工具",
        reviewing: "审阅中",
        authenticating: "认证中",
        initializing: "初始化",
        error: "异常",
        confirm: "待确认!",
    },
    mode: ModeLabels {
        plan: "规划",
        auto: "自动",
        accept_edits: "接受编辑",
    },
    metrics: MetricLabels {
        context: "上下文",
        quota: "配额",
        cache: "缓存",
        cost: "费用",
        sandbox_on: "沙箱开启",
        sandbox_off: "沙箱关闭",
        artifacts: "产物",
        subagents: "子代理",
        tasks: "后台",
        queued: "排队",
        memory: "内存",
        resets_in: "重置",
        remaining: "剩余",
        tools: "工具",
        agents: "代理",
        todos: "进度",
        skills: "技能",
        mcp: "MCP",
    },
};

pub fn get_labels(lang: Language) -> &'static Labels {
    match lang {
        Language::ZhHans => &ZH_HANS,
        Language::En => &EN,
    }
}
