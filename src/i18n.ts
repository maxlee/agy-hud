import type { Language } from './types.ts';

export interface Labels {
  state: {
    idle: string;
    thinking: string;
    working: string;
    tool_use: string;
    reviewing: string;
    authenticating: string;
    initializing: string;
    error: string;
    confirm: string;
  };
  mode: {
    plan: string;
    auto: string;
    acceptEdits: string;
  };
  metrics: {
    context: string;
    quota: string;
    cache: string;
    cost: string;
    sandboxOn: string;
    sandboxOff: string;
    artifacts: string;
    subagents: string;
    tasks: string;
    queued: string;
    memory: string;
    resetsIn: string;
    remaining: string;
    tools: string;
    agents: string;
    todos: string;
    skills: string;
    mcp: string;
  };
}

const EN: Labels = {
  state: {
    idle: 'READY',
    thinking: 'THINKING',
    working: 'WORKING',
    tool_use: 'TOOL',
    reviewing: 'REVIEW',
    authenticating: 'AUTH',
    initializing: 'INIT',
    error: 'ERROR',
    confirm: 'CONFIRM?',
  },
  mode: {
    plan: 'PLAN',
    auto: 'AUTO',
    acceptEdits: 'ACCEPT',
  },
  metrics: {
    context: 'Ctx',
    quota: 'Quota',
    cache: 'Cache',
    cost: 'Cost',
    sandboxOn: 'Sandbox ON',
    sandboxOff: 'Sandbox off',
    artifacts: 'art',
    subagents: 'agents',
    tasks: 'tasks',
    queued: 'queued',
    memory: 'RAM',
    resetsIn: 'resets',
    remaining: 'left',
    tools: 'Tools',
    agents: 'Agents',
    todos: 'Tasks',
    skills: 'Skills',
    mcp: 'MCP',
  },
};

const ZH_HANS: Labels = {
  state: {
    idle: '就绪',
    thinking: '思考中',
    working: '执行中',
    tool_use: '调用工具',
    reviewing: '审阅中',
    authenticating: '认证中',
    initializing: '初始化',
    error: '异常',
    confirm: '待确认!',
  },
  mode: {
    plan: '规划',
    auto: '自动',
    acceptEdits: '接受编辑',
  },
  metrics: {
    context: '上下文',
    quota: '配额',
    cache: '缓存',
    cost: '费用',
    sandboxOn: '沙箱开启',
    sandboxOff: '沙箱关闭',
    artifacts: '产物',
    subagents: '子代理',
    tasks: '后台',
    queued: '排队',
    memory: '内存',
    resetsIn: '重置',
    remaining: '剩余',
    tools: '工具',
    agents: '代理',
    todos: '进度',
    skills: '技能',
    mcp: 'MCP',
  },
};

export function getLabels(lang: Language): Labels {
  return lang === 'zh-Hans' ? ZH_HANS : EN;
}
