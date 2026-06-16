import { useEffect, useMemo, useState } from "react";
import { AlarmClock, FileText, FolderOpen, Loader2, Moon } from "lucide-react";
import { api } from "./tauri";
import type { Account, AppSnapshot, ToolId } from "./types";

const PRIME_TOOLS: ToolId[] = ["claude", "codex"];

/** A subscription (OAuth) Claude/Codex account is prime-eligible; API-proxy accounts are not. */
function isPrimeEligible(account: Account): boolean {
  return PRIME_TOOLS.includes(account.toolId) && !account.apiProvider;
}

function resultLabel(result?: string | null): string {
  switch (result) {
    case "success":
      return "Lần gần nhất: thành công";
    case "failed":
      return "Lần gần nhất: thất bại";
    case "skip":
      return "Lần gần nhất: bỏ qua (token)";
    case "hold":
      return "Lần gần nhất: hoãn";
    default:
      return "Chưa prime lần nào";
  }
}

export function AutoSessionView({
  snapshot,
  setSnapshot,
  notify,
}: {
  snapshot: AppSnapshot;
  setSnapshot: (next: AppSnapshot) => void;
  notify: (text: string, kind?: "success" | "error") => void;
}) {
  const [busyId, setBusyId] = useState<string | null>(null);
  const [allTime, setAllTime] = useState("05:30");
  const [log, setLog] = useState<string | null>(null);
  const [loadingLog, setLoadingLog] = useState(false);
  const [wakeHelper, setWakeHelper] = useState<boolean | null>(null);
  const [wakeBusy, setWakeBusy] = useState(false);

  useEffect(() => {
    void api.wakeHelperStatus().then(setWakeHelper).catch(() => setWakeHelper(false));
  }, []);

  async function toggleWakeHelper() {
    setWakeBusy(true);
    try {
      const installed = wakeHelper
        ? await api.uninstallWakeHelper()
        : await api.installWakeHelper();
      setWakeHelper(installed);
      notify(
        installed
          ? "Đã cài trợ giúp đánh thức máy — Mac sẽ tự thức để prime đúng giờ"
          : "Đã gỡ trợ giúp đánh thức máy",
      );
    } catch (e) {
      notify(String(e), "error");
    } finally {
      setWakeBusy(false);
    }
  }

  // Flatten every prime-eligible account across Claude + Codex, in tool order.
  const accounts = useMemo(() => {
    const rows: { tool: string; account: Account }[] = [];
    for (const tool of snapshot.tools) {
      if (!PRIME_TOOLS.includes(tool.id)) continue;
      for (const account of tool.accounts) {
        if (isPrimeEligible(account)) rows.push({ tool: tool.name, account });
      }
    }
    return rows;
  }, [snapshot]);

  // Local draft of the time input per account (so typing doesn't fight the snapshot).
  const [drafts, setDrafts] = useState<Record<string, string>>({});
  const timeOf = (account: Account) =>
    drafts[account.id] ?? snapshot.autoPrime[account.id]?.time ?? "05:30";

  async function save(account: Account, enabled: boolean) {
    const time = timeOf(account);
    if (!/^\d{1,2}:\d{2}$/.test(time)) {
      notify("Giờ không hợp lệ (cần dạng HH:MM 24h)", "error");
      return;
    }
    setBusyId(account.id);
    try {
      const next = await api.setAutoPrime({
        toolId: account.toolId,
        accountId: account.id,
        enabled,
        time,
      });
      setSnapshot(next);
      notify(enabled ? `Đã bật auto prime ${time} cho "${account.name}"` : `Đã tắt auto prime cho "${account.name}"`);
    } catch (e) {
      notify(String(e), "error");
    } finally {
      setBusyId(null);
    }
  }

  async function applyAll() {
    if (!/^\d{1,2}:\d{2}$/.test(allTime)) {
      notify("Giờ không hợp lệ (cần dạng HH:MM 24h)", "error");
      return;
    }
    setBusyId("__all__");
    try {
      const next = await api.setAutoPrimeAll({ time: allTime, enabled: true });
      setSnapshot(next);
      setDrafts({});
      notify(`Đã áp dụng ${allTime} cho tất cả tài khoản`);
    } catch (e) {
      notify(String(e), "error");
    } finally {
      setBusyId(null);
    }
  }

  async function viewLog() {
    setLoadingLog(true);
    try {
      setLog(await api.getAutoPrimeLog());
    } catch (e) {
      notify(String(e), "error");
    } finally {
      setLoadingLog(false);
    }
  }

  return (
    <section className="panel autoSession">
      <header className="panelHead">
        <div>
          <h2>
            <AlarmClock size={18} /> Auto Session
          </h2>
          <p className="muted">
            Tự gửi 1 tin nhắn mồi đúng giờ để neo mốc reset 5h theo nhịp làm việc. Mỗi tài khoản 1
            giờ, prime tối đa 1 lần/ngày.
          </p>
        </div>
      </header>

      <div className="wakeRow">
        <div className="wakeText">
          <strong>
            <Moon size={15} /> Đánh thức máy để prime (pmset)
          </strong>
          <span className="muted">
            {wakeHelper === null
              ? "Đang kiểm tra…"
              : wakeHelper
                ? "Đã bật — Mac sẽ tự thức ~5 phút trước giờ prime rồi ngủ lại."
                : "Chưa bật — hiện chỉ prime khi máy đang thức / app đang mở. Bật để Mac tự thức (cần quyền admin 1 lần)."}
          </span>
        </div>
        <button onClick={() => void toggleWakeHelper()} disabled={wakeBusy || wakeHelper === null}>
          {wakeBusy ? <Loader2 className="spin" size={15} /> : null}
          {wakeHelper ? "Tắt" : "Bật"}
        </button>
      </div>

      <div className="autoAllRow">
        <span>Áp 1 giờ cho tất cả:</span>
        <input
          type="time"
          value={allTime}
          onChange={(e) => setAllTime(e.target.value)}
          aria-label="Giờ áp cho tất cả"
        />
        <button className="primary" onClick={() => void applyAll()} disabled={busyId !== null}>
          {busyId === "__all__" ? <Loader2 className="spin" size={15} /> : null} Apply all
        </button>
      </div>

      {accounts.length === 0 ? (
        <p className="muted">
          Chưa có tài khoản nào hỗ trợ auto prime. Cần tài khoản Claude / Codex đăng nhập
          subscription (không phải API key).
        </p>
      ) : (
        <div className="autoGrid">
          {accounts.map(({ tool, account }) => {
            const setting = snapshot.autoPrime[account.id];
            const enabled = setting?.enabled ?? false;
            const busy = busyId === account.id;
            return (
              <div key={account.id} className={`autoCard ${enabled ? "on" : ""}`}>
                <div className="autoCardHead">
                  <strong>{account.name}</strong>
                  <span className="autoTool">{tool}</span>
                </div>
                <div className="autoCardBody">
                  <input
                    type="time"
                    value={timeOf(account)}
                    onChange={(e) =>
                      setDrafts((d) => ({ ...d, [account.id]: e.target.value }))
                    }
                    aria-label={`Giờ prime cho ${account.name}`}
                  />
                  <button className="primary" onClick={() => void save(account, true)} disabled={busy}>
                    {busy ? <Loader2 className="spin" size={15} /> : null} Set
                  </button>
                  <button
                    onClick={() => void save(account, !enabled)}
                    disabled={busy}
                    title={enabled ? "Tắt auto prime" : "Bật auto prime"}
                  >
                    {enabled ? "Tắt" : "Bật"}
                  </button>
                </div>
                <div className="autoCardFoot muted">
                  <span className={`autoBadge ${enabled ? "on" : "off"}`}>
                    {enabled ? `Bật · ${setting?.time}` : "Tắt"}
                  </span>
                  <span>{resultLabel(setting?.lastResult)}</span>
                </div>
              </div>
            );
          })}
        </div>
      )}

      <div className="autoLogBar">
        <button onClick={() => void viewLog()} disabled={loadingLog}>
          {loadingLog ? <Loader2 className="spin" size={15} /> : <FileText size={15} />} Xem log
        </button>
        <button onClick={() => void api.openAutoPrimeLog().catch((e) => notify(String(e), "error"))}>
          <FileText size={15} /> Mở log
        </button>
        <button
          onClick={() => void api.openAutoPrimeLogFolder().catch((e) => notify(String(e), "error"))}
        >
          <FolderOpen size={15} /> Mở thư mục log
        </button>
      </div>

      {log !== null && (
        <pre className="autoLog">{log.trim() ? log : "Chưa có hoạt động nào được ghi."}</pre>
      )}
    </section>
  );
}
