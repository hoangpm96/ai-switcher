import { useEffect, useMemo, useState } from "react";
import { AlarmClock, BarChart3, FileText, FolderOpen, Loader2, Moon } from "lucide-react";
import { api } from "./tauri";
import type { Account, AppSnapshot, AutoPrimeDayStat, ToolId } from "./types";

const PRIME_TOOLS: ToolId[] = ["claude", "codex"];

/** A subscription (OAuth) Claude/Codex account is prime-eligible; API-proxy accounts are not. */
function isPrimeEligible(account: Account): boolean {
  return PRIME_TOOLS.includes(account.toolId) && !account.apiProvider;
}

/** "HH:MM" + 5 hours (the 5h window length), wrapping past midnight. Empty if input is invalid. */
function plusFiveHours(hhmm: string): string {
  const m = /^(\d{1,2}):(\d{2})$/.exec(hhmm.trim());
  if (!m) return "";
  const total = (Number(m[1]) * 60 + Number(m[2]) + 5 * 60) % (24 * 60);
  return `${String(Math.floor(total / 60)).padStart(2, "0")}:${String(total % 60).padStart(2, "0")}`;
}

function resultLabel(result?: string | null): string {
  switch (result) {
    case "success":
      return "✓ đã prime";
    case "failed":
      return "✗ lỗi";
    case "skip":
      return "bỏ qua (token)";
    case "hold":
      return "đã hoãn";
    case "retrying":
      return "đang thử lại";
    default:
      return "chưa prime";
  }
}

function localHHMM(iso: string): string {
  const parsed = new Date(iso);
  return Number.isNaN(parsed.getTime())
    ? iso
    : parsed.toLocaleTimeString([], { hour: "2-digit", minute: "2-digit", hour12: false });
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
  const [primeAsleep, setPrimeAsleep] = useState<boolean | null>(null);
  const [primeAsleepBusy, setPrimeAsleepBusy] = useState(false);
  const [stats, setStats] = useState<AutoPrimeDayStat[] | null>(null);
  const [loadingStats, setLoadingStats] = useState(false);

  useEffect(() => {
    void api.wakeHelperStatus().then(setWakeHelper).catch(() => setWakeHelper(false));
    void api.primeDaemonStatus().then(setPrimeAsleep).catch(() => setPrimeAsleep(false));
  }, []);

  async function viewStats() {
    setLoadingStats(true);
    try {
      setStats(await api.getAutoPrimeStats());
    } catch (e) {
      notify(String(e), "error");
    } finally {
      setLoadingStats(false);
    }
  }

  async function toggleAutoExtend(account: Account, enabled: boolean) {
    try {
      const next = await api.setAutoExtend({
        toolId: account.toolId,
        accountId: account.id,
        enabled,
      });
      setSnapshot(next);
    } catch (e) {
      notify(String(e), "error");
    }
  }

  async function toggleWakeHelper() {
    setWakeBusy(true);
    try {
      const installed = wakeHelper
        ? await api.uninstallWakeHelper()
        : await api.installWakeHelper();
      setWakeHelper(installed);
      if (!installed) setPrimeAsleep(false);
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

  async function togglePrimeAsleep() {
    setPrimeAsleepBusy(true);
    try {
      const installed = await api.setPrimeWhileAsleep(!primeAsleep);
      setPrimeAsleep(installed);
      notify(
        installed
          ? "Đã bật tự prime khi máy ngủ — Mac sẽ tự thức và gửi yêu cầu prime đúng giờ; nếu Keychain còn khóa thì sẽ thử lại sau khi mở khóa"
          : "Đã tắt tự prime khi máy ngủ",
      );
    } catch (e) {
      notify(String(e), "error");
    } finally {
      setPrimeAsleepBusy(false);
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
      <header className="panelHead autoHead">
        <h2>
          <AlarmClock size={18} /> Auto Session
        </h2>
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
                ? "Mac tự thức ~5 phút trước giờ prime rồi ngủ lại."
                : "Bật để Mac tự thức đúng giờ (cần quyền admin 1 lần)."}
          </span>
        </div>
        <div className="wakeToggle">
          {wakeBusy ? <Loader2 className="spin" size={15} /> : null}
          <button
            type="button"
            role="switch"
            aria-checked={!!wakeHelper}
            className={`switchTrack ${wakeHelper ? "on" : ""}`}
            disabled={wakeBusy || wakeHelper === null}
            onClick={() => void toggleWakeHelper()}
            aria-label="Đánh thức máy để prime"
          >
            <span className="switchThumb" />
          </button>
        </div>
      </div>

      <div className="wakeRow">
        <div className="wakeText">
          <strong>
            <AlarmClock size={15} /> Tự prime cả khi máy ngủ
          </strong>
          <span className="muted">
            {primeAsleep === null
              ? "Đang kiểm tra…"
              : primeAsleep
                ? "Một tiến trình nền chạy dưới đúng macOS profile để gửi yêu cầu prime đúng giờ; nếu chưa đọc được token do Keychain khóa, lần tick sau sẽ thử lại."
                : "Chạy dưới đúng macOS profile và cấu hình từng account; không lưu mật khẩu máy. Claude sẽ thử lại sau khi mở khóa nếu Keychain đang bị khóa (cần quyền admin 1 lần)."}
          </span>
        </div>
        <div className="wakeToggle">
          {primeAsleepBusy ? <Loader2 className="spin" size={15} /> : null}
          <button
            type="button"
            role="switch"
            aria-checked={!!primeAsleep}
            className={`switchTrack ${primeAsleep ? "on" : ""}`}
            disabled={primeAsleepBusy || primeAsleep === null}
            onClick={() => void togglePrimeAsleep()}
            aria-label="Tự prime cả khi máy ngủ"
          >
            <span className="switchThumb" />
          </button>
        </div>
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
            const attempt = snapshot.primeAttempts[account.id];
            const enabled = setting?.enabled ?? false;
            const busy = busyId === account.id;
            return (
              <div key={account.id} className={`autoCard ${enabled ? "on" : ""}`}>
                <div className="autoCardHead">
                  <div className="autoCardTitle">
                    <strong>{account.name}</strong>
                    <span className="autoTool">{tool}</span>
                  </div>
                  <button
                    type="button"
                    role="switch"
                    aria-checked={enabled}
                    className={`switchTrack small ${enabled ? "on" : ""}`}
                    disabled={busy}
                    onClick={() => void save(account, !enabled)}
                    aria-label={enabled ? "Tắt auto prime" : "Bật auto prime"}
                    title={enabled ? "Tắt auto prime" : "Bật auto prime"}
                  >
                    <span className="switchThumb" />
                  </button>
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
                    {busy ? <Loader2 className="spin" size={15} /> : null} Lưu giờ
                  </button>
                </div>
                {plusFiveHours(timeOf(account)) && (
                  <p className="autoCardHint" title="Dự kiến (giờ neo + 5h). Nếu phiên cũ chưa hết, app sẽ hoãn nên mốc thực tế có thể muộn hơn.">
                    {timeOf(account)} → reset ~{plusFiveHours(timeOf(account))}
                  </p>
                )}
                {attempt && (
                  <p className="autoCardHint">
                    Đang mở session · thử lại {localHHMM(attempt.nextActionAt)} · hết hạn{" "}
                    {localHHMM(attempt.deadlineAt)}
                  </p>
                )}
                <div className="autoExtendToggle">
                  <button
                    type="button"
                    role="switch"
                    aria-checked={setting?.autoExtend ?? false}
                    className={`switchTrack small ${setting?.autoExtend ? "on" : ""}`}
                    onClick={() => void toggleAutoExtend(account, !(setting?.autoExtend ?? false))}
                    aria-label="Tự gia hạn không hỏi"
                  >
                    <span className="switchThumb" />
                  </button>
                  <span title="Khi phiên 5h sắp hết (≤30 phút), tự mở phiên kế tiếp mà không cần hỏi. Tắt = app sẽ hỏi trước.">
                    Tự gia hạn
                  </span>
                  <span className="autoCardResult">
                    {attempt ? "đang xác nhận…" : resultLabel(setting?.lastResult)}
                  </span>
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
        <button onClick={() => void viewStats()} disabled={loadingStats}>
          {loadingStats ? <Loader2 className="spin" size={15} /> : <BarChart3 size={15} />} Thống kê
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

      {stats !== null &&
        (stats.length === 0 ? (
          <p className="autoEmpty">Chưa có dữ liệu thống kê.</p>
        ) : (
          <table className="autoStatsTable">
            <thead>
              <tr>
                <th>Ngày</th>
                <th>Thành công</th>
                <th>Thất bại</th>
                <th>Hoãn</th>
                <th>Bỏ qua</th>
              </tr>
            </thead>
            <tbody>
              {stats.map((d) => (
                <tr key={d.date}>
                  <td>{d.date}</td>
                  <td>{d.success}</td>
                  <td>{d.failed}</td>
                  <td>{d.hold}</td>
                  <td>{d.skip}</td>
                </tr>
              ))}
            </tbody>
          </table>
        ))}

      {log !== null && (
        <pre className="autoLog">{log.trim() ? log : "Chưa có hoạt động nào được ghi."}</pre>
      )}
    </section>
  );
}
