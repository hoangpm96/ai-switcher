export type ToolId = "claude" | "codex" | "antigravity";
export type UsageToolId = ToolId | "all";

export type AccountState = "idle" | "active" | "exhausted" | "needs-login";

export interface QuotaWindow {
  label: string;
  percentUsed: number | null;
  resetAt: string | null;
}

export interface QuotaInfo {
  fiveHour: QuotaWindow;
  weekly: QuotaWindow;
  /** Per-model quota detail (Antigravity). Absent for Claude/Codex. */
  models?: QuotaWindow[] | null;
  /** Subscription plan label (e.g. "Plus", "Pro", "Max"). Absent when the API omits it. */
  plan?: string | null;
  updatedAt: string | null;
  error: string | null;
}

export interface ApiProvider {
  /** Gateway base URL, e.g. `https://your-gateway.com/v1`. */
  baseUrl: string;
  /** Gateway model id the account runs (one model per account). */
  model: string;
  /** Add `--dangerously-bypass-approvals-and-sandbox` to the launcher. */
  bypass: boolean;
}

export interface Account {
  id: string;
  toolId: ToolId;
  name: string;
  state: AccountState;
  fingerprint: string;
  createdAt: string;
  updatedAt: string;
  lastUsedAt: string | null;
  quota: QuotaInfo | null;
  /** Custom command to use the account (e.g. `claude-work`). null for Default (bare command). */
  launcherCommand: string | null;
  /** true for Machine default (~/.claude / ~/.codex) — read-only. */
  isDefault: boolean;
  /** Google avatar (Antigravity only) — shown instead of the fingerprint. */
  avatarUrl?: string | null;
  /** Present when the account runs through an external API/proxy gateway (no quota). */
  apiProvider?: ApiProvider | null;
}

export interface ToolStatus {
  id: ToolId;
  name: string;
  installed: boolean;
  activeAccountId: string | null;
  accounts: Account[];
}

export interface AppSnapshot {
  tools: ToolStatus[];
  disclaimerAccepted: boolean;
  autoSwitch: boolean;
  autoSwitchThreshold: number;
  autoSwitchSettings: Record<string, AutoSwitchSetting>;
  toolSetups: Record<string, ToolSetup>;
}

export interface AutoSwitchSetting {
  enabled: boolean;
  threshold: number;
}

export type DetectionSource = "env" | "default" | "path" | "appManaged" | "manual" | "fallback";

export interface ToolSetup {
  binaryPath?: string | null;
  defaultConfigDir?: string | null;
  binarySource: DetectionSource;
  configSource: DetectionSource;
  validatedAt?: string | null;
  validationWarnings: string[];
}

export interface ValidationEvidence {
  label: string;
  found: boolean;
}

export interface ConfigCandidate {
  path: string;
  source: DetectionSource;
  score: number;
  valid: boolean;
  isAppManaged: boolean;
  evidence: ValidationEvidence[];
  warnings: string[];
}

export interface BinaryCandidate {
  path: string;
  resolvedPath?: string | null;
  source: DetectionSource;
  score: number;
  valid: boolean;
  isAppLauncher: boolean;
  evidence: ValidationEvidence[];
  warnings: string[];
}

export type ResolutionKind = "resolved" | "needsUserChoice" | "needsManualInput";

export interface DetectionResolution {
  kind: ResolutionKind;
  setup?: ToolSetup | null;
  reason: string;
}

export interface DetectionReport {
  toolId: ToolId;
  configCandidates: ConfigCandidate[];
  binaryCandidates: BinaryCandidate[];
  resolution: DetectionResolution;
}

export interface SetToolSetupInput {
  toolId: ToolId;
  binaryPath: string;
  defaultConfigDir: string;
}

export interface AddAccountInput {
  toolId: ToolId;
  name: string;
  mode: "import" | "login";
  /** Custom command name (required for Claude/Codex). */
  launcher?: string;
}

export interface AddApiAccountInput {
  toolId: ToolId;
  name: string;
  baseUrl: string;
  apiKey: string;
  model: string;
  launcher?: string;
  bypass: boolean;
}

export interface RenameAccountInput {
  toolId: ToolId;
  accountId: string;
  name: string;
}

export interface SwitchAccountInput {
  toolId: ToolId;
  accountId: string;
}

export interface SetLauncherInput {
  toolId: ToolId;
  accountId: string;
  name: string;
}

// --- Token usage tracking (Usage tab) ---

export interface TokenBreakdown {
  input: number;
  output: number;
  cacheRead: number;
  cacheCreation: number;
}

export interface DayUsage {
  date: string;
  tokens: TokenBreakdown;
  costUsd: number | null;
}

export interface ModelUsage {
  model: string;
  tokens: TokenBreakdown;
  costUsd: number | null;
}

export interface SessionUsage {
  id: string;
  date: string;
  model: string;
  tokens: TokenBreakdown;
  costUsd: number | null;
}

export interface ToolUsage {
  toolId: UsageToolId;
  displayName: string;
  /** true = numbers are an estimate (Claude's JSONL undercounts tokens). */
  estimate: boolean;
  total: TokenBreakdown;
  totalCostUsd: number | null;
  today: TokenBreakdown;
  todayCostUsd: number | null;
  daily: DayUsage[];
  byModel: ModelUsage[];
  sessions: SessionUsage[];
}

export interface UsageReport {
  tools: ToolUsage[];
  generatedAt: string;
  /** "live" | "cached" | "unavailable" */
  priceStatus: string;
  priceUpdatedAt: string | null;
}
