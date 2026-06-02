import { invoke as tauriInvoke } from "@tauri-apps/api/core";
import type {
  AddAccountInput,
  AppSnapshot,
  RenameAccountInput,
  SetLauncherInput,
  SwitchAccountInput,
  ToolId,
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

async function invoke<T>(command: string, args?: Record<string, unknown>): Promise<T> {
  if (isTauri) {
    return tauriInvoke<T>(command, args);
  }

  await new Promise((resolve) => window.setTimeout(resolve, 120));
  if (command === "load_snapshot" || command === "refresh_tool") {
    return structuredClone(demoSnapshot) as T;
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
};
