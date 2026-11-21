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
    pub battle: &'static str,
}

#[derive(Debug, Clone, Copy)]
pub struct MetricLabels {
    pub context: &'static str,
    pub quota: &'static str,
    pub cache: &'static str,
    pub cost: &'static str,
    pub subagent_cost: &'static str,
    pub today: &'static str,
    pub week: &'static str,
    pub sandbox_on: &'static str,
    pub sandbox_off: &'static str,
    pub sandbox_net: &'static str,
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
        battle: "BATTLE",
    },
    metrics: MetricLabels {
        context: "Ctx",
        quota: "Quota",
        cache: "Cache",
        cost: "Cost",
        subagent_cost: "sub",
        today: "Today",
        week: "Week",
        sandbox_on: "Sandbox ON",
        sandbox_off: "Sandbox off",
        sandbox_net: "net",
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
        battle: "对战",
    },
    metrics: MetricLabels {
        context: "上下文",
        quota: "配额",
        cache: "缓存",
        cost: "费用",
        subagent_cost: "子代理",
        today: "今日",
        week: "本周",
        sandbox_on: "沙箱开启",
        sandbox_off: "沙箱关闭",
        sandbox_net: "联网",
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

pub const JA: Labels = Labels {
    state: StateLabels {
        idle: "待機中",
        thinking: "思考中",
        working: "実行中",
        tool_use: "ツール実行",
        reviewing: "レビュー中",
        authenticating: "認証中",
        initializing: "初期化中",
        error: "エラー",
        confirm: "確認待ち!",
    },
    mode: ModeLabels {
        plan: "計画",
        auto: "自動",
        accept_edits: "編集承認",
        battle: "対戦",
    },
    metrics: MetricLabels {
        context: "コンテキスト",
        quota: "利用枠",
        cache: "キャッシュ",
        cost: "費用",
        subagent_cost: "子",
        today: "本日",
        week: "今週",
        sandbox_on: "サンドボックスON",
        sandbox_off: "サンドボックスOFF",
        sandbox_net: "ネット",
        artifacts: "成果物",
        subagents: "サブエージェント",
        tasks: "タスク",
        queued: "待機",
        memory: "メモリ",
        resets_in: "リセット",
        remaining: "残り",
        tools: "ツール",
        agents: "エージェント",
        todos: "進捗",
        skills: "スキル",
        mcp: "MCP",
    },
};

pub fn get_labels(lang: Language) -> &'static Labels {
    match lang {
        Language::ZhHans => &ZH_HANS,
        Language::Ja => &JA,
        Language::En | Language::Auto => &EN,
    }
}

/// Pure string matching for POSIX/Windows locale strings (zero `std::env`).
pub fn detect_locale_language(loc: &str) -> Language {
    let lower = loc.to_ascii_lowercase();
    let norm = lower.replace('_', "-");
    if norm.starts_with("zh") {
        Language::ZhHans
    } else if norm.starts_with("ja") {
        Language::Ja
    } else {
        Language::En
    }
}

/// Resolves `Language::Auto` against an optional system locale string, preserving explicit choices.
pub fn resolve_language(lang: Language, system_locale: Option<&str>) -> Language {
    match lang {
        Language::Auto => match system_locale {
            Some(loc) => detect_locale_language(loc),
            None => Language::En,
        },
        explicit => explicit,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn locale_detection_maps_posix_and_windows_locale_strings() {
        assert_eq!(detect_locale_language("ja_JP.UTF-8"), Language::Ja);
        assert_eq!(detect_locale_language("ja_JP"), Language::Ja);
        assert_eq!(detect_locale_language("ja"), Language::Ja);
        assert_eq!(detect_locale_language("JA-JP"), Language::Ja);

        assert_eq!(detect_locale_language("zh_CN.UTF-8"), Language::ZhHans);
        assert_eq!(detect_locale_language("zh_TW"), Language::ZhHans);
        assert_eq!(detect_locale_language("zh-Hans"), Language::ZhHans);
        assert_eq!(detect_locale_language("zh"), Language::ZhHans);

        assert_eq!(detect_locale_language("en_US.UTF-8"), Language::En);
        assert_eq!(detect_locale_language("en_GB"), Language::En);
        assert_eq!(detect_locale_language("C"), Language::En);
        assert_eq!(detect_locale_language("POSIX"), Language::En);
        assert_eq!(detect_locale_language(""), Language::En);
    }

    #[test]
    fn resolve_language_respects_explicit_override_and_falls_back_cleanly() {
        assert_eq!(
            resolve_language(Language::ZhHans, Some("ja_JP.UTF-8")),
            Language::ZhHans
        );
        assert_eq!(
            resolve_language(Language::Ja, Some("en_US.UTF-8")),
            Language::Ja
        );
        assert_eq!(
            resolve_language(Language::En, Some("zh_CN.UTF-8")),
            Language::En
        );

        assert_eq!(
            resolve_language(Language::Auto, Some("ja_JP.UTF-8")),
            Language::Ja
        );
        assert_eq!(
            resolve_language(Language::Auto, Some("zh_CN.UTF-8")),
            Language::ZhHans
        );
        assert_eq!(
            resolve_language(Language::Auto, Some("en_US.UTF-8")),
            Language::En
        );
        assert_eq!(resolve_language(Language::Auto, None), Language::En);
    }

    #[test]
    fn labels_return_distinct_localized_strings() {
        let ja = get_labels(Language::Ja);
        assert_eq!(ja.state.idle, "待機中");
        assert_eq!(ja.metrics.context, "コンテキスト");
        assert_eq!(ja.metrics.quota, "利用枠");
        assert_eq!(ja.mode.plan, "計画");
        assert_eq!(ja.mode.battle, "対戦");
        assert_eq!(ja.metrics.subagent_cost, "子");
        assert_eq!(ja.metrics.sandbox_net, "ネット");

        let zh = get_labels(Language::ZhHans);
        assert_eq!(zh.state.idle, "就绪");
        assert_eq!(zh.metrics.context, "上下文");
        assert_eq!(zh.mode.battle, "对战");
        assert_eq!(zh.metrics.subagent_cost, "子代理");
        assert_eq!(zh.metrics.sandbox_net, "联网");

        let en = get_labels(Language::En);
        assert_eq!(en.state.idle, "READY");
        assert_eq!(en.mode.battle, "BATTLE");
        assert_eq!(en.metrics.subagent_cost, "sub");
        assert_eq!(en.metrics.sandbox_net, "net");

        let auto = get_labels(Language::Auto);
        assert_eq!(auto.state.idle, "READY");
    }
}
