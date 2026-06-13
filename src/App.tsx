import {
  AlertTriangle,
  BarChart3,
  Check,
  CircleHelp,
  Copy,
  KeyRound,
  Loader2,
  LogIn,
  Pencil,
  RefreshCw,
  RotateCcw,
  Settings,
  ShieldAlert,
  Terminal,
  Trash2,
  X,
  Zap,
} from "lucide-react";
import { useCallback, useEffect, useMemo, useState } from "react";
import { listen } from "@tauri-apps/api/event";
import { getVersion } from "@tauri-apps/api/app";
import { openUrl } from "@tauri-apps/plugin-opener";
import { api } from "./tauri";
import { UsageView } from "./UsageView";
import logoUrl from "./assets/logo.svg";
import type {
  Account,
  AddAccountInput,
  AddApiAccountInput,
  AppSnapshot,
  BinaryCandidate,
  ConfigCandidate,
  DetectionReport,
  SetToolSetupInput,
  ToolSetup,
  ToolId,
  ToolStatus,
} from "./types";

/** Host shown on an API account card (best-effort parse of the gateway URL). */
function gatewayHost(baseUrl: string) {
  try {
    return new URL(baseUrl).host;
  } catch {
    return baseUrl;
  }
}

const toolDescriptions: Record<ToolId, string> = {
  claude:
    "The bare `claude` command follows the selected account (Use button). Each account also has its own `claude-…` command to run in parallel in another terminal.",
  codex:
    "The bare `codex` command follows the selected account (Use button). Each account also has its own `codex-…` command to run in parallel in another terminal.",
  antigravity:
    "Save Antigravity IDE login sessions to switch between them. Click Use: the app quits the IDE, loads that account's token, then reopens the IDE.",
};

const emptySnapshot: AppSnapshot = {
  tools: [],
  disclaimerAccepted: true,
  autoSwitch: false,
  autoSwitchThreshold: 100,
  autoSwitchSettings: {},
  toolSetups: {},
};

export function App() {
  const [snapshot, setSnapshot] = useState<AppSnapshot>(emptySnapshot);
  const [selectedTool, setSelectedTool] = useState<ToolId>("claude");
  const [view, setView] = useState<"accounts" | "usage" | "settings">("accounts");
  const [toast, setToast] = useState<string | null>(null);
  const [busy, setBusy] = useState<string | null>(null);
  const [refreshingAccounts, setRefreshingAccounts] = useState<Set<string>>(new Set());
  const [dialog, setDialog] = useState<"add" | "rename" | "launcher" | "setup" | null>(null);
  const [selectedAccount, setSelectedAccount] = useState<Account | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [switchNotice, setSwitchNotice] = useState<string | null>(null);
  const [autoSwitchBanner, setAutoSwitchBanner] = useState<string | null>(null);
  const [version, setVersion] = useState("");
  const [setupPrompted, setSetupPrompted] = useState<Set<ToolId>>(new Set());

  useEffect(() => {
    getVersion()
      .then(setVersion)
      .catch(() => {});
  }, []);

  const load = useCallback(async () => {
    setBusy("load");
    setError(null);
    try {
      setSnapshot(await api.loadSnapshot());
    } catch (err) {
      setError(errorMessage(err));
    } finally {
      setBusy(null);
    }
  }, []);

  useEffect(() => {
    void load();
  }, [load]);

  // Backend pushes a fresh snapshot when a login finishes / a background auto-switch happens.
  useEffect(() => {
    const unlisten = listen<AppSnapshot>("snapshot-changed", (event) => {
      setSnapshot(event.payload);
    });
    return () => {
      void unlisten.then((fn) => fn());
    };
  }, []);

  // Banner when the app auto-switches an account because quota ran out.
  useEffect(() => {
    const unlisten = listen<string>("auto-switched", (event) => {
      setAutoSwitchBanner(event.payload);
    });
    return () => {
      void unlisten.then((fn) => fn());
    };
  }, []);

  useEffect(() => {
    if (!snapshot.disclaimerAccepted || dialog) return;
    const needsSetup = snapshot.tools.find(
      (tool) => tool.id !== "antigravity" && tool.installed && !snapshot.toolSetups[tool.id] && !setupPrompted.has(tool.id),
    );
    if (!needsSetup) return;
    setSelectedTool(needsSetup.id);
    setDialog("setup");
    setSetupPrompted((prev) => new Set(prev).add(needsSetup.id));
  }, [dialog, setupPrompted, snapshot]);

  const currentTool = useMemo(
    () => snapshot.tools.find((tool) => tool.id === selectedTool) ?? snapshot.tools[0],
    [selectedTool, snapshot.tools],
  );
  const currentAutoSwitch = currentTool
    ? snapshot.autoSwitchSettings[currentTool.id] ?? {
        enabled: snapshot.autoSwitch,
        threshold: snapshot.autoSwitchThreshold,
      }
    : { enabled: false, threshold: 100 };

  useEffect(() => {
    if (currentTool && currentTool.id !== selectedTool) {
      setSelectedTool(currentTool.id);
    }
  }, [currentTool, selectedTool]);

  // Switching tool tab → clear the previous tool's switch notice (a Claude notice
  // must not leak onto Codex/Antigravity).
  useEffect(() => {
    setSwitchNotice(null);
  }, [selectedTool]);

  const run = async (
    key: string,
    task: () => Promise<AppSnapshot>,
    success?: string,
    notice?: string,
  ): Promise<boolean> => {
    setBusy(key);
    setError(null);
    try {
      const next = await task();
      setSnapshot(next);
      if (success) setToast(success);
      if (notice) setSwitchNotice(notice);
      return true;
    } catch (err) {
      setError(errorMessage(err));
      return false;
    } finally {
      setBusy(null);
    }
  };

  const refreshOneAccount = async (toolId: ToolId, accountId: string) => {
    setRefreshingAccounts((prev) => new Set([...prev, accountId]));
    try {
      const updated = await api.refreshAccount(toolId, accountId);
      // Merge only the refreshed account into the current snapshot so concurrent
      // refreshes don't clobber each other's results.
      setSnapshot((prev) => {
        const updatedAccount = updated.tools
          .find((t) => t.id === toolId)
          ?.accounts.find((a) => a.id === accountId);
        if (!updatedAccount) return prev;
        return {
          ...prev,
          tools: prev.tools.map((t) =>
            t.id !== toolId
              ? t
              : { ...t, accounts: t.accounts.map((a) => (a.id !== accountId ? a : updatedAccount)) },
          ),
        };
      });
    } catch {
      // quota error is surfaced inside account.quota.error — no global toast needed
    } finally {
      setRefreshingAccounts((prev) => {
        const next = new Set(prev);
        next.delete(accountId);
        return next;
      });
    }
  };

  const refresh = () => {
    const tool = snapshot.tools.find((t) => t.id === selectedTool);
    if (!tool) return;
    // Fire all account refreshes in parallel so each card updates independently.
    for (const account of tool.accounts) {
      if (account.state !== "needs-login" && !account.apiProvider) {
        void refreshOneAccount(selectedTool, account.id);
      }
    }
  };

  const switchAccount = (tool: ToolStatus, account: Account) =>
    run(
      `switch-${account.id}`,
      () => api.switchAccount({ toolId: tool.id, accountId: account.id }),
      tool.id === "antigravity"
        ? `Switched to: ${account.name}`
        : `Now using: ${account.name} (${tool.name})`,
      tool.id === "antigravity"
        ? `Loaded account ${account.name}. Antigravity IDE is restarting to apply.`
        : `Selected ${account.name}. A new terminal running \`${tool.id}\` uses it right away; in an open terminal run \`aisw\` to sync.`,
    );

  return (
    <main className="shell">
      <header className="topbar" data-tauri-drag-region />

      {!snapshot.disclaimerAccepted && (
        <section className="notice">
          <ShieldAlert />
          <span>
            Note: using multiple subscription accounts may violate the provider's terms. This app
            only manages logins — use at your own discretion.
          </span>
          <button onClick={() => run("disclaimer", api.acceptDisclaimer)}>Got it</button>
        </section>
      )}

      {error && (
        <section className="error" role="alert">
          <AlertTriangle />
          <span>{error}</span>
          <button className="iconButton" onClick={() => setError(null)} title="Dismiss">
            <X />
          </button>
        </section>
      )}

      {toast && (
        <section className="toast" onAnimationEnd={() => setToast(null)}>
          <Check />
          <span>{toast}</span>
        </section>
      )}

      {autoSwitchBanner && (
        <section className="autoBanner">
          <RotateCcw />
          <span>{autoSwitchBanner}</span>
          <button className="iconButton" onClick={() => setAutoSwitchBanner(null)} title="Dismiss">
            <X />
          </button>
        </section>
      )}

      <div className="workspace">
        <aside className="sidebar">
          <div className="sidebarNav">
            {snapshot.tools.map((tool) => (
              <button
                className={`toolTab ${view === "accounts" && tool.id === selectedTool ? "selected" : ""}`}
                key={tool.id}
                onClick={() => {
                  setView("accounts");
                  setSelectedTool(tool.id);
                }}
              >
                <span>{tool.name}</span>
                <small>{tool.installed ? activeLabel(tool) : "Tool not installed"}</small>
              </button>
            ))}
            <div className="sideDivider" />
            <button
              className={`toolTab ${view === "usage" ? "selected" : ""}`}
              onClick={() => setView("usage")}
            >
              <span className="usageTabLabel">
                <BarChart3 />
                Usage
              </span>
              <small>Token &amp; cost</small>
            </button>
            <button
              className={`toolTab ${view === "settings" ? "selected" : ""}`}
              onClick={() => setView("settings")}
            >
              <span className="usageTabLabel">
                <Settings />
                Settings
              </span>
              <small>CLI paths</small>
            </button>
          </div>

          <div className="sidebarFoot">
            <img className="footLogo" src={logoUrl} alt="" />
            <div className="footMeta">
              <strong>AI Account Switcher</strong>
              <small>{version ? `v${version}` : ""}</small>
              <button
                className="footBy"
                onClick={() => void openUrl("https://hoangphan.blog/").catch(() => {})}
                title="https://hoangphan.blog/"
              >
                Powered by Hoàng Phan
              </button>
            </div>
          </div>
        </aside>

        {view === "usage" && <UsageView />}

        {view === "settings" && (
          <SettingsView
            snapshot={snapshot}
            busy={busy !== null}
            onSetup={(toolId) => {
              setSelectedTool(toolId);
              setDialog("setup");
            }}
            onAutoSwitchChange={(toolId, enabled, threshold) =>
              run(`autoSwitch-${toolId}`, () => api.setAutoSwitchSetting(toolId, enabled, threshold))
            }
          />
        )}

        {view === "accounts" && currentTool && (
          <section className="panel">
            <div className="panelHead">
              <div>
                <div className="titleRow">
                  <h2>{currentTool.name}</h2>
                  <span className={`status ${currentTool.installed ? "ok" : "muted"}`}>
                    {currentTool.installed ? "Ready" : "Tool not installed"}
                  </span>
                  {currentTool.id !== "antigravity" && currentAutoSwitch.enabled && (
                    <span
                      className="autoChip"
                      title={`Auto-switch the bare ${currentTool.id} command at ${currentAutoSwitch.threshold}% quota`}
                    >
                      <Zap />
                      Auto {currentAutoSwitch.threshold}%
                    </span>
                  )}
                  <span className="helpDot" title={toolDescriptions[currentTool.id]}>
                    <CircleHelp />
                  </span>
                </div>
              </div>
              <div className="actions">
                {currentTool.id === "antigravity" && (
                  <button
                    onClick={() =>
                      run(
                        "agNewLogin",
                        api.antigravityNewLogin,
                        undefined,
                        "Antigravity IDE opened at the sign-in screen. After signing into the new account, click “Save current account”.",
                      )
                    }
                    disabled={!currentTool.installed || busy !== null}
                  >
                    <LogIn />
                    Sign in new account
                  </button>
                )}
                <button
                  onClick={() => setDialog("add")}
                  disabled={!currentTool.installed || busy !== null}
                >
                  <LogIn />
                  {currentTool.id === "antigravity" ? "Save current account" : "Add account"}
                </button>
                <button
                  onClick={refresh}
                  disabled={busy !== null || refreshingAccounts.size > 0}
                >
                  {refreshingAccounts.size > 0 ? (
                    <Loader2 className="spin" />
                  ) : (
                    <RefreshCw />
                  )}
                  Refresh quota
                </button>
              </div>
            </div>

            {switchNotice && (
              <div className="drift">
                <CircleHelp />
                <span>{switchNotice}</span>
                <button className="iconButton" onClick={() => setSwitchNotice(null)} title="Dismiss">
                  <X />
                </button>
              </div>
            )}

            <div className="accountGrid">
              {currentTool.accounts.length === 0 ? (
                <div className="empty">
                  <KeyRound />
                  <strong>No accounts yet</strong>
                  <span>Click Add account to log in a new account (each account gets its own command).</span>
                </div>
              ) : (
                currentTool.accounts.map((account) => (
                  <AccountCard
                    key={account.id}
                    account={account}
                    tool={currentTool}
                    busy={busy}
                    onSwitch={() => switchAccount(currentTool, account)}
                    onRename={() => {
                      setSelectedAccount(account);
                      setDialog("rename");
                    }}
                    onSetLauncher={() => {
                      setSelectedAccount(account);
                      setDialog("launcher");
                    }}
                    onCopy={(text) => {
                      void navigator.clipboard?.writeText(text);
                      setToast(`Copied: ${text}`);
                    }}
                    onDelete={() =>
                      run("delete", () => api.deleteAccount(currentTool.id, account.id))
                    }
                    onRefreshQuota={() => void refreshOneAccount(currentTool.id, account.id)}
                    refreshingQuota={refreshingAccounts.has(account.id)}
                  />
                ))
              )}
            </div>
          </section>
        )}
      </div>

      {dialog === "add" && currentTool && (
        <AddDialog
          tool={currentTool}
          onClose={() => setDialog(null)}
          onSubmit={async (input) => {
            if (await run("add", () => api.addAccount(input))) {
              setDialog(null);
            }
          }}
          onSubmitApi={async (input) => {
            if (await run("add", () => api.addApiAccount(input))) {
              setDialog(null);
            }
          }}
        />
      )}

      {dialog === "rename" && currentTool && selectedAccount && (
        <NameDialog
          title="Rename account"
          label="Account name"
          initialName={selectedAccount.name}
          maxLength={20}
          submitText="Save"
          onClose={() => setDialog(null)}
          onSubmit={async (name) => {
            if (
              await run("rename", () =>
                api.renameAccount({ toolId: currentTool.id, accountId: selectedAccount.id, name }),
              )
            ) {
              setDialog(null);
            }
          }}
        />
      )}

      {dialog === "launcher" && currentTool && selectedAccount && (
        <NameDialog
          title={`Custom command (${currentTool.id}-…)`}
          label="Command name"
          hint={`Forces the ${currentTool.id}- prefix, only a-z 0-9 - _`}
          initialName={selectedAccount.launcherCommand ?? ""}
          maxLength={47}
          submitText="Save command"
          onClose={() => setDialog(null)}
          onSubmit={async (name) => {
            if (
              await run("launcher", () =>
                api.setLauncher({ toolId: currentTool.id, accountId: selectedAccount.id, name }),
              )
            ) {
              setDialog(null);
            }
          }}
        />
      )}

      {dialog === "setup" && currentTool && currentTool.id !== "antigravity" && (
        <CliSetupDialog
          tool={currentTool}
          currentSetup={snapshot.toolSetups[currentTool.id]}
          onClose={() => setDialog(null)}
          onSave={async (input) => {
            if (await run("setup", () => api.setToolSetup(input), "CLI setup saved")) {
              setDialog(null);
            }
          }}
        />
      )}
    </main>
  );
}

function SettingsView({
  snapshot,
  busy,
  onSetup,
  onAutoSwitchChange,
}: {
  snapshot: AppSnapshot;
  busy: boolean;
  onSetup: (toolId: ToolId) => void;
  onAutoSwitchChange: (toolId: ToolId, enabled: boolean, threshold: number) => void;
}) {
  const cliTools = snapshot.tools.filter((tool) => tool.id !== "antigravity");
  return (
    <section className="panel">
      <div className="panelHead">
        <div>
          <div className="titleRow">
            <h2>Settings</h2>
            <span className="status ok">CLI paths</span>
          </div>
        </div>
      </div>

      <div className="settingsList">
        {cliTools.map((tool) => (
          <CliSetupBar
            key={tool.id}
            tool={tool}
            setup={snapshot.toolSetups[tool.id]}
            onOpen={() => onSetup(tool.id)}
          />
        ))}

        <div className="settingsSection">
          <div className="settingsSectionHead">
            <Zap />
            <div>
              <strong>Auto-switch</strong>
              <small>Configure quota fallback separately for each CLI.</small>
            </div>
          </div>
          {cliTools.map((tool) => {
            const setting = snapshot.autoSwitchSettings[tool.id] ?? {
              enabled: snapshot.autoSwitch,
              threshold: snapshot.autoSwitchThreshold,
            };
            return (
              <AutoSwitchBar
                key={tool.id}
                tool={tool}
                enabled={setting.enabled}
                threshold={setting.threshold}
                busy={busy}
                onChange={(enabled, threshold) => onAutoSwitchChange(tool.id, enabled, threshold)}
              />
            );
          })}
        </div>
      </div>
    </section>
  );
}

function CliSetupBar({
  tool,
  setup,
  onOpen,
}: {
  tool: ToolStatus;
  setup: ToolSetup | undefined;
  onOpen: () => void;
}) {
  const ready = !!setup?.binaryPath && !!setup?.defaultConfigDir;
  return (
    <div className={`cliSetup ${ready ? "ready" : "needsSetup"}`}>
      <Terminal />
      <div className="cliSetupText">
        <strong>{ready ? "CLI setup resolved" : "CLI setup needs review"}</strong>
        <small>
          {ready
            ? `${shortPath(setup!.binaryPath!)} · ${shortPath(setup!.defaultConfigDir!)}`
            : `Set where ${tool.name} is installed and where its main config lives.`}
        </small>
      </div>
      <button type="button" onClick={onOpen}>
        {ready ? "Review" : "Setup"}
      </button>
    </div>
  );
}

function AccountCard({
  account,
  tool,
  busy,
  onSwitch,
  onRename,
  onSetLauncher,
  onCopy,
  onDelete,
  onRefreshQuota,
  refreshingQuota,
}: {
  account: Account;
  tool: ToolStatus;
  busy: string | null;
  onSwitch: () => void;
  onRename: () => void;
  onSetLauncher: () => void;
  onCopy: (text: string) => void;
  onDelete: () => void;
  onRefreshQuota: () => void;
  refreshingQuota: boolean;
}) {
  const isAntigravity = tool.id === "antigravity";
  const isApi = !!account.apiProvider;
  const isActive = account.id === tool.activeAccountId || account.state === "active";
  const exhausted = account.state === "exhausted";
  const needsLogin = account.state === "needs-login";

  return (
    <article className={`account ${isActive ? "active" : ""} ${exhausted ? "exhausted" : ""}`}>
      <div className="accountTop">
        <div className="accountIdentity">
          {isAntigravity &&
            (account.avatarUrl ? (
              <img className="avatar" src={account.avatarUrl} alt="" referrerPolicy="no-referrer" />
            ) : (
              <span className="avatar avatarFallback">
                {account.name.slice(0, 1).toUpperCase()}
              </span>
            ))}
          <div className="identityText">
            <div className="accountName">
              {isActive && <span className="dot" aria-hidden />}
              <h3 title={account.name}>{account.name}</h3>
              {account.quota?.plan && <span className="plan">{account.quota.plan}</span>}
              {account.launcherCommand && (
                <button
                  className="cmdChip"
                  onClick={() => onCopy(account.launcherCommand!)}
                  title={`Copy command: ${account.launcherCommand}`}
                >
                  <code>{account.launcherCommand}</code>
                  <Copy />
                </button>
              )}
            </div>
            {isApi ? (
              <span className="fingerprint" title={account.apiProvider!.baseUrl}>
                via {gatewayHost(account.apiProvider!.baseUrl)}
              </span>
            ) : (
              account.isDefault && <span className="fingerprint">Machine default</span>
            )}
          </div>
        </div>
        <div className="badgeRow">
          {isApi && <span className="badge api">API</span>}
          <span className={`badge ${badgeClass(account.state)}`}>
            {exhausted ? "Out of quota" : isActive ? "In use" : stateLabel(account.state)}
          </span>
        </div>
      </div>

      {isApi ? (
        <p className="apiMeta">
          Model <code>{account.apiProvider!.model}</code>
        </p>
      ) : (
        <Quota quota={account.quota} />
      )}

      {needsLogin && (
        <div className="pendingLogin">
          <Loader2 className="spin" />
          <span>
            {isAntigravity
              ? "Waiting for sign-in in the Antigravity IDE window — the app will detect it."
              : "Waiting for login in Terminal — the app will detect it."}
          </span>
        </div>
      )}

      <div className="cardActions">
        <button onClick={onSwitch} disabled={isActive || needsLogin || busy !== null}>
          <RotateCcw /> Use
        </button>
        {!isApi && !needsLogin && (
          <button
            className="iconButton"
            onClick={onRefreshQuota}
            disabled={refreshingQuota || busy !== null}
            title="Refresh quota"
          >
            {refreshingQuota ? <Loader2 className="spin" /> : <RefreshCw />}
          </button>
        )}
        {tool.id !== "antigravity" && !account.isDefault && (
          <button
            className="iconButton"
            onClick={onSetLauncher}
            disabled={needsLogin || busy !== null}
            title="Custom command"
          >
            <Terminal />
          </button>
        )}
        {!account.isDefault && (
          <button
            className="iconButton"
            onClick={onRename}
            disabled={needsLogin || busy !== null}
            title="Rename"
          >
            <Pencil />
          </button>
        )}
        {!account.isDefault && (
          <button className="iconButton danger" onClick={onDelete} disabled={busy !== null} title="Delete">
            <Trash2 />
          </button>
        )}
      </div>
    </article>
  );
}

function AutoSwitchBar({
  tool,
  enabled,
  threshold,
  busy,
  onChange,
}: {
  tool: ToolStatus;
  enabled: boolean;
  threshold: number;
  busy: boolean;
  onChange: (enabled: boolean, threshold: number) => void;
}) {
  return (
    <div className={`autoSwitch ${enabled ? "on" : ""}`}>
      <div className="autoSwitchMain">
        <span className="autoSwitchIcon" aria-hidden>
          <Zap />
        </span>
        <div className="autoSwitchText">
          <strong>{tool.name}</strong>
          <small>
            Switch the bare <code>{tool.id}</code> command to another account when quota is nearly out.
          </small>
        </div>
        <button
          type="button"
          role="switch"
          aria-checked={enabled}
          className={`switchTrack ${enabled ? "on" : ""}`}
          disabled={busy}
          onClick={() => onChange(!enabled, threshold)}
        >
          <span className="switchThumb" />
        </button>
      </div>
      {enabled && (
        <div className="autoSwitchOpts">
          <span>Switch after using</span>
          {[90, 95, 100].map((value) => (
            <button
              key={value}
              type="button"
              className={`pill ${threshold === value ? "active" : ""}`}
              disabled={busy}
              onClick={() => onChange(enabled, value)}
            >
              {value === 100 ? "100% (fully out)" : `${value}%`}
            </button>
          ))}
        </div>
      )}
    </div>
  );
}

function Quota({ quota }: { quota: Account["quota"] }) {
  if (!quota) return <p className="quotaError">No quota data yet</p>;
  if (quota.error) return <p className="quotaError">{quota.error}</p>;

  // Antigravity: quota is reported per model instead of as a single overall window.
  if (quota.models && quota.models.length > 0) {
    return (
      <div className="quotaBox">
        <span className="quotaTitle">Quota by model</span>
        {quota.models.map((model) => (
          <QuotaBar key={model.label} label={model.label} percent={model.percentUsed} resetAt={model.resetAt} />
        ))}
      </div>
    );
  }

  return (
    <div className="quotaBox">
      <QuotaBar label="5-hour limit" percent={quota.fiveHour.percentUsed} resetAt={quota.fiveHour.resetAt} />
      <QuotaBar label="Weekly limit" percent={quota.weekly.percentUsed} resetAt={quota.weekly.resetAt} />
    </div>
  );
}

function QuotaBar({ label, percent, resetAt }: { label: string; percent: number | null; resetAt: string | null }) {
  const value = Math.max(0, Math.min(100, percent ?? 0));
  const level = percent === null ? "unknown" : value >= 90 ? "high" : value >= 70 ? "mid" : "low";
  return (
    <div className="quotaLine">
      <div className="quotaMeta">
        <span className="quotaLabel">{label}</span>
        <strong>{percent === null ? "?" : `${Math.round(value)}%`}</strong>
        {resetAt && (
          <span className="quotaReset" title={`Resets at ${formatTime(resetAt)}`}>
            {formatReset(resetAt)}
          </span>
        )}
      </div>
      <div className="bar" data-level={level}>
        <span style={{ width: `${value}%` }} />
      </div>
    </div>
  );
}

function AddDialog({
  tool,
  onClose,
  onSubmit,
  onSubmitApi,
}: {
  tool: ToolStatus;
  onClose: () => void;
  onSubmit: (input: AddAccountInput) => Promise<void>;
  onSubmitApi: (input: AddApiAccountInput) => Promise<void>;
}) {
  const isCli = tool.id !== "antigravity";
  // API/proxy accounts are supported for the CLI tools (Codex + Claude Code).
  const canApi = tool.id === "codex" || tool.id === "claude";
  const bypassFlag =
    tool.id === "claude"
      ? "--dangerously-skip-permissions"
      : "--dangerously-bypass-approvals-and-sandbox";
  const [kind, setKind] = useState<"login" | "api">("login");
  const isApi = canApi && kind === "api";

  const [name, setName] = useState("");
  const [launcher, setLauncher] = useState("");
  const [message, setMessage] = useState<string | null>(null);

  // API-mode state.
  const [baseUrl, setBaseUrl] = useState("");
  const [apiKey, setApiKey] = useState("");
  const [models, setModels] = useState<string[]>([]);
  const [fetching, setFetching] = useState(false);
  const [model, setModel] = useState("");
  const [bypass, setBypass] = useState(false);

  const fetchModels = async () => {
    if (!baseUrl.trim().startsWith("https://")) {
      setMessage("Gateway URL must start with https://");
      return;
    }
    if (!apiKey.trim()) {
      setMessage("API key is required");
      return;
    }
    setMessage(null);
    setFetching(true);
    try {
      const list = await api.fetchGatewayModels(baseUrl.trim(), apiKey.trim());
      setModels(list);
      if (list.length > 0 && !model) setModel(list[0]);
    } catch (err) {
      setMessage(errorMessage(err));
    } finally {
      setFetching(false);
    }
  };

  const submit = async () => {
    if (name.trim().length > 20) {
      setMessage("Account name is limited to 20 characters");
      return;
    }
    if (isCli && launcher.trim() === "") {
      setMessage(`A custom command is required (e.g. ${tool.id}-work)`);
      return;
    }
    await onSubmit({
      toolId: tool.id,
      name: name.trim(),
      mode: isCli ? "login" : "import",
      launcher: isCli ? launcher.trim() : undefined,
    });
  };

  const submitApi = async () => {
    if (name.trim().length > 20) {
      setMessage("Account name is limited to 20 characters");
      return;
    }
    if (models.length === 0) {
      setMessage("Fetch the gateway models first");
      return;
    }
    if (!model) {
      setMessage("Pick a model");
      return;
    }
    await onSubmitApi({
      toolId: tool.id,
      name: name.trim(),
      baseUrl: baseUrl.trim(),
      apiKey: apiKey.trim(),
      model,
      launcher: launcher.trim() || undefined,
      bypass,
    });
  };

  return (
    <div className="modalBackdrop" role="presentation" onMouseDown={onClose}>
      <section className="modal" onMouseDown={(event) => event.stopPropagation()}>
        <h2>{isCli ? `Add ${tool.name} account` : "Save the signed-in Antigravity IDE account"}</h2>

        {canApi && (
          <div className="kindToggle">
            <button
              type="button"
              className={kind === "login" ? "active" : ""}
              onClick={() => setKind("login")}
            >
              Subscription (login)
            </button>
            <button
              type="button"
              className={kind === "api" ? "active" : ""}
              onClick={() => setKind("api")}
            >
              API / Proxy
            </button>
          </div>
        )}

        <label>
          Account name
          <input
            autoFocus
            value={name}
            maxLength={20}
            onChange={(event) => setName(event.target.value)}
            placeholder="Auto-generated if left blank"
          />
        </label>

        {isCli && !isApi && (
          <label>
            <span className="labelRow">
              Custom command (required)
              <span
                className="helpDot"
                title={`Open a terminal and run this command to use the account separately, in parallel. Forces the ${tool.id}- prefix, only a-z 0-9 - _`}
              >
                <CircleHelp />
              </span>
            </span>
            <input
              value={launcher}
              onChange={(event) => setLauncher(event.target.value)}
              placeholder={`${tool.id}-work`}
            />
          </label>
        )}

        {isApi && (
          <>
            <label>
              Gateway URL
              <input
                value={baseUrl}
                onChange={(event) => setBaseUrl(event.target.value)}
                placeholder="https://your-gateway.com/v1"
              />
            </label>
            <label>
              API key
              <input
                type="password"
                value={apiKey}
                onChange={(event) => setApiKey(event.target.value)}
                placeholder="sk-…"
              />
            </label>
            <button type="button" className="fetchModels" onClick={fetchModels} disabled={fetching}>
              {fetching ? <Loader2 className="spin" /> : <RefreshCw />}
              {models.length > 0 ? `Models loaded (${models.length})` : "Fetch models"}
            </button>

            {models.length > 0 && (
              <>
                <label>
                  <span className="labelRow">
                    Model (required)
                    <span
                      className="helpDot"
                      title="The gateway model this account runs. The CLI's model picker can't switch gateway models — add a separate account for a different model."
                    >
                      <CircleHelp />
                    </span>
                  </span>
                  <select value={model} onChange={(event) => setModel(event.target.value)}>
                    {models.map((id) => (
                      <option key={id} value={id}>
                        {id}
                      </option>
                    ))}
                  </select>
                </label>
                <p className="hint">
                  One account = one model. The gateway only knows its own model ids — for another
                  model, add another account.
                </p>

                <label>
                  <span className="labelRow">
                    Custom command (optional)
                    <span
                      className="helpDot"
                      title={`A separate command (e.g. ${tool.id}-p) to use this account in its own terminal. Forces the ${tool.id}- prefix.`}
                    >
                      <CircleHelp />
                    </span>
                  </span>
                  <input
                    value={launcher}
                    onChange={(event) => setLauncher(event.target.value)}
                    placeholder={`${tool.id}-p`}
                  />
                </label>

                <label className="checkRow">
                  <input
                    type="checkbox"
                    checked={bypass}
                    onChange={(event) => setBypass(event.target.checked)}
                  />
                  <span>
                    Bypass approvals &amp; sandbox in the custom command
                    <small>Adds {bypassFlag}. Off by default.</small>
                  </span>
                </label>
              </>
            )}
          </>
        )}

        {!isCli && (
          <p className="hint">
            Make sure Antigravity IDE is signed into the account you want to save, then click Save.
            The app will quit &amp; reopen the IDE to capture the right session (the IDE only writes
            the token on quit).
          </p>
        )}
        {message && <p className="quotaError">{message}</p>}
        <div className="modalActions">
          <button onClick={onClose}>Cancel</button>
          <button className="primary" onClick={isApi ? submitApi : submit}>
            {isApi ? "Create account" : isCli ? "Create & login" : "Save this account"}
          </button>
        </div>
      </section>
    </div>
  );
}

function NameDialog({
  title,
  label,
  hint,
  initialName,
  maxLength,
  submitText,
  onClose,
  onSubmit,
}: {
  title: string;
  label: string;
  hint?: string;
  initialName: string;
  maxLength: number;
  submitText: string;
  onClose: () => void;
  onSubmit: (name: string) => Promise<void>;
}) {
  const [name, setName] = useState(initialName);

  return (
    <div className="modalBackdrop" role="presentation" onMouseDown={onClose}>
      <section className="modal" onMouseDown={(event) => event.stopPropagation()}>
        <h2>{title}</h2>
        <label>
          <span className="labelRow">
            {label}
            {hint && (
              <span className="helpDot" title={hint}>
                <CircleHelp />
              </span>
            )}
          </span>
          <input
            autoFocus
            value={name}
            maxLength={maxLength}
            onChange={(event) => setName(event.target.value)}
          />
        </label>
        <div className="modalActions">
          <button onClick={onClose}>Cancel</button>
          <button className="primary" onClick={() => onSubmit(name.trim())}>
            {submitText}
          </button>
        </div>
      </section>
    </div>
  );
}

function CliSetupDialog({
  tool,
  currentSetup,
  onClose,
  onSave,
}: {
  tool: ToolStatus;
  currentSetup: ToolSetup | undefined;
  onClose: () => void;
  onSave: (input: SetToolSetupInput) => Promise<void>;
}) {
  const [report, setReport] = useState<DetectionReport | null>(null);
  const [binaryPath, setBinaryPath] = useState(currentSetup?.binaryPath ?? "");
  const [configDir, setConfigDir] = useState(currentSetup?.defaultConfigDir ?? "");
  const [message, setMessage] = useState<string | null>(null);
  const [loading, setLoading] = useState(true);
  const [manual, setManual] = useState(false);

  const detect = useCallback(async () => {
    setLoading(true);
    setMessage(null);
    try {
      const next = await api.detectToolSetup(tool.id);
      setReport(next);
      if (next.resolution.setup?.binaryPath) setBinaryPath(next.resolution.setup.binaryPath);
      if (next.resolution.setup?.defaultConfigDir) setConfigDir(next.resolution.setup.defaultConfigDir);
      if (!next.resolution.setup) {
        setManual(true);
        setMessage(next.resolution.reason);
      }
    } catch (err) {
      setMessage(errorMessage(err));
    } finally {
      setLoading(false);
    }
  }, [tool.id]);

  useEffect(() => {
    void detect();
  }, [detect]);

  const save = async () => {
    if (!binaryPath.trim() || !configDir.trim()) {
      setMessage("Both binary path and config directory are required");
      return;
    }
    await onSave({
      toolId: tool.id,
      binaryPath: binaryPath.trim(),
      defaultConfigDir: configDir.trim(),
    });
  };

  const binaryChoices = report?.binaryCandidates ?? [];
  const configChoices = report?.configCandidates ?? [];
  const simpleRecommendation =
    binaryChoices.length <= 1 &&
    configChoices.length <= 1 &&
    Boolean(binaryPath.trim()) &&
    Boolean(configDir.trim());
  const canEditPaths = manual || !simpleRecommendation;

  return (
    <div className="modalBackdrop" role="presentation" onMouseDown={onClose}>
      <section className="modal setupModal" onMouseDown={(event) => event.stopPropagation()}>
        <div className="modalTitleRow">
          <div>
            <h2>{tool.name} paths</h2>
            <p className="modalSub">
              Review the paths detected for this CLI.
            </p>
          </div>
          <button className="iconButton" onClick={detect} disabled={loading} title="Re-detect">
            <RefreshCw className={loading ? "spin" : ""} />
          </button>
        </div>

        <div className="setupCurrent">
          {canEditPaths ? (
            <>
              <label>
                CLI executable
                <input value={binaryPath} onChange={(event) => setBinaryPath(event.target.value)} />
              </label>
              <label>
                Main config folder
                <input value={configDir} onChange={(event) => setConfigDir(event.target.value)} />
              </label>
            </>
          ) : (
            <>
              <ReadOnlyPath label="CLI executable" value={binaryPath} />
              <ReadOnlyPath label="Main config folder" value={configDir} />
            </>
          )}
        </div>

        {simpleRecommendation && (
          <div className="setupSummary">
            <Check />
            <div>
              <strong>Recommended paths found</strong>
              <small>No extra choice is needed.</small>
            </div>
          </div>
        )}

        {report && !simpleRecommendation && (
          <div className="candidateGrid">
            <CandidateList
              title="Detected binaries"
              kind="binary"
              items={report.binaryCandidates}
              selected={binaryPath}
              onSelect={setBinaryPath}
            />
            <CandidateList
              title="Detected config folders"
              kind="config"
              items={report.configCandidates}
              selected={configDir}
              onSelect={setConfigDir}
            />
          </div>
        )}

        {message && <p className="quotaError">{message}</p>}
        <div className="modalActions">
          {!canEditPaths && (
            <button type="button" onClick={() => setManual(true)}>
              Enter manually
            </button>
          )}
          <button onClick={onClose}>Cancel</button>
          <button className="primary" onClick={save}>
            {simpleRecommendation ? "Save recommended paths" : "Save paths"}
          </button>
        </div>
      </section>
    </div>
  );
}

function ReadOnlyPath({ label, value }: { label: string; value: string }) {
  return (
    <div className="readonlyPath">
      <span>{label}</span>
      <code>{value}</code>
    </div>
  );
}

function CandidateList({
  title,
  kind,
  items,
  selected,
  onSelect,
}: {
  title: string;
  kind: "binary" | "config";
  items: Array<BinaryCandidate | ConfigCandidate>;
  selected: string;
  onSelect: (path: string) => void;
}) {
  return (
    <div className="candidateList">
      <strong>{title}</strong>
      {items.length === 0 ? (
        <span className="candidateEmpty">Nothing detected. Enter the path manually above.</span>
      ) : (
        items.map((item, index) => (
          <button
            key={`${item.source}:${item.path}`}
            type="button"
            className={`candidate ${selected === item.path ? "selected" : ""} ${item.valid ? "" : "weak"}`}
            onClick={() => onSelect(item.path)}
          >
            <span className="candidatePath">{item.path}</span>
            <span className="candidateMeta">
              {candidateLabel(item, index, kind)}
            </span>
            {item.warnings.length > 0 && <span className="candidateWarn">{item.warnings[0]}</span>}
          </button>
        ))
      )}
    </div>
  );
}

function activeLabel(tool: ToolStatus) {
  const account = tool.accounts.find((item) => item.id === tool.activeAccountId);
  const name = !account || account.isDefault ? "Machine default" : account.name;
  // Antigravity is a GUI app with no "bare command", so use different wording.
  return tool.id === "antigravity" ? `Using: ${name}` : `Bare command = ${name}`;
}

function stateLabel(state: Account["state"]) {
  if (state === "needs-login") return "Signing in";
  if (state === "exhausted") return "Out of quota";
  if (state === "active") return "In use";
  return "Ready";
}

function badgeClass(state: Account["state"]) {
  if (state === "active") return "ok";
  if (state === "exhausted") return "warn";
  if (state === "needs-login") return "bad";
  return "muted";
}

function shortPath(path: string) {
  const home = "/Users/";
  if (path.startsWith(home)) {
    const parts = path.split("/");
    if (parts.length > 3) return `~/${parts.slice(3).join("/")}`;
  }
  return path.length > 58 ? `…${path.slice(-55)}` : path;
}

function sourceLabel(source: string) {
  if (source === "env") return "from environment";
  if (source === "default") return "standard location";
  if (source === "path") return "found in PATH";
  if (source === "manual") return "manual";
  return source;
}

function candidateLabel(item: BinaryCandidate | ConfigCandidate, index: number, kind: "binary" | "config") {
  const tags = [index === 0 && item.valid ? "Recommended" : null, sourceLabel(item.source)]
    .filter(Boolean)
    .join(" · ");
  if (!item.valid) return `${tags} · needs review`;
  if (kind === "config" && item.source === "default") return `${tags} · main config`;
  return tags;
}

function formatTime(value: string) {
  return new Intl.DateTimeFormat("en-US", {
    dateStyle: "short",
    timeStyle: "short",
  }).format(new Date(value));
}

/** Compact reset stamp shown inline on a quota row, e.g. "06/07, 17:07". */
function formatReset(value: string) {
  return new Intl.DateTimeFormat("en-US", {
    month: "2-digit",
    day: "2-digit",
    hour: "2-digit",
    minute: "2-digit",
    hour12: false,
  }).format(new Date(value));
}

function errorMessage(err: unknown) {
  return err instanceof Error ? err.message : String(err);
}
