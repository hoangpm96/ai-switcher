export type ToolId = "claude" | "codex" | "antigravity";

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
  updatedAt: string | null;
  error: string | null;
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
}

export interface AddAccountInput {
  toolId: ToolId;
  name: string;
  mode: "import" | "login";
  /** Custom command name (required for Claude/Codex). */
  launcher?: string;
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
