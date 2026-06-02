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
  ShieldAlert,
  Terminal,
  Trash2,
  X,
  Zap,
} from "lucide-react";
import { useCallback, useEffect, useMemo, useState } from "react";
import { listen } from "@tauri-apps/api/event";
import { api } from "./tauri";
import { UsageView } from "./UsageView";
import type { Account, AddAccountInput, AppSnapshot, ToolId, ToolStatus } from "./types";

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
};

export function App() {
  const [snapshot, setSnapshot] = useState<AppSnapshot>(emptySnapshot);
  const [selectedTool, setSelectedTool] = useState<ToolId>("claude");
  const [view, setView] = useState<"accounts" | "usage">("accounts");
  const [toast, setToast] = useState<string | null>(null);
  const [busy, setBusy] = useState<string | null>(null);
  const [dialog, setDialog] = useState<"add" | "rename" | "launcher" | null>(null);
  const [selectedAccount, setSelectedAccount] = useState<Account | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [switchNotice, setSwitchNotice] = useState<string | null>(null);
  const [autoSwitchBanner, setAutoSwitchBanner] = useState<string | null>(null);

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

  const currentTool = useMemo(
    () => snapshot.tools.find((tool) => tool.id === selectedTool) ?? snapshot.tools[0],
    [selectedTool, snapshot.tools],
  );

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

  const refresh = () => run("refresh", () => api.refreshTool(selectedTool));

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
      <header className="topbar" data-tauri-drag-region>
        <div>
          <h1>AI Account Switcher</h1>
        </div>
        <button className="iconButton" onClick={refresh} disabled={busy !== null} title="Refresh quota for the selected tool">
          {busy === "refresh" || busy === "load" ? <Loader2 className="spin" /> : <RefreshCw />}
        </button>
      </header>

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
        </aside>

        {view === "usage" && <UsageView />}

        {view === "accounts" && currentTool && (
          <section className="panel">
            <div className="panelHead">
              <div>
                <div className="titleRow">
                  <h2>{currentTool.name}</h2>
                  <span className={`status ${currentTool.installed ? "ok" : "muted"}`}>
                    {currentTool.installed ? "Ready" : "Tool not installed"}
                  </span>
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
                <button onClick={refresh} disabled={busy !== null}>
                  <RefreshCw />
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

            {currentTool.id !== "antigravity" && (
              <AutoSwitchBar
                enabled={snapshot.autoSwitch}
                threshold={snapshot.autoSwitchThreshold}
                busy={busy !== null}
                onChange={(enabled, threshold) =>
                  run("autoSwitch", () => api.setAutoSwitch(enabled, threshold))
                }
              />
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
    </main>
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
}: {
  account: Account;
  tool: ToolStatus;
  busy: string | null;
  onSwitch: () => void;
  onRename: () => void;
  onSetLauncher: () => void;
  onCopy: (text: string) => void;
  onDelete: () => void;
}) {
  const isAntigravity = tool.id === "antigravity";
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
          <div>
            <div className="accountName">
              {isActive && <span className="dot" aria-hidden />}
              <h3>{account.name}</h3>
            </div>
            {!isAntigravity && (
              <span
                className="fingerprint"
                title={account.isDefault ? undefined : account.fingerprint}
              >
                {account.isDefault ? "Machine default" : shortFingerprint(account.fingerprint)}
              </span>
            )}
          </div>
        </div>
        <span className={`badge ${badgeClass(account.state)}`}>
          {exhausted ? "Out of quota" : isActive ? "In use" : stateLabel(account.state)}
        </span>
      </div>

      <Quota quota={account.quota} />

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

      {tool.id !== "antigravity" && !account.isDefault && !needsLogin && (
        <div className="launcherRow">
          <Terminal />
          {account.launcherCommand ? (
            <button
              className="launcherChip"
              onClick={() => onCopy(account.launcherCommand!)}
              title="Copy the command to run this account in any terminal"
            >
              <code>{account.launcherCommand}</code>
              <Copy />
            </button>
          ) : (
            <button className="launcherChip muted" onClick={onSetLauncher}>
              Set custom command
            </button>
          )}
        </div>
      )}

      <div className="cardActions">
        <button onClick={onSwitch} disabled={isActive || needsLogin || busy !== null}>
          <RotateCcw /> Use
        </button>
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
  enabled,
  threshold,
  busy,
  onChange,
}: {
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
          <strong>Auto-switch account when quota runs out</strong>
          <small>
            When the active account (bare command) is nearly out → auto-switch to the account with
            the most quota left. Custom <code>claude-…</code> commands are unaffected. Applies in new terminals.
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
        <span>{label}</span>
        <strong>{percent === null ? "?" : `${Math.round(value)}%`}</strong>
      </div>
      <div className="bar" data-level={level}>
        <span style={{ width: `${value}%` }} />
      </div>
      <small>{resetAt ? `Resets at ${formatTime(resetAt)}` : "Reset time unknown"}</small>
    </div>
  );
}

function AddDialog({
  tool,
  onClose,
  onSubmit,
}: {
  tool: ToolStatus;
  onClose: () => void;
  onSubmit: (input: AddAccountInput) => Promise<void>;
}) {
  const isCli = tool.id !== "antigravity";
  const [name, setName] = useState("");
  const [launcher, setLauncher] = useState("");
  const [message, setMessage] = useState<string | null>(null);

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

  return (
    <div className="modalBackdrop" role="presentation" onMouseDown={onClose}>
      <section className="modal" onMouseDown={(event) => event.stopPropagation()}>
        <h2>{isCli ? `Add ${tool.name} account` : "Save the signed-in Antigravity IDE account"}</h2>
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
        {isCli && (
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
          <button className="primary" onClick={submit}>
            {isCli ? "Create & login" : "Save this account"}
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

function shortFingerprint(fingerprint: string) {
  // Short fingerprints (e.g. Antigravity "fp:ab12cd…") are left as-is; only long ones
  // like "profile:f8455f1c-77f6-40ee-..." are shortened to "profile:f8455f1c…" (full value in tooltip).
  if (fingerprint.length <= 20) return fingerprint;
  const match = fingerprint.match(/^([^:]+):([0-9a-f]{8})/i);
  if (match) return `${match[1]}:${match[2]}…`;
  return `${fingerprint.slice(0, 18)}…`;
}

function formatTime(value: string) {
  return new Intl.DateTimeFormat("en-US", {
    dateStyle: "short",
    timeStyle: "short",
  }).format(new Date(value));
}

function errorMessage(err: unknown) {
  return err instanceof Error ? err.message : String(err);
}
