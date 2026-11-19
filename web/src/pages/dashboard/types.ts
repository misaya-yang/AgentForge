// web/src/pages/dashboard/types.ts

// Layout item type for react-grid-layout
export interface LayoutItem {
  i: string;
  x: number;
  y: number;
  w: number;
  h: number;
  minW?: number;
  minH?: number;
  maxW?: number;
  maxH?: number;
  static?: boolean;
  isDraggable?: boolean;
  isResizable?: boolean;
}

// Source filter type - internal (AI assistant, chat) vs external (API)
export type SourceFilter = "all" | "internal" | "external";

// Refresh interval options
export type RefreshInterval = 0 | 30 | 60 | 300; // 0 = manual, 30s, 1min, 5min

// Service filter - "all" or specific service_id
export type ServiceFilter = "all" | string;

// User filter - "all" or specific user_id
export type UserFilter = "all" | string;

// Trace filter for KPI → Trace 联动
export interface TraceFilter {
  status?: "success" | "error";
  error_type?: string;
  service_id?: string;
  sample_reason?: string;
  model?: string;
  provider?: string;
}

// Dashboard context shared across panels
export interface DashboardContext {
  dateRange: [string, string]; // [startDate, endDate] in YYYY-MM-DD
  granularity: "hour" | "day";
  source: SourceFilter;
  serviceId: ServiceFilter;
  userId: UserFilter;
  refreshInterval: RefreshInterval;
  /** Time a dashboard refresh was requested, not a successful data observation. */
  lastRefresh: Date;
  refreshMode: "manual" | "automatic" | "paused";
  traceFilter: TraceFilter;
}

// Panel configuration
export interface PanelConfig {
  id: string;
  title: string;
  type: PanelType;
  visible: boolean;
}

export type PanelType =
  | "service-health"
  | "performance"
  | "token-usage"
  | "cost-analysis"
  | "user-quota"
  | "security-events"
  | "request-trace"
  | "failure-analysis"
  | "provider-status";

// KPI card data
export interface KPIData {
  totalRequests: number;
  totalCost: number;
  avgLatency: number;
  successRate: number;
  activeUsers: number;
  costChange?: number; // percentage change
}

// Service health data
export interface ServiceHealth {
  serviceId: string;
  serviceName: string;
  status: "healthy" | "degraded" | "down";
  qps: number;
  avgLatency: number;
  errorRate: number;
}

// Performance metrics
export interface PerformanceMetrics {
  timestamp: string;
  p50: number;
  p95: number;
  p99: number;
  avgLatency: number;
}

// Token usage data
export interface TokenUsageData {
  totalTokens: number;
  inputTokens: number;
  outputTokens: number;
  byModel: Array<{
    model: string;
    tokens: number;
    percentage: number;
  }>;
  trend: Array<{
    date: string;
    tokens: number;
  }>;
}

// Cost analysis data
export interface CostAnalysisData {
  today: number;
  thisWeek: number;
  thisMonth: number;
  todayChange: number;
  weekChange: number;
  monthChange: number;
  bySource: Array<{
    source: string;
    cost: number;
    percentage: number;
  }>;
  byService: Array<{
    service: string;
    cost: number;
    percentage: number;
  }>;
  trend: Array<{
    date: string;
    internal: number;
    external: number;
  }>;
}

// User quota data
export interface UserQuotaData {
  userId: string;
  username: string;
  dailyUsed: number;
  dailyLimit: number;
  monthlyUsed: number;
  monthlyLimit: number;
  status: "normal" | "warning" | "exceeded";
}

// Security event data
export interface SecurityEventData {
  authFailures: number;
  rateLimitHits: number;
  anomalies: number;
  authFailuresChange: number;
  rateLimitChange: number;
  anomaliesChange: number;
  hourlyDistribution: Array<{
    hour: string;
    authFailures: number;
    rateLimitHits: number;
  }>;
  topUsers: Array<{
    user: string;
    eventType: string;
    count: number;
  }>;
}

// Request trace data
export interface RequestTrace {
  requestId: string;
  timestamp: string;
  service: string;
  model: string;
  user: string;
  source: "internal" | "external";
  totalLatency: number;
  status: "success" | "error";
  inputTokens: number;
  outputTokens: number;
  cost: number;
  spans?: TraceSpan[];
}

export interface TraceSpan {
  name: string;
  startTime: number;
  duration: number;
}

// Default panel layouts
export const DEFAULT_LAYOUTS: LayoutItem[] = [
  { i: "service-health", x: 0, y: 0, w: 12, h: 4, minW: 6, minH: 3 },
  { i: "performance", x: 0, y: 4, w: 6, h: 5, minW: 4, minH: 4 },
  { i: "token-usage", x: 6, y: 4, w: 6, h: 5, minW: 4, minH: 4 },
  { i: "failure-analysis", x: 0, y: 9, w: 6, h: 6, minW: 4, minH: 4 },
  { i: "cost-analysis", x: 6, y: 9, w: 6, h: 6, minW: 4, minH: 5 },
  { i: "user-quota", x: 0, y: 15, w: 6, h: 6, minW: 4, minH: 5 },
  { i: "security-events", x: 6, y: 15, w: 6, h: 5, minW: 4, minH: 4 },
  { i: "request-trace", x: 0, y: 21, w: 12, h: 6, minW: 6, minH: 4 },
];

// Default panel configs
export const DEFAULT_PANELS: PanelConfig[] = [
  { id: "service-health", title: "Service Health", type: "service-health", visible: true },
  { id: "performance", title: "Performance", type: "performance", visible: true },
  { id: "token-usage", title: "Token Usage", type: "token-usage", visible: true },
  { id: "failure-analysis", title: "Failure Analysis", type: "failure-analysis", visible: true },
  { id: "cost-analysis", title: "Cost Analysis", type: "cost-analysis", visible: true },
  { id: "user-quota", title: "User Quota", type: "user-quota", visible: true },
  { id: "security-events", title: "Security Events", type: "security-events", visible: true },
  { id: "request-trace", title: "Request Trace", type: "request-trace", visible: true },
  { id: "provider-status", title: "Provider Status", type: "provider-status", visible: true },
];
