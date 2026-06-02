import { invoke as tauriInvoke } from "@tauri-apps/api/core";
import type {
  AddAccountInput,
  AppSnapshot,
  RenameAccountInput,
  SetLauncherInput,
  SwitchAccountInput,
  ToolId,
  UsageReport,
} from "./types";

const isTauri = "__TAURI_INTERNALS__" in window;

const quota = (five: number | null, week: number | null, error: string | null = null) => ({
  fiveHour: { label: "5-hour limit", percentUsed: five, resetAt: "2026-05-31T15:30:00Z" },
  weekly: { label: "Weekly limit", percentUsed: week, resetAt: "2026-06-02T00:00:00Z" },
  updatedAt: "2026-05-31T08:00:00Z",
  error,
});

const demoSnapshot: AppSnapshot = {
  disclaimerAccepted: false,
  autoSwitch: false,
  autoSwitchThreshold: 100,
  tools: [
    {
      id: "claude",
      name: "Claude Code",
      installed: true,
      activeAccountId: "c2",
      accounts: [
        {
          id: "default-claude", toolId: "claude", name: "Machine default", state: "idle",
          fingerprint: "default", createdAt: "2026-05-20T10:00:00Z", updatedAt: "2026-05-31T08:00:00Z",
          lastUsedAt: null, quota: quota(32, 18), launcherCommand: null, isDefault: true,
        },
        {
          id: "c2", toolId: "claude", name: "Work", state: "active", fingerprint: "profile:c2",
          createdAt: "2026-05-21T10:00:00Z", updatedAt: "2026-05-30T08:00:00Z",
          lastUsedAt: "2026-05-29T19:10:00Z", quota: quota(82, 70),
          launcherCommand: "claude-work", isDefault: false,
        },
        {
          id: "c3", toolId: "claude", name: "Client", state: "exhausted", fingerprint: "profile:c3",
          createdAt: "2026-05-22T10:00:00Z", updatedAt: "2026-05-31T06:00:00Z",
          lastUsedAt: "2026-05-31T05:00:00Z", quota: quota(100, 96),
          launcherCommand: "claude-client", isDefault: false,
        },
      ],
    },
    {
      id: "codex",
      name: "Codex",
      installed: true,
      activeAccountId: null,
      accounts: [
        {
          id: "default-codex", toolId: "codex", name: "Machine default", state: "idle",
          fingerprint: "default", createdAt: "2026-05-18T10:00:00Z", updatedAt: "2026-05-31T08:00:00Z",
          lastUsedAt: null, quota: quota(40, 55), launcherCommand: null, isDefault: true,
        },
        {
          id: "x2", toolId: "codex", name: "Pro 6x", state: "needs-login", fingerprint: "profile:x2",
          createdAt: "2026-05-19T10:00:00Z", updatedAt: "2026-05-31T08:00:00Z",
          lastUsedAt: null, quota: quota(null, null, "Waiting for login in Terminal"),
          launcherCommand: "codex-pro6", isDefault: false,
        },
      ],
    },
    {
      id: "antigravity",
      name: "Antigravity",
      installed: false,
      activeAccountId: null,
      accounts: [],
    },
  ],
};

const tb = (input: number, output: number, cacheRead: number, cacheCreation = 0) => ({
  input,
  output,
  cacheRead,
  cacheCreation,
});

const demoUsage: UsageReport = {
  generatedAt: "2026-06-02T08:00:00Z",
  priceStatus: "live",
  priceUpdatedAt: "2026-06-02T00:00:00Z",
  tools: [
    {
      toolId: "claude",
      displayName: "Claude Code",
      estimate: true,
      total: tb(120_000, 480_000, 5_200_000, 1_300_000),
      totalCostUsd: 12.84,
      today: tb(8_000, 32_000, 410_000, 95_000),
      todayCostUsd: 1.12,
      daily: [
        { date: "2026-05-28", tokens: tb(20000, 80000, 900000, 210000), costUsd: 2.1 },
        { date: "2026-05-29", tokens: tb(15000, 60000, 700000, 160000), costUsd: 1.6 },
        { date: "2026-05-30", tokens: tb(31000, 120000, 1300000, 320000), costUsd: 3.2 },
        { date: "2026-05-31", tokens: tb(26000, 96000, 1100000, 260000), costUsd: 2.7 },
        { date: "2026-06-01", tokens: tb(20000, 92000, 790000, 255000), costUsd: 2.12 },
        { date: "2026-06-02", tokens: tb(8000, 32000, 410000, 95000), costUsd: 1.12 },
      ],
      byModel: [
        { model: "claude-opus-4-8", tokens: tb(70000, 300000, 3200000, 800000), costUsd: 9.1 },
        { model: "claude-sonnet-4-5", tokens: tb(50000, 180000, 2000000, 500000), costUsd: 3.74 },
      ],
      sessions: [
        { id: "7e5d3164", date: "2026-06-02", model: "claude-opus-4-8", tokens: tb(8000, 32000, 410000, 95000), costUsd: 1.12 },
        { id: "a1b2c3d4", date: "2026-06-01", model: "claude-sonnet-4-5", tokens: tb(12000, 40000, 380000, 120000), costUsd: 0.95 },
      ],
    },
    {
      toolId: "codex",
      displayName: "Codex",
      estimate: false,
      total: tb(900_000, 240_000, 3_100_000, 0),
      totalCostUsd: 6.42,
      today: tb(60_000, 18_000, 210_000, 0),
      todayCostUsd: 0.51,
      daily: [
        { date: "2026-05-29", tokens: tb(160000, 42000, 560000, 0), costUsd: 1.1 },
        { date: "2026-05-30", tokens: tb(220000, 60000, 780000, 0), costUsd: 1.6 },
        { date: "2026-05-31", tokens: tb(180000, 48000, 640000, 0), costUsd: 1.3 },
        { date: "2026-06-01", tokens: tb(280000, 72000, 910000, 0), costUsd: 1.91 },
        { date: "2026-06-02", tokens: tb(60000, 18000, 210000, 0), costUsd: 0.51 },
      ],
      byModel: [
        { model: "gpt-5.5", tokens: tb(700000, 190000, 2400000, 0), costUsd: 5.0 },
        { model: "gpt-5", tokens: tb(200000, 50000, 700000, 0), costUsd: 1.42 },
      ],
      sessions: [
        { id: "019e887b", date: "2026-06-02", model: "gpt-5.5", tokens: tb(60000, 18000, 210000, 0), costUsd: 0.51 },
      ],
    },
  ],
};

async function invoke<T>(command: string, args?: Record<string, unknown>): Promise<T> {
  if (isTauri) {
    return tauriInvoke<T>(command, args);
  }

  await new Promise((resolve) => window.setTimeout(resolve, 120));
  if (command === "load_snapshot" || command === "refresh_tool") {
    return structuredClone(demoSnapshot) as T;
  }
  if (command === "get_usage") {
    return structuredClone(demoUsage) as T;
  }
  if (command === "accept_disclaimer") {
    demoSnapshot.disclaimerAccepted = true;
    return structuredClone(demoSnapshot) as T;
  }
  throw new Error("Desktop commands only run inside the Tauri app");
}

export const api = {
  loadSnapshot: () => invoke<AppSnapshot>("load_snapshot"),
  refreshTool: (toolId: ToolId) => invoke<AppSnapshot>("refresh_tool", { toolId }),
  addAccount: (input: AddAccountInput) => invoke<AppSnapshot>("add_account", { input }),
  renameAccount: (input: RenameAccountInput) => invoke<AppSnapshot>("rename_account", { input }),
  switchAccount: (input: SwitchAccountInput) => invoke<AppSnapshot>("switch_account", { input }),
  setLauncher: (input: SetLauncherInput) => invoke<AppSnapshot>("set_launcher", { input }),
  deleteAccount: (toolId: ToolId, accountId: string) =>
    invoke<AppSnapshot>("delete_account", { toolId, accountId }),
  acceptDisclaimer: () => invoke<AppSnapshot>("accept_disclaimer"),
  antigravityNewLogin: () => invoke<AppSnapshot>("antigravity_new_login"),
  setAutoSwitch: (enabled: boolean, threshold: number) =>
    invoke<AppSnapshot>("set_auto_switch", { enabled, threshold }),
  getUsage: () => invoke<UsageReport>("get_usage"),
};
