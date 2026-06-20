import {
  AlarmClock,
  AlertTriangle,
  ArrowDown,
  ArrowUp,
  BarChart3,
  Boxes,
  Check,
  ChevronDown,
  CircleHelp,
  Copy,
  Info,
  KeyRound,
  Layers,
  Loader2,
  LogIn,
  Pencil,
  Plus,
  RefreshCw,
  RotateCcw,
  Server,
  Settings,
  ShieldAlert,
  Terminal,
  Trash2,
  Users,
  X,
  Zap,
} from "lucide-react";
import { useCallback, useEffect, useMemo, useState } from "react";
import { listen } from "@tauri-apps/api/event";
import { getVersion } from "@tauri-apps/api/app";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { openUrl } from "@tauri-apps/plugin-opener";
import { api } from "./tauri";
import { UsageView } from "./UsageView";
import { AutoSessionView } from "./AutoSessionView";
import logoUrl from "./assets/logo.svg";
import type {
  Account,
  AddAccountInput,
  AddApiAccountInput,
  ApiGatewayCombo,
  ApiPoolAccountState,
  ApiRotationStrategy,
  ApiUsageReport,
  AppSnapshot,
  AutoPrimeSetting,
  PrimeAttemptStatus,
  BinaryCandidate,
  ConfigCandidate,
  CreateApiGatewayKeyInput,
  DetectionReport,
  SaveApiGatewayComboInput,
  SetApiGatewayAccountInput,
  SetToolSetupInput,
  StartApiGatewayInput,
  ToolSetup,
  ToolId,
  ToolStatus,
} from "./types";

function primeAttemptSourceLabel(source: PrimeAttemptStatus["source"]): string {
  switch (source) {
    case "schedule":
      return "Lịch";
    case "autoExtend":
      return "Tự gia hạn";
    case "userExtend":
      return "Gia hạn";
    case "manual":
      return "Prime ngay";
    case "scheduleAutoExtend":
      return "Lịch + tự gia hạn";
    case "scheduleUserExtend":
      return "Lịch + gia hạn";
  }
}

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
  autoPrime: {},
  primeAttempts: {},
  toolSetups: {},
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
};

export function App() {
  const [snapshot, setSnapshot] = useState<AppSnapshot>(emptySnapshot);
  const [selectedTool, setSelectedTool] = useState<ToolId>("claude");
  const [view, setView] = useState<"accounts" | "api" | "usage" | "auto" | "settings">("accounts");
  // One unified notification channel for the whole app: success + error both render as a
  // top-right toast (see the Toasts renderer). `notify` is the single entry point.
  const [toasts, setToasts] = useState<
    { id: number; kind: "success" | "error" | "info"; text: string }[]
  >([]);
  const notify = useCallback((text: string, kind: "success" | "error" | "info" = "success") => {
    if (!text) return;
    const id = nextToastId();
    setToasts((current) => [...current.slice(-3), { id, kind, text }]);
    window.setTimeout(() => setToasts((current) => current.filter((item) => item.id !== id)), 4500);
  }, []);
  const dismissToast = useCallback(
    (id: number) => setToasts((current) => current.filter((item) => item.id !== id)),
    [],
  );
  const setToast = useCallback((text: string | null) => text && notify(text, "success"), [notify]);
  const setError = useCallback((text: string | null) => text && notify(text, "error"), [notify]);
  const [busy, setBusy] = useState<string | null>(null);
  const [refreshingAccounts, setRefreshingAccounts] = useState<Set<string>>(new Set());
  const [dialog, setDialog] = useState<"add" | "rename" | "launcher" | "setup" | null>(null);
  const [selectedAccount, setSelectedAccount] = useState<Account | null>(null);
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

  // Auto-refresh real quota only while the app window is FOCUSED. Losing focus (typing in another
  // app, even if this window is still visible) stops the polling; refocusing refreshes immediately
  // and resumes. `refresh_tool` fetches live usage; an `inFlight` guard coalesces overlapping
  // refreshes and the 60s per-account backend cache absorbs the rest, so fast focus toggling can't
  // hammer the provider's usage endpoint.
  useEffect(() => {
    let timer: number | undefined;
    let cancelled = false; // set on cleanup: blocks any async callback that resolves post-unmount
    let inFlight = false; // skip a new tick while the previous refresh pair is still running
    let unlisten: (() => void) | undefined;

    const tick = () => {
      if (cancelled || inFlight) return;
      inFlight = true;
      Promise.allSettled([
        api.refreshTool("claude").then(setSnapshot),
        api.refreshTool("codex").then(setSnapshot),
      ]).finally(() => {
        inFlight = false;
      });
    };
    const start = () => {
      if (cancelled || timer !== undefined) return;
      tick(); // refresh right away on (re)gaining focus
      timer = window.setInterval(tick, 150_000);
    };
    const stop = () => {
      if (timer !== undefined) {
        window.clearInterval(timer);
        timer = undefined;
      }
    };

    // Register the focus listener BEFORE reading the current focus, so we can't miss a blur that
    // happens between the isFocused() read and the listener being installed.
    const appWindow = getCurrentWindow();
    void appWindow
      .onFocusChanged(({ payload: focused }) => {
        if (cancelled) return;
        if (focused) start();
        else stop();
      })
      .then((fn) => {
        if (cancelled) {
          fn(); // effect already cleaned up while we were registering — unlisten immediately
          return;
        }
        unlisten = fn;
        // Now that the listener is live, sync to the current focus state.
        void appWindow.isFocused().then((focused) => {
          if (!cancelled && focused) start();
        });
      });

    return () => {
      cancelled = true;
      stop();
      unlisten?.();
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

  // Auto session prime updated a schedule / reminded to extend / primed → re-pull the snapshot.
  useEffect(() => {
    const unlisten = listen("auto-prime-changed", () => {
      void load();
    });
    return () => {
      void unlisten.then((fn) => fn());
    };
  }, [load]);

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

      <div className="toastStack">
        {toasts.map((item) => (
          <section
            className={`toast ${item.kind}`}
            role={item.kind === "error" ? "alert" : "status"}
            key={item.id}
          >
            {item.kind === "error" ? (
              <AlertTriangle />
            ) : item.kind === "info" ? (
              <Info />
            ) : (
              <Check />
            )}
            <span>{item.text}</span>
            <button className="iconButton" onClick={() => dismissToast(item.id)} title="Dismiss">
              <X />
            </button>
          </section>
        ))}
      </div>

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
              className={`toolTab ${view === "api" ? "selected" : ""}`}
              onClick={() => setView("api")}
            >
              <span className="usageTabLabel">
                <Server />
                API
              </span>
              <small>{snapshot.apiGateway.status.state === "running" ? "Server running" : "Local gateway"}</small>
            </button>
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
              className={`toolTab ${view === "auto" ? "selected" : ""}`}
              onClick={() => setView("auto")}
            >
              <span className="usageTabLabel">
                <AlarmClock />
                Auto Session
              </span>
              <small>Neo mốc reset 5h</small>
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

        {view === "auto" && (
          <AutoSessionView snapshot={snapshot} setSnapshot={setSnapshot} notify={notify} />
        )}

        {view === "api" && (
          <ApiGatewayView
            snapshot={snapshot}
            busy={busy !== null}
            onStart={(input) => run("api-start", () => api.startApiGateway(input), `API server đang chạy tại http://${input.bindHost}:${input.port}`)}
            onStop={() => run("api-stop", api.stopApiGateway)}
            onCreateKey={async (input) => {
              setBusy("api-key");
              try {
                const result = await api.createApiGatewayKey(input);
                setSnapshot(result.snapshot);
                const copied = await copyToClipboard(result.secret);
                notify(
                  copied
                    ? "API key created and copied to clipboard."
                    : "API key created — copy it now, it is shown only once.",
                  "success",
                );
                return result.secret;
              } catch (err) {
                notify(errorMessage(err), "error");
                return null;
              } finally {
                setBusy(null);
              }
            }}
            onDeleteKey={(keyId) => run("api-key-delete", () => api.deleteApiGatewayKey(keyId))}
            onCopyKey={async (keyId) => {
              try {
                const secret = await api.revealApiGatewayKey(keyId);
                const ok = await copyToClipboard(secret);
                notify(ok ? "API key copied to clipboard." : "Couldn't access the clipboard.", ok ? "success" : "error");
              } catch (err) {
                notify(errorMessage(err), "error");
              }
            }}
            onSaveCombo={(input) => run("api-combo", () => api.saveApiGatewayCombo(input))}
            onDeleteCombo={(comboId) => run("api-combo-delete", () => api.deleteApiGatewayCombo(comboId))}
            onSetAccount={(input) => run("api-account", () => api.setApiGatewayAccount(input))}
            onRefreshModels={() =>
              run("api-models", api.refreshApiGatewayModels, "Model registry updated")
            }
            onCreateVirtual={(toolId, model) =>
              run(`api-virtual-${toolId}`, () => api.createVirtualApiAccount(toolId, model))
            }
            onRefresh={async () => {
              setSnapshot(await api.loadSnapshot());
            }}
            onCopy={async (text) => {
              const ok = await copyToClipboard(text);
              notify(ok ? `Copied: ${text}` : "Couldn't access the clipboard.", ok ? "success" : "error");
            }}
          />
        )}

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
                    autoPrime={snapshot.autoPrime[account.id] ?? null}
                    primeAttempt={snapshot.primeAttempts[account.id] ?? null}
                    onExtend={(accept) =>
                      run("extend", () =>
                        api.confirmExtend({
                          toolId: currentTool.id,
                          accountId: account.id,
                          accept,
                        }),
                      )
                    }
                    onPrimeNow={async () => {
                      setBusy(`prime:${account.id}`);
                      setError(null);
                      try {
                        const res = await api.primeNow({
                          toolId: currentTool.id,
                          accountId: account.id,
                        });
                        // The backend emits auto-prime-changed → snapshot re-pulls itself; just toast.
                        // Pass the backend's kind straight through: "success" (new window), "info"
                        // (window still running — a Hold, neutral, not an error), "error" (failure).
                        notify(res.message, res.kind);
                      } catch (err) {
                        notify(errorMessage(err), "error");
                      } finally {
                        setBusy(null);
                      }
                    }}
                    onSwitch={() => switchAccount(currentTool, account)}
                    onRename={() => {
                      setSelectedAccount(account);
                      setDialog("rename");
                    }}
                    onSetLauncher={() => {
                      setSelectedAccount(account);
                      setDialog("launcher");
                    }}
                    onCopy={async (text) => {
                      const ok = await copyToClipboard(text);
                      notify(ok ? `Copied: ${text}` : "Couldn't access the clipboard.", ok ? "success" : "error");
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

/** A small on/off switch (replaces checkboxes across the API tab). */
function Toggle({
  checked,
  onChange,
  disabled,
  title,
}: {
  checked: boolean;
  onChange: (next: boolean) => void;
  disabled?: boolean;
  title?: string;
}) {
  return (
    <button
      type="button"
      className={`toggle ${checked ? "on" : ""}`}
      role="switch"
      aria-checked={checked}
      onClick={() => onChange(!checked)}
      disabled={disabled}
      title={title}
    >
      <span className="toggleKnob" />
    </button>
  );
}

function ApiGatewayView({
  snapshot,
  busy,
  onStart,
  onStop,
  onCreateKey,
  onDeleteKey,
  onCopyKey,
  onSaveCombo,
  onDeleteCombo,
  onSetAccount,
  onRefreshModels,
  onCreateVirtual,
  onRefresh,
  onCopy,
}: {
  snapshot: AppSnapshot;
  busy: boolean;
  onStart: (input: StartApiGatewayInput) => Promise<boolean>;
  onStop: () => Promise<boolean>;
  onCreateKey: (input: CreateApiGatewayKeyInput) => Promise<string | null>;
  onDeleteKey: (keyId: string) => Promise<boolean>;
  onCopyKey: (keyId: string) => Promise<void>;
  onSaveCombo: (input: SaveApiGatewayComboInput) => Promise<boolean>;
  onDeleteCombo: (comboId: string) => Promise<boolean>;
  onSetAccount: (input: SetApiGatewayAccountInput) => Promise<boolean>;
  onRefreshModels: () => Promise<boolean>;
  onCreateVirtual: (toolId: ToolId, model?: string) => Promise<boolean>;
  onRefresh: () => Promise<void>;
  onCopy: (text: string) => void;
}) {
  const gateway = snapshot.apiGateway;
  const [port, setPort] = useState(String(gateway.config.port));
  const [threshold, setThreshold] = useState(String(gateway.config.quotaThreshold));
  const [allowLan, setAllowLan] = useState(gateway.config.bindHost === "0.0.0.0");
  const [rotationStrategy, setRotationStrategy] = useState(gateway.config.rotationStrategy);
  const [showKeyModal, setShowKeyModal] = useState(false);
  const [comboEditing, setComboEditing] = useState<ApiGatewayCombo | null>(null);
  const [showComboModal, setShowComboModal] = useState(false);
  const [virtualTool, setVirtualTool] = useState<ToolId | null>(null);
  const [showModels, setShowModels] = useState(false);
  const [usage, setUsage] = useState<ApiUsageReport | null>(null);
  const [starting, setStarting] = useState(false);

  const running = gateway.status.state === "running";
  const hasVirtualClaude = snapshot.tools
    .find((tool) => tool.id === "claude")
    ?.accounts.some((account) => account.fingerprint === "api-local");
  const hasVirtualCodex = snapshot.tools
    .find((tool) => tool.id === "codex")
    ?.accounts.some((account) => account.fingerprint === "api-local");

  // Eligible subscription accounts (Claude/Codex, real login, not virtual), with their gateway
  // participation entry merged in for on/off + status display.
  const accounts = useMemo(
    () =>
      snapshot.tools
        .filter((tool) => tool.id === "claude" || tool.id === "codex")
        .flatMap((tool) =>
          tool.accounts
            .filter(
              (account) =>
                !account.apiProvider &&
                account.fingerprint !== "api-local" &&
                account.state !== "needs-login",
            )
            .map((account) => {
              const entry = gateway.config.accounts.find(
                (item) => item.toolId === tool.id && item.accountId === account.id,
              );
              return { tool, account, entry };
            }),
        ),
    [snapshot.tools, gateway.config.accounts],
  );

  // Every model the gateway can serve, grouped by provider (for the picker + the collapsible
  // "Available models" list). Primary source is the live per-account model registry; if that
  // hasn't been discovered yet we fall back to a small curated list so the picker is never blank.
  const modelsByProvider = useMemo(() => {
    const groups: { tool: ToolId; label: string; models: string[] }[] = [];
    for (const tool of ["claude", "codex"] as ToolId[]) {
      // Union the live registry with the curated known list, so the picker always offers the
      // full set of usable models (like 9router) even when account discovery is sparse.
      const models = new Set<string>(FALLBACK_MODELS[tool]);
      for (const registry of gateway.config.modelRegistry.filter((r) => r.toolId === tool)) {
        registry.models.forEach((model) => models.add(model));
      }
      groups.push({
        tool,
        label: tool === "claude" ? "Claude" : "Codex",
        models: Array.from(models).sort(),
      });
    }
    return groups;
  }, [gateway.config.modelRegistry]);

  useEffect(() => {
    api.getApiUsage().then(setUsage).catch(() => setUsage(null));
  }, [gateway.status.state, gateway.config.combos.length]);

  useEffect(() => {
    if (!running) return;
    const timer = window.setInterval(() => {
      void onRefresh().catch(() => {});
      void api.getApiUsage().then(setUsage).catch(() => {});
    }, 5_000);
    return () => window.clearInterval(timer);
  }, [onRefresh, running]);

  const submitStart = async () => {
    setStarting(true);
    try {
      const ok = await onStart({
        bindHost: allowLan ? "0.0.0.0" : "127.0.0.1",
        port: Number(port) || 8783,
        quotaThreshold: Number(threshold) || 95,
        rotationStrategy,
      });
      // Refresh the model registry in the background — Start no longer waits on it.
      if (ok) void onRefreshModels().catch(() => {});
    } finally {
      setStarting(false);
    }
  };

  const openCreateCombo = () => {
    setComboEditing(null);
    setShowComboModal(true);
    void onRefreshModels().catch(() => {});
  };
  const openEditCombo = (combo: ApiGatewayCombo) => {
    setComboEditing(combo);
    setShowComboModal(true);
    void onRefreshModels().catch(() => {});
  };

  return (
    <section className="panel">
      <div className="panelHead">
        <div>
          <div className="titleRow">
            <h2>API</h2>
            <span
              className={`status ${
                starting ? "muted" : running ? "ok" : gateway.status.state === "errored" ? "bad" : "muted"
              }`}
            >
              {starting
                ? "Starting…"
                : running
                  ? "Running"
                  : gateway.status.state === "errored"
                    ? "Errored"
                    : "Stopped"}
            </span>
          </div>
          <p className="panelLead">Local OpenAI/Anthropic-compatible gateway for Claude and Codex subscription accounts.</p>
        </div>
        <div className="actions">
          <button onClick={() => onCopy(gateway.status.baseUrl)}>
            <Copy />
            Base URL
          </button>
          {running ? (
            <button className="danger" onClick={onStop} disabled={busy}>
              <X />
              Stop
            </button>
          ) : (
            <button className="primary" onClick={submitStart} disabled={busy || starting}>
              {starting ? <Loader2 className="spin" /> : <Server />}
              {starting ? "Starting…" : "Start"}
            </button>
          )}
        </div>
      </div>

      {gateway.status.error && <p className="quotaError">{gateway.status.error}</p>}

      <div className="apiGrid">
        <section className="apiSection">
          <div className="settingsSectionHead">
            <Server />
            <div>
              <strong>Server</strong>
              <small>{gateway.status.baseUrl}</small>
            </div>
          </div>
          <div className="apiFormRow cols3">
            <label>
              Bind
              <input value={allowLan ? "0.0.0.0" : "127.0.0.1"} readOnly />
            </label>
            <label>
              Port
              <input value={port} onChange={(event) => setPort(event.target.value)} />
            </label>
            <label>
              <span>
                Quota cutoff <small>(%)</small>
              </span>
              <input
                value={threshold}
                onChange={(event) => setThreshold(event.target.value)}
                title="Drop an account from rotation once its quota usage passes this percentage."
              />
            </label>
          </div>
          <div className="apiToggleRow">
            <span>
              <strong>Allow LAN access</strong>
              <small>Bind to 0.0.0.0. API keys remain required.</small>
            </span>
            <Toggle checked={allowLan} onChange={setAllowLan} title="Allow LAN access" />
          </div>
          <div className="apiInline">
            <button
              onClick={() => setVirtualTool("claude")}
              disabled={busy || !running || gateway.config.combos.length === 0}
            >
              <Terminal />
              {hasVirtualClaude ? "Update Claude Code" : "Add to Claude Code"}
            </button>
            <button
              onClick={() => setVirtualTool("codex")}
              disabled={busy || !running || gateway.config.combos.length === 0}
            >
              <Terminal />
              {hasVirtualCodex ? "Update Codex" : "Add to Codex"}
            </button>
          </div>
        </section>

        <section className="apiSection">
          <div className="settingsSectionHead">
            <KeyRound />
            <div>
              <strong>API keys</strong>
              <small>Any valid key can call every combo.</small>
            </div>
            <button onClick={() => setShowKeyModal(true)} disabled={busy}>
              <Plus />
              Add key
            </button>
          </div>
          <div className="apiList">
            {gateway.config.keys.length === 0 ? (
              <p className="hint">Create a key before calling the gateway.</p>
            ) : (
              gateway.config.keys.map((key) => (
                <div className="apiListItem" key={key.id}>
                  <div>
                    <strong>{key.name || "Unnamed key"}</strong>
                    <small>
                      {key.prefix}
                      {key.expiresAt ? ` · expires ${new Date(key.expiresAt).toLocaleDateString()}` : ""}
                    </small>
                  </div>
                  <div className="apiItemActions">
                    <button className="iconButton" onClick={() => onCopyKey(key.id)} disabled={busy} title="Copy full key">
                      <Copy />
                    </button>
                    <button className="iconButton danger" onClick={() => onDeleteKey(key.id)} disabled={busy} title="Delete key">
                      <Trash2 />
                    </button>
                  </div>
                </div>
              ))
            )}
          </div>
        </section>

        <section className="apiSection wide">
          <div className="settingsSectionHead">
            <Users />
            <div>
              <strong>Accounts</strong>
              <small>Toggle which subscription accounts the gateway may rotate through.</small>
            </div>
            <label className="apiRotation" title="On = rotate accounts round-robin. Off = fill one until exhausted.">
              <span>Round-robin</span>
              <Toggle
                checked={rotationStrategy === "roundRobin"}
                onChange={(next) => setRotationStrategy(next ? "roundRobin" : "fillFirst")}
              />
            </label>
            <button
              className="iconButton"
              onClick={onRefreshModels}
              disabled={busy}
              title="Refresh subscription models"
            >
              <RefreshCw />
            </button>
          </div>
          {accounts.length === 0 ? (
            <p className="hint">No Claude or Codex subscription accounts yet.</p>
          ) : (
            <div className="apiList">
              {accounts.map(({ tool, account, entry }) => {
                const enabled = entry?.enabled ?? true;
                const state = entry?.state ?? "available";
                return (
                  <div className="apiListItem" key={`${tool.id}-${account.id}`}>
                    <div>
                      <strong>{account.name}</strong>
                      <small>
                        {tool.name}
                        {!enabled && " · Off"}
                        {enabled && state !== "available" && (
                          <>
                            {" · "}
                            <span className={`poolMemberState ${state}`}>{poolStateLabel(state)}</span>
                          </>
                        )}
                      </small>
                    </div>
                    <button
                      className={`toggle ${enabled ? "on" : ""}`}
                      role="switch"
                      aria-checked={enabled}
                      onClick={() =>
                        onSetAccount({ toolId: tool.id, accountId: account.id, enabled: !enabled })
                      }
                      disabled={busy}
                      title={enabled ? "Disable for gateway" : "Enable for gateway"}
                    >
                      <span className="toggleKnob" />
                    </button>
                  </div>
                );
              })}
            </div>
          )}
        </section>

        <section className="apiSection wide">
          <div className="settingsSectionHead">
            <Layers />
            <div>
              <strong>Combos</strong>
              <small>A combo name is the model id clients request; members are tried in order.</small>
            </div>
            <button onClick={openCreateCombo} disabled={busy}>
              <Plus />
              Add Combo
            </button>
          </div>
          <div className="apiList">
            {gateway.config.combos.length === 0 ? (
              <p className="hint">Create a combo, then point your client at its name.</p>
            ) : (
              gateway.config.combos.map((combo: ApiGatewayCombo) => (
                <div className="apiListItem comboItem" key={combo.id}>
                  <div>
                    <strong>{combo.name}</strong>
                    <small>
                      {(combo.strategy ?? rotationStrategy) === "fillFirst"
                        ? "Fill-first"
                        : "Round-robin"}
                      {" · "}
                      {combo.members.length} model(s)
                    </small>
                    <div className="apiModelChips comboMembers">
                      {combo.members.map((model, index) => (
                        <span className="apiModelChip" key={model}>
                          <span className="comboMemberOrder">{index + 1}</span>
                          {model}
                        </span>
                      ))}
                    </div>
                  </div>
                  <div className="apiItemActions">
                    <button className="iconButton" onClick={() => onCopy(combo.name)} title="Copy combo name">
                      <Copy />
                    </button>
                    <button className="iconButton" onClick={() => openEditCombo(combo)} disabled={busy} title="Edit combo">
                      <Pencil />
                    </button>
                    <button className="iconButton danger" onClick={() => onDeleteCombo(combo.id)} disabled={busy} title="Delete combo">
                      <Trash2 />
                    </button>
                  </div>
                </div>
              ))
            )}
          </div>
        </section>

        <section className="apiSection wide">
          <button className="apiCollapseHead" onClick={() => setShowModels((value) => !value)}>
            <div className="settingsSectionHead">
              <Boxes />
              <div>
                <strong>Available models</strong>
                <small>
                  {modelsByProvider.reduce((sum, group) => sum + group.models.length, 0)} model(s)
                  from your enabled accounts
                </small>
              </div>
            </div>
            <ChevronDown className={showModels ? "rotated" : ""} />
          </button>
          {showModels &&
            (modelsByProvider.length === 0 ? (
              <p className="hint">No models yet — refresh while accounts are signed in.</p>
            ) : (
              modelsByProvider.map((group) => (
                <div className="apiModelGroup" key={group.tool}>
                  <small className="apiModelGroupLabel">
                    {group.label} ({group.models.length})
                  </small>
                  <div className="apiModelChips">
                    {group.models.map((model) => (
                      <span className="apiModelChip" key={model}>
                        {model}
                      </span>
                    ))}
                  </div>
                </div>
              ))
            ))}
        </section>

        <section className="apiSection wide">
          <div className="settingsSectionHead">
            <BarChart3 />
            <div>
              <strong>API usage</strong>
              <small>{usage ? `${usage.totalRequests} request(s)` : "No proxy usage yet"}</small>
            </div>
          </div>
          <div className="apiUsageStats">
            <span>Input {formatCount(usage?.total.input ?? 0)}</span>
            <span>Output {formatCount(usage?.total.output ?? 0)}</span>
            <span>Cache {formatCount((usage?.total.cacheRead ?? 0) + (usage?.total.cacheCreation ?? 0))}</span>
          </div>
          <div className="apiList">
            {usage?.rows.map((row) => (
              <div className="apiListItem" key={`${row.comboName}-${row.keyId}-${row.accountId}`}>
                <div>
                  <strong>{row.comboName}</strong>
                  <small>
                    {row.toolId} · {row.requests} request(s) · {formatCount(row.tokens.input + row.tokens.output)} tokens
                  </small>
                </div>
              </div>
            ))}
          </div>
        </section>
      </div>

      {showKeyModal && (
        <AddKeyModal
          busy={busy}
          onClose={() => setShowKeyModal(false)}
          onCreate={onCreateKey}
          onCopy={onCopy}
        />
      )}

      {showComboModal && (
        <ComboModal
          busy={busy}
          combo={comboEditing}
          defaultStrategy={rotationStrategy}
          modelsByProvider={modelsByProvider}
          existingCombos={gateway.config.combos}
          onClose={() => setShowComboModal(false)}
          onSave={async (input) => {
            const ok = await onSaveCombo(input);
            if (ok) setShowComboModal(false);
          }}
        />
      )}

      {virtualTool && (
        <VirtualAccountModal
          busy={busy}
          tool={virtualTool}
          combos={gateway.config.combos.filter((combo) => combo.enabled)}
          modelsByProvider={modelsByProvider}
          currentModel={
            snapshot.tools
              .find((t) => t.id === virtualTool)
              ?.accounts.find((a) => a.fingerprint === "api-local")?.apiProvider?.model ?? null
          }
          onClose={() => setVirtualTool(null)}
          onConfirm={async (model) => {
            const ok = await onCreateVirtual(virtualTool, model);
            if (ok) setVirtualTool(null);
          }}
        />
      )}
    </section>
  );
}

function VirtualAccountModal({
  busy,
  tool,
  combos,
  modelsByProvider,
  currentModel,
  onClose,
  onConfirm,
}: {
  busy: boolean;
  tool: ToolId;
  combos: ApiGatewayCombo[];
  modelsByProvider: { tool: ToolId; label: string; models: string[] }[];
  currentModel: string | null;
  onClose: () => void;
  onConfirm: (model: string) => Promise<void>;
}) {
  const toolName = tool === "claude" ? "Claude Code" : "Codex";
  const [model, setModel] = useState(currentModel ?? combos[0]?.name ?? "");
  return (
    <div className="modalBackdrop" role="presentation" onMouseDown={onClose}>
      <section className="modal" onMouseDown={(event) => event.stopPropagation()}>
        <h2>{currentModel ? `Update ${toolName}` : `Add to ${toolName}`}</h2>
        <p className="hint">
          A <code>{tool === "claude" ? "claude-api" : "codex-api"}</code> account is added to{" "}
          {toolName} pointing at the local gateway. Choose the model it requests — a combo (with
          fallback) or any single model the gateway can serve.
        </p>
        <label>
          Model
          <input
            value={model}
            onChange={(event) => setModel(event.target.value)}
            placeholder="combo name or model id"
          />
        </label>
        <div className="comboPicker">
          {combos.length > 0 && (
            <div className="apiModelGroup">
              <small className="apiModelGroupLabel">Combos</small>
              <div className="apiModelChips">
                {combos.map((combo) => (
                  <button
                    key={combo.id}
                    className={`apiModelChip pickable ${model === combo.name ? "picked" : ""}`}
                    onClick={() => setModel(combo.name)}
                  >
                    {combo.name}
                  </button>
                ))}
              </div>
            </div>
          )}
          {modelsByProvider.map((group) => (
            <div className="apiModelGroup" key={group.tool}>
              <small className="apiModelGroupLabel">
                {group.label} ({group.models.length})
              </small>
              <div className="apiModelChips">
                {group.models.map((name) => (
                  <button
                    key={name}
                    className={`apiModelChip pickable ${model === name ? "picked" : ""}`}
                    onClick={() => setModel(name)}
                  >
                    {name}
                  </button>
                ))}
              </div>
            </div>
          ))}
        </div>
        <div className="modalActions">
          <button onClick={onClose}>Cancel</button>
          <button
            className="primary"
            onClick={() => model.trim() && onConfirm(model.trim())}
            disabled={busy || !model.trim()}
          >
            <Terminal />
            {currentModel ? "Update" : "Add"}
          </button>
        </div>
      </section>
    </div>
  );
}

function ComboModal({
  busy,
  combo,
  defaultStrategy,
  modelsByProvider,
  existingCombos,
  onClose,
  onSave,
}: {
  busy: boolean;
  combo: ApiGatewayCombo | null;
  defaultStrategy: ApiRotationStrategy;
  modelsByProvider: { tool: ToolId; label: string; models: string[] }[];
  existingCombos: ApiGatewayCombo[];
  onClose: () => void;
  onSave: (input: SaveApiGatewayComboInput) => Promise<void>;
}) {
  const [name, setName] = useState(combo?.name ?? "");
  const [members, setMembers] = useState<string[]>(combo?.members ?? []);
  const [roundRobin, setRoundRobin] = useState(
    (combo?.strategy ?? defaultStrategy) === "roundRobin",
  );
  const [picking, setPicking] = useState(false);

  const move = (index: number, delta: number) => {
    const next = [...members];
    const target = index + delta;
    if (target < 0 || target >= next.length) return;
    [next[index], next[target]] = [next[target], next[index]];
    setMembers(next);
  };
  const toggleModel = (model: string) => {
    setMembers((current) =>
      current.includes(model) ? current.filter((item) => item !== model) : [...current, model],
    );
  };
  const submit = () =>
    onSave({
      id: combo?.id ?? null,
      name: name.trim(),
      members,
      strategy: roundRobin ? "roundRobin" : "fillFirst",
    });

  return (
    <div className="modalBackdrop" role="presentation" onMouseDown={onClose}>
      <section className="modal" onMouseDown={(event) => event.stopPropagation()}>
        <h2>{combo ? "Edit Combo" : "Create Combo"}</h2>
        <label>
          Combo name
          <input
            autoFocus
            value={name}
            onChange={(event) => setName(event.target.value)}
            placeholder="e.g. claude-sonnet-4-6"
          />
          <small>Only letters, numbers, -, _ and . allowed. Clients request this name.</small>
        </label>

        <div className="apiToggleRow">
          <span>
            Round-robin
            <small>Rotate members each request. Off = fallback (try members top to bottom).</small>
          </span>
          <Toggle checked={roundRobin} onChange={setRoundRobin} title="Round-robin" />
        </div>

        <div className="comboModels">
          <span className="labelRow">Models</span>
          {members.length === 0 ? (
            <p className="comboEmpty">
              <Layers />
              No models added yet
            </p>
          ) : (
            members.map((model, index) => (
              <div className="comboMemberRow" key={model}>
                <span className="comboMemberIndex">{index + 1}</span>
                <span className="comboMemberName">{model}</span>
                <button
                  className="iconButton"
                  onClick={() => move(index, -1)}
                  disabled={index === 0}
                  title="Move up"
                >
                  <ArrowUp />
                </button>
                <button
                  className="iconButton"
                  onClick={() => move(index, 1)}
                  disabled={index === members.length - 1}
                  title="Move down"
                >
                  <ArrowDown />
                </button>
                <button className="iconButton danger" onClick={() => toggleModel(model)} title="Remove">
                  <X />
                </button>
              </div>
            ))
          )}
          <button className="comboAddModel" onClick={() => setPicking((value) => !value)}>
            <Plus />
            Add Model
          </button>
          {picking && (
            <div className="comboPicker">
              {existingCombos.filter((item) => item.id !== combo?.id).length > 0 && (
                <div className="apiModelGroup">
                  <small className="apiModelGroupLabel">Combos</small>
                  <div className="apiModelChips">
                    {existingCombos
                      .filter((item) => item.id !== combo?.id)
                      .map((item) => (
                        <button
                          key={item.id}
                          className={`apiModelChip pickable ${members.includes(item.name) ? "picked" : ""}`}
                          onClick={() => toggleModel(item.name)}
                        >
                          {item.name}
                        </button>
                      ))}
                  </div>
                </div>
              )}
              {modelsByProvider.map((group) => (
                <div className="apiModelGroup" key={group.tool}>
                  <small className="apiModelGroupLabel">
                    {group.label} ({group.models.length})
                  </small>
                  <div className="apiModelChips">
                    {group.models.map((model) => (
                      <button
                        key={model}
                        className={`apiModelChip pickable ${members.includes(model) ? "picked" : ""}`}
                        onClick={() => toggleModel(model)}
                      >
                        {model}
                      </button>
                    ))}
                  </div>
                </div>
              ))}
            </div>
          )}
        </div>

        <div className="modalActions">
          <button onClick={onClose}>Cancel</button>
          <button
            className="primary"
            onClick={submit}
            disabled={busy || !name.trim() || members.length === 0}
          >
            {combo ? "Save" : "Create"}
          </button>
        </div>
      </section>
    </div>
  );
}

function AddKeyModal({
  busy,
  onClose,
  onCreate,
  onCopy,
}: {
  busy: boolean;
  onClose: () => void;
  onCreate: (input: CreateApiGatewayKeyInput) => Promise<string | null>;
  onCopy: (text: string) => void;
}) {
  const [name, setName] = useState("");
  const [expiry, setExpiry] = useState("");
  const [secret, setSecret] = useState<string | null>(null);
  const submit = async () => {
    const created = await onCreate({
      name: name.trim() || "Untitled key",
      expiresAt: expiry ? new Date(`${expiry}T23:59:59`).toISOString() : null,
    });
    if (created) setSecret(created);
  };
  return (
    <div className="modalBackdrop" role="presentation" onMouseDown={onClose}>
      <section className="modal" onMouseDown={(event) => event.stopPropagation()}>
        <h2>Add API key</h2>
        {secret ? (
          // Reveal view: the secret is shown exactly once. Copy works even if auto-copy was denied.
          <>
            <label>
              Your new API key
              <div className="secretReveal">
                <code>{secret}</code>
                <button className="iconButton" onClick={() => onCopy(secret)} title="Copy key">
                  <Copy />
                </button>
              </div>
              <small>Copy it now — it will not be shown again.</small>
            </label>
            <div className="modalActions">
              <button className="primary" onClick={onClose}>
                Done
              </button>
            </div>
          </>
        ) : (
          <>
            <label>
              Key name
              <input
                autoFocus
                value={name}
                onChange={(event) => setName(event.target.value)}
                placeholder="e.g. Cline laptop"
              />
            </label>
            <label>
              <span className="labelRow">
                Expires <small>(optional)</small>
              </span>
              <input type="date" value={expiry} onChange={(event) => setExpiry(event.target.value)} />
            </label>
            <p className="hint">The full key is shown once when created.</p>
            <div className="modalActions">
              <button onClick={onClose}>Cancel</button>
              <button className="primary" onClick={submit} disabled={busy}>
                <Plus />
                Create key
              </button>
            </div>
          </>
        )}
      </section>
    </div>
  );
}

let toastSeq = 0;
function nextToastId() {
  toastSeq += 1;
  return toastSeq;
}

/// Copy text to the clipboard. Prefers Tauri's clipboard plugin (writes via the OS, so it works
/// even though the webview denies the JS Clipboard API), then falls back to navigator/execCommand
/// for the browser demo. Never throws — returns whether it worked.
async function copyToClipboard(text: string): Promise<boolean> {
  try {
    const { writeText } = await import("@tauri-apps/plugin-clipboard-manager");
    await writeText(text);
    return true;
  } catch {
    // Not in Tauri (browser demo) or plugin unavailable — try the web APIs.
  }
  try {
    if (navigator.clipboard?.writeText) {
      await navigator.clipboard.writeText(text);
      return true;
    }
  } catch {
    // fall through to the legacy path
  }
  try {
    const area = document.createElement("textarea");
    area.value = text;
    area.style.position = "fixed";
    area.style.opacity = "0";
    document.body.appendChild(area);
    area.focus();
    area.select();
    const ok = document.execCommand("copy");
    document.body.removeChild(area);
    return ok;
  } catch {
    return false;
  }
}

// The live per-account model registry is authoritative — it lists exactly the models each
// subscription account can actually serve (e.g. a ChatGPT/Codex account can't use `gpt-5`, only
// `gpt-5.4`/`gpt-5.5`). We deliberately keep no hardcoded fallback so the picker never offers a
// model the account would reject; the registry refreshes on Start and when the combo modal opens.
const FALLBACK_MODELS: Record<ToolId, string[]> = {
  claude: [],
  codex: [],
  antigravity: [],
};

function poolStateLabel(state: ApiPoolAccountState) {
  if (state === "coolingDown") return "Cooling down";
  if (state === "exhausted") return "Exhausted";
  if (state === "errored") return "Needs attention";
  if (state === "excluded") return "Excluded";
  return "Available";
}

function formatCount(value: number) {
  return new Intl.NumberFormat("en", { notation: value >= 100_000 ? "compact" : "standard" }).format(value);
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
  autoPrime,
  primeAttempt,
  onExtend,
  onPrimeNow,
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
  autoPrime: AutoPrimeSetting | null;
  primeAttempt: PrimeAttemptStatus | null;
  onExtend: (accept: boolean) => void;
  onPrimeNow: () => void;
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
  const isVirtualApi = account.fingerprint === "api-local";
  const isActive = account.id === tool.activeAccountId || account.state === "active";
  const exhausted = account.state === "exhausted";
  const needsLogin = account.state === "needs-login";

  // Auto session prime status shown on the card (subscription Claude/Codex only).
  const canPrime = (tool.id === "claude" || tool.id === "codex") && !isApi;
  const primeOn = !!autoPrime?.enabled;
  const resetAt = account.quota?.fiveHour.resetAt ?? null;
  const minsToReset = resetAt ? Math.round((Date.parse(resetAt) - Date.now()) / 60000) : null;
  // Offer "extend" when the window is about to end (still has time left, ≤30'), the reminder is for
  // THIS exact window (reset_at match — so it never lingers onto the next window), the user hasn't
  // already accepted, and hasn't dismissed it. `minsToReset > 0`: once the window hits 0/expired we
  // stop offering — that window is gone; a fresh one earns its own reminder from the poller.
  const showExtend =
    canPrime &&
    !!autoPrime?.extendRemindedReset &&
    autoPrime.extendRemindedReset === resetAt &&
    !autoPrime.extendRequested &&
    autoPrime.extendDismissedReset !== resetAt &&
    minsToReset !== null &&
    minsToReset > 0 &&
    minsToReset <= 30;
  const autoStatus = (() => {
    if (!canPrime) return null;
    if (primeAttempt) {
      const deadline = new Date(primeAttempt.deadlineAt).toLocaleTimeString([], {
        hour: "2-digit",
        minute: "2-digit",
        hour12: false,
      });
      return `${primeAttemptSourceLabel(primeAttempt.source)} · đang mở session · thử tới ${deadline}`;
    }
    if (!primeOn) return null;
    if (autoPrime?.extendRequested) return "Sẽ mở phiên mới khi phiên cũ hết";
    if (autoPrime?.lastResult === "success") return `Auto ${autoPrime.time} · đã prime`;
    return `Auto ${autoPrime?.time}`;
  })();
  // "Prime ngay": let the user open a new 5h window on demand, for the case where there's no live
  // window and they don't want to drop to a terminal to send a message. The backend decides this
  // (provider-aware `primeAvailable`) — the UI must NOT recompute from `resetAt`: a Codex reset_at
  // can be in the future yet rolling/unanchored (no real window), and a Claude `resetAt === null`
  // can mean "fully ended" (offer) rather than "unknown" (hide). `primeAvailable === true` means
  // ended-or-unanchored; undefined means unknown/read-error → hide. Still hidden when login is
  // needed or an extend is already armed (that opens the window itself). The backend's D2 stays
  // the real guard; this just shows the button at the right time.
  const showPrimeNow =
    canPrime &&
    account.quota?.primeAvailable === true &&
    !needsLogin &&
    !autoPrime?.extendRequested &&
    !primeAttempt;
  const primingNow = busy === `prime:${account.id}`;

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
          {isApi && <span className="badge api">{isVirtualApi ? "Local API" : "API"}</span>}
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

      {showExtend ? (
        // One quiet inline line, not a banner with two buttons: "Phiên còn 20' · Gia hạn". Clicking
        // "Gia hạn" arms the extend; doing nothing just lets the window end (no dismiss button — not
        // acting IS the decline). Keeps the card calm even when several accounts end at once.
        <div className="autoStatusLine extendLine">
          <AlarmClock size={13} /> Phiên còn {minsToReset}′ ·{" "}
          <button className="linkBtn" onClick={() => onExtend(true)} disabled={busy !== null}>
            Gia hạn
          </button>
        </div>
      ) : (
        autoStatus && (
          <div className="autoStatusLine">
            <AlarmClock size={13} /> {autoStatus}
          </div>
        )
      )}

      {showPrimeNow && (
        <button
          className="primeNowBtn"
          onClick={onPrimeNow}
          disabled={busy !== null}
          title="Gửi một tin nhắn tối thiểu để mở phiên 5 giờ mới ngay bây giờ"
        >
          {primingNow ? <Loader2 className="spin" size={14} /> : <AlarmClock size={14} />} Prime ngay
        </button>
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
        {tool.id !== "antigravity" && !account.isDefault && !isVirtualApi && (
          <button
            className="iconButton"
            onClick={onSetLauncher}
            disabled={needsLogin || busy !== null}
            title="Custom command"
          >
            <Terminal />
          </button>
        )}
        {!account.isDefault && !isVirtualApi && (
          <button
            className="iconButton"
            onClick={onRename}
            disabled={needsLogin || busy !== null}
            title="Rename"
          >
            <Pencil />
          </button>
        )}
        {!account.isDefault && !isVirtualApi && (
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

                <div className="apiToggleRow">
                  <span>
                    Bypass approvals &amp; sandbox in the custom command
                    <small>Adds {bypassFlag}. Off by default.</small>
                  </span>
                  <Toggle checked={bypass} onChange={setBypass} title="Bypass approvals & sandbox" />
                </div>
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
