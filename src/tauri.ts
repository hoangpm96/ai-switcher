import { invoke as tauriInvoke } from "@tauri-apps/api/core";
import type {
  AddAccountInput,
  AddApiAccountInput,
  ApiUsageReport,
  AppSnapshot,
  DetectionReport,
  RenameAccountInput,
  CreateApiGatewayKeyInput,
  CreateApiGatewayKeyResult,
  CreateVirtualApiAccountInput,
  SaveApiGatewayComboInput,
  SetApiGatewayAccountInput,
  SetAutoPrimeAllInput,
  SetAutoPrimeInput,
  SetLauncherInput,
  SetToolSetupInput,
  StartApiGatewayInput,
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
  autoSwitchSettings: {
    claude: { enabled: false, threshold: 100 },
    codex: { enabled: true, threshold: 95 },
  },
  autoPrime: {
    claude: { enabled: false, time: "07:30" },
    codex: { enabled: false, time: "08:00" },
  },
  toolSetups: {
    claude: {
      binaryPath: "/Users/demo/.local/bin/claude",
      defaultConfigDir: "/Users/demo/.claude",
      binarySource: "path",
      configSource: "default",
      validatedAt: "2026-06-08T10:00:00Z",
      validationWarnings: [],
    },
    codex: {
      binaryPath: "/opt/homebrew/bin/codex",
      defaultConfigDir: "/Users/demo/.codex",
      binarySource: "path",
      configSource: "default",
      validatedAt: "2026-06-08T10:00:00Z",
      validationWarnings: [],
    },
  },
  apiGateway: {
    config: {
      bindHost: "127.0.0.1",
      port: 8783,
      quotaThreshold: 95,
      maxRetries: 3,
      rotationStrategy: "roundRobin",
      keys: [],
      combos: [],
      accounts: [],
      modelRegistry: [],
      virtualClaudeEnabled: false,
      virtualCodexEnabled: false,
    },
    status: { state: "stopped", baseUrl: "http://127.0.0.1:8783", error: null },
  },
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

const demoApiUsage: ApiUsageReport = {
  generatedAt: "2026-06-14T09:00:00Z",
  totalRequests: 0,
  total: tb(0, 0, 0, 0),
  rows: [],
};

async function invoke<T>(command: string, args?: Record<string, unknown>): Promise<T> {
  if (isTauri) {
    return tauriInvoke<T>(command, args);
  }

  await new Promise((resolve) => window.setTimeout(resolve, 120));
  if (command === "load_snapshot" || command === "refresh_tool" || command === "set_auto_prime" || command === "set_auto_prime_all") {
    return structuredClone(demoSnapshot) as T;
  }
  if (command === "get_auto_prime_log") {
    return "" as T;
  }
  if (command === "open_auto_prime_log" || command === "open_auto_prime_log_folder") {
    return undefined as T;
  }
  if (command === "get_usage") {
    return structuredClone(demoUsage) as T;
  }
  if (command === "get_api_usage") {
    return structuredClone(demoApiUsage) as T;
  }
  if (command === "fetch_gateway_models") {
    return [
      "cx/gpt-5.5-codex",
      "cx/gpt-5.5-codex-high",
      "cx/gpt-5.4",
      "kr/claude-sonnet-4.5",
      "gc/gemini-3-pro-preview",
    ] as T;
  }
  if (command === "detect_tool_setup" || command === "validate_tool_setup") {
    const toolId = (args?.toolId ?? (args?.input as { toolId?: ToolId } | undefined)?.toolId ?? "claude") as ToolId;
    const setup = demoSnapshot.toolSetups[toolId];
    return {
      toolId,
      configCandidates: setup?.defaultConfigDir
        ? [{
            path: setup.defaultConfigDir,
            source: setup.configSource,
            score: 10,
            valid: true,
            isAppManaged: false,
            evidence: [{ label: "demo", found: true }],
            warnings: [],
          }]
        : [],
      binaryCandidates: setup?.binaryPath
        ? [{
            path: setup.binaryPath,
            resolvedPath: setup.binaryPath,
            source: setup.binarySource,
            score: 10,
            valid: true,
            isAppLauncher: false,
            evidence: [{ label: "demo", found: true }],
            warnings: [],
          }]
        : [],
      resolution: { kind: "resolved", setup, reason: "Demo setup" },
    } as T;
  }
  if (command === "set_tool_setup") {
    const input = args?.input as SetToolSetupInput;
    demoSnapshot.toolSetups[input.toolId] = {
      binaryPath: input.binaryPath,
      defaultConfigDir: input.defaultConfigDir,
      binarySource: "manual",
      configSource: "manual",
      validatedAt: new Date().toISOString(),
      validationWarnings: [],
    };
    return structuredClone(demoSnapshot) as T;
  }
  if (command === "set_auto_switch_setting") {
    const toolId = args?.toolId as ToolId;
    demoSnapshot.autoSwitchSettings[toolId] = {
      enabled: Boolean(args?.enabled),
      threshold: Number(args?.threshold ?? 100),
    };
    demoSnapshot.autoSwitch = Object.values(demoSnapshot.autoSwitchSettings).some((setting) => setting.enabled);
    demoSnapshot.autoSwitchThreshold = demoSnapshot.autoSwitchSettings[toolId].threshold;
    return structuredClone(demoSnapshot) as T;
  }
  if (command === "set_auto_switch") {
    const enabled = Boolean(args?.enabled);
    const threshold = Number(args?.threshold ?? 100);
    demoSnapshot.autoSwitch = enabled;
    demoSnapshot.autoSwitchThreshold = threshold;
    demoSnapshot.autoSwitchSettings.claude = { enabled, threshold };
    demoSnapshot.autoSwitchSettings.codex = { enabled, threshold };
    return structuredClone(demoSnapshot) as T;
  }
  if (command === "start_api_gateway") {
    const input = args?.input as StartApiGatewayInput;
    demoSnapshot.apiGateway.config.bindHost = input.bindHost;
    demoSnapshot.apiGateway.config.port = input.port;
    demoSnapshot.apiGateway.config.quotaThreshold = input.quotaThreshold;
    demoSnapshot.apiGateway.config.rotationStrategy = input.rotationStrategy;
    demoSnapshot.apiGateway.status = {
      state: "running",
      baseUrl: `http://${input.bindHost}:${input.port}`,
      error: null,
    };
    return structuredClone(demoSnapshot) as T;
  }
  if (command === "stop_api_gateway") {
    demoSnapshot.apiGateway.status.state = "stopped";
    return structuredClone(demoSnapshot) as T;
  }
  if (command === "create_api_gateway_key") {
    const input = args?.input as CreateApiGatewayKeyInput;
    const secret = `sk-demo-${Math.random().toString(16).slice(2)}${Date.now()}`;
    demoSnapshot.apiGateway.config.keys.push({
      id: crypto.randomUUID(),
      name: input.name || "Default key",
      prefix: `sk-...${secret.slice(-6)}`,
      enabled: true,
      expiresAt: input.expiresAt,
      createdAt: new Date().toISOString(),
    });
    return { snapshot: structuredClone(demoSnapshot), secret } as T;
  }
  if (command === "delete_api_gateway_key") {
    const keyId = (args?.input as { keyId: string }).keyId;
    demoSnapshot.apiGateway.config.keys = demoSnapshot.apiGateway.config.keys.filter((key) => key.id !== keyId);
    return structuredClone(demoSnapshot) as T;
  }
  if (command === "reveal_api_gateway_key") {
    return ("sk-demo-revealed-key") as T;
  }
  if (command === "save_api_gateway_combo") {
    const input = args?.input as SaveApiGatewayComboInput;
    const id = input.id || crypto.randomUUID();
    const existing = demoSnapshot.apiGateway.config.combos.findIndex((combo) => combo.id === id);
    const combo = {
      id,
      name: input.name,
      members: input.members,
      strategy: input.strategy ?? null,
      enabled: existing >= 0 ? demoSnapshot.apiGateway.config.combos[existing].enabled : true,
      createdAt: new Date().toISOString(),
      updatedAt: new Date().toISOString(),
    };
    if (existing >= 0) demoSnapshot.apiGateway.config.combos[existing] = combo;
    else demoSnapshot.apiGateway.config.combos.push(combo);
    return structuredClone(demoSnapshot) as T;
  }
  if (command === "delete_api_gateway_combo") {
    const comboId = (args?.input as { comboId: string }).comboId;
    demoSnapshot.apiGateway.config.combos = demoSnapshot.apiGateway.config.combos.filter(
      (combo) => combo.id !== comboId,
    );
    return structuredClone(demoSnapshot) as T;
  }
  if (command === "set_api_gateway_account") {
    const input = args?.input as SetApiGatewayAccountInput;
    const entry = demoSnapshot.apiGateway.config.accounts.find(
      (account) => account.toolId === input.toolId && account.accountId === input.accountId,
    );
    if (entry) entry.enabled = input.enabled;
    else
      demoSnapshot.apiGateway.config.accounts.push({
        toolId: input.toolId,
        accountId: input.accountId,
        enabled: input.enabled,
        state: "available",
      });
    return structuredClone(demoSnapshot) as T;
  }
  if (command === "refresh_api_gateway_models") {
    return structuredClone(demoSnapshot) as T;
  }
  if (command === "create_virtual_api_account") {
    const input = args?.input as CreateVirtualApiAccountInput;
    const tool = demoSnapshot.tools.find((item) => item.id === input.toolId);
    if (tool && !tool.accounts.some((account) => account.fingerprint === "api-local")) {
      tool.accounts.push({
        id: crypto.randomUUID(),
        toolId: input.toolId,
        name: input.toolId === "claude" ? "claude-api" : "codex-api",
        state: "idle",
        fingerprint: "api-local",
        createdAt: new Date().toISOString(),
        updatedAt: new Date().toISOString(),
        lastUsedAt: null,
        quota: null,
        launcherCommand: null,
        isDefault: false,
        apiProvider: {
          baseUrl: demoSnapshot.apiGateway.status.baseUrl,
          model:
            input.model ||
            demoSnapshot.apiGateway.config.combos[0]?.name ||
            "local-subscription",
          bypass: false,
        },
      });
    }
    return structuredClone(demoSnapshot) as T;
  }
  if (command === "add_api_account") {
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
  refreshAccount: (toolId: ToolId, accountId: string) =>
    invoke<AppSnapshot>("refresh_account", { toolId, accountId }),
  addAccount: (input: AddAccountInput) => invoke<AppSnapshot>("add_account", { input }),
  addApiAccount: (input: AddApiAccountInput) => invoke<AppSnapshot>("add_api_account", { input }),
  fetchGatewayModels: (baseUrl: string, apiKey: string) =>
    invoke<string[]>("fetch_gateway_models", { baseUrl, apiKey }),
  renameAccount: (input: RenameAccountInput) => invoke<AppSnapshot>("rename_account", { input }),
  switchAccount: (input: SwitchAccountInput) => invoke<AppSnapshot>("switch_account", { input }),
  setLauncher: (input: SetLauncherInput) => invoke<AppSnapshot>("set_launcher", { input }),
  deleteAccount: (toolId: ToolId, accountId: string) =>
    invoke<AppSnapshot>("delete_account", { toolId, accountId }),
  acceptDisclaimer: () => invoke<AppSnapshot>("accept_disclaimer"),
  antigravityNewLogin: () => invoke<AppSnapshot>("antigravity_new_login"),
  setAutoSwitch: (enabled: boolean, threshold: number) =>
    invoke<AppSnapshot>("set_auto_switch", { enabled, threshold }),
  setAutoSwitchSetting: (toolId: ToolId, enabled: boolean, threshold: number) =>
    invoke<AppSnapshot>("set_auto_switch_setting", { toolId, enabled, threshold }),
  detectToolSetup: (toolId: ToolId) => invoke<DetectionReport>("detect_tool_setup", { toolId }),
  validateToolSetup: (input: SetToolSetupInput) =>
    invoke<DetectionReport>("validate_tool_setup", { input }),
  setToolSetup: (input: SetToolSetupInput) => invoke<AppSnapshot>("set_tool_setup", { input }),
  getUsage: (rangeDays: number) => invoke<UsageReport>("get_usage", { rangeDays }),
  getApiUsage: () => invoke<ApiUsageReport>("get_api_usage"),
  startApiGateway: (input: StartApiGatewayInput) =>
    invoke<AppSnapshot>("start_api_gateway", { input }),
  stopApiGateway: () => invoke<AppSnapshot>("stop_api_gateway"),
  createApiGatewayKey: (input: CreateApiGatewayKeyInput) =>
    invoke<CreateApiGatewayKeyResult>("create_api_gateway_key", { input }),
  deleteApiGatewayKey: (keyId: string) =>
    invoke<AppSnapshot>("delete_api_gateway_key", { input: { keyId } }),
  revealApiGatewayKey: (keyId: string) => invoke<string>("reveal_api_gateway_key", { keyId }),
  saveApiGatewayCombo: (input: SaveApiGatewayComboInput) =>
    invoke<AppSnapshot>("save_api_gateway_combo", { input }),
  deleteApiGatewayCombo: (comboId: string) =>
    invoke<AppSnapshot>("delete_api_gateway_combo", { input: { comboId } }),
  setApiGatewayAccount: (input: SetApiGatewayAccountInput) =>
    invoke<AppSnapshot>("set_api_gateway_account", { input }),
  refreshApiGatewayModels: () => invoke<AppSnapshot>("refresh_api_gateway_models"),
  createVirtualApiAccount: (toolId: ToolId, model?: string) =>
    invoke<AppSnapshot>("create_virtual_api_account", { input: { toolId, model: model ?? null } }),
  setAutoPrime: (input: SetAutoPrimeInput) => invoke<AppSnapshot>("set_auto_prime", { input }),
  setAutoPrimeAll: (input: SetAutoPrimeAllInput) =>
    invoke<AppSnapshot>("set_auto_prime_all", { input }),
  getAutoPrimeLog: () => invoke<string>("get_auto_prime_log"),
  openAutoPrimeLog: () => invoke<void>("open_auto_prime_log"),
  openAutoPrimeLogFolder: () => invoke<void>("open_auto_prime_log_folder"),
};
