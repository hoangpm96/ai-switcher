import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { listen } from "@tauri-apps/api/event";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { Check, Loader2, RefreshCw, Settings2, X } from "lucide-react";
import { api } from "./tauri";
import type {
  AppSnapshot,
  OverlaySettings,
  QuotaInfo,
  QuotaWindow,
  SystemLoad,
  ToolId,
} from "./types";
import "./overlay.css";

/** Rows are keyed `"<tool>:<accountId>"` so the same id under two tools can't collide. */
function rowKey(toolId: ToolId, accountId: string) {
  return `${toolId}:${accountId}`;
}

const toolShortNames: Record<ToolId, string> = {
  claude: "Claude",
  codex: "Codex",
  cursor: "Cursor",
  opencode: "opencode",
  antigravity: "AG",
};

/** CLIs whose quota can be re-read on demand (Antigravity needs its IDE open). */
const REFRESHABLE_TOOLS: ToolId[] = ["claude", "codex", "cursor", "opencode"];

const defaultSettings: OverlaySettings = {
  enabled: true,
  accounts: [],
  opacity: 0.45,
  hoverOpacity: 1,
  compact: false,
  clickThrough: false,
  showSystem: true,
  rect: { x: 40, y: 60, width: 288, height: 330 },
};

/** One line in the overlay: a switchable account, or a quota-only CLI (Cursor / opencode). */
interface Row {
  key: string;
  /** Short badge, e.g. `Claude`, `Cursor`. */
  toolLabel: string;
  name: string;
  quota: QuotaInfo | null;
  /** The account the plain command currently uses (accounts only). */
  active: boolean;
  /** API/proxy account: billed through a gateway, so it has no quota of its own. */
  isApi: boolean;
  /** Included by default when the user hasn't picked anything. */
  defaultShown: boolean;
}

/** Every account the overlay could show, in tab order. */
function allRows(snapshot: AppSnapshot): Row[] {
  return snapshot.tools.flatMap((tool) =>
    tool.accounts
      .filter((account) => !account.hidden)
      .map((account) => ({
        key: rowKey(tool.id, account.id),
        toolLabel: toolShortNames[tool.id],
        name: account.name,
        quota: account.quota,
        active: tool.activeAccountId === account.id,
        isApi: Boolean(account.apiProvider),
        // Default view: the account each CLI is actually using right now.
        defaultShown: tool.activeAccountId === account.id && tool.id !== "antigravity",
      })),
  );
}

/** The rows to render: the user's picks, or — when nothing is picked — the sensible default set. */
function selectedRows(snapshot: AppSnapshot, picked: string[]): Row[] {
  const rows = allRows(snapshot);
  if (picked.length === 0) {
    return rows.filter((row) => row.defaultShown);
  }
  const order = new Map(picked.map((key, index) => [key, index]));
  return rows
    .filter((row) => order.has(row.key))
    .sort((a, b) => (order.get(a.key) ?? 0) - (order.get(b.key) ?? 0));
}

/** The windows to draw for a row: the provider's own list when it has one (Cursor/opencode report
 *  three), otherwise the 5-hour + weekly pair that Claude and Codex use. */
function rowWindows(quota: QuotaInfo | null): QuotaWindow[] {
  if (!quota) return [];
  if (quota.models && quota.models.length > 0) return quota.models;
  return [quota.fiveHour, quota.weekly];
}

/** Squeeze a window label into the few characters an overlay row can spare. */
function shortWindowLabel(label: string) {
  const text = label.toLowerCase();
  if (text.includes("5-hour") || text.includes("5 hour")) return "5h";
  if (text.includes("week")) return "7d";
  if (text.includes("month")) return "30d";
  if (text.includes("rolling")) return "roll";
  if (text.includes("included") || text.includes("total")) return "incl";
  if (text.includes("auto")) return "auto";
  if (text.includes("api") || text.includes("named")) return "api";
  return label.slice(0, 4).toLowerCase();
}

export function OverlayApp() {
  const [snapshot, setSnapshot] = useState<AppSnapshot | null>(null);
  const [settings, setSettings] = useState<OverlaySettings>(defaultSettings);
  const [showSettings, setShowSettings] = useState(false);
  const [refreshing, setRefreshing] = useState(false);
  // Ghost mode: the overlay sits faint over whatever is underneath and only becomes solid while
  // the pointer is on it. In click-through mode the window gets no mouse events at all, so the
  // backend samples the pointer and pushes the state in via `overlay-hover` instead.
  const [hovered, setHovered] = useState(false);
  // The machine is close to choking: light the panel up even while idle so it gets noticed.
  const [sysAlert, setSysAlert] = useState(false);
  // Bumped on a timer so the "resets in …" labels count down without refetching quota.
  const [, setTick] = useState(0);
  const mounted = useRef(true);

  useEffect(() => {
    mounted.current = true;
    return () => {
      mounted.current = false;
    };
  }, []);

  useEffect(() => {
    void api
      .getSnapshot()
      .then((next) => mounted.current && setSnapshot(next))
      .catch(() => undefined);
    void api
      .getOverlaySettings()
      .then((next) => mounted.current && setSettings(next))
      .catch(() => undefined);
  }, []);

  // The backend pushes a snapshot after a switch / background refresh / auto-switch.
  useEffect(() => {
    const unlisten = listen<AppSnapshot>("snapshot-changed", (event) => setSnapshot(event.payload));
    return () => {
      void unlisten.then((fn) => fn());
    };
  }, []);

  // Pointer state pushed from the backend while clicks pass through the window.
  useEffect(() => {
    const unlisten = listen<boolean>("overlay-hover", (event) => setHovered(event.payload));
    return () => {
      void unlisten.then((fn) => fn());
    };
  }, []);

  // Settings can also be changed from the main window's Settings tab.
  useEffect(() => {
    const unlisten = listen<OverlaySettings>("overlay-settings-changed", (event) =>
      setSettings(event.payload),
    );
    return () => {
      void unlisten.then((fn) => fn());
    };
  }, []);

  // Slow poll: picks up quota the 5-minute backend poller refreshed, and keeps countdowns honest.
  useEffect(() => {
    const timer = window.setInterval(() => {
      setTick((value) => value + 1);
      void api
        .getSnapshot()
        .then((next) => mounted.current && setSnapshot(next))
        .catch(() => undefined);
    }, 30_000);
    return () => window.clearInterval(timer);
  }, []);

  const save = useCallback(async (next: OverlaySettings) => {
    setSettings(next); // optimistic: the switch shouldn't lag behind the click
    try {
      const saved = await api.setOverlaySettings(next);
      if (mounted.current) setSettings(saved);
    } catch {
      // Keep the optimistic value; the next `overlay-settings-changed` corrects it.
    }
  }, []);

  const refresh = useCallback(async () => {
    setRefreshing(true);
    try {
      await Promise.allSettled(REFRESHABLE_TOOLS.map((tool) => api.refreshTool(tool)));
      // Read the combined state once everything settled — each refresh returns a snapshot from its
      // own moment, so taking one of them could show the older picture.
      const next = await api.getSnapshot();
      if (mounted.current) setSnapshot(next);
    } catch {
      // Leave the previous numbers on screen; the next poll tries again.
    } finally {
      if (mounted.current) setRefreshing(false);
    }
  }, []);

  const close = useCallback(() => {
    void api.setOverlayEnabled(false).catch(() => {
      void getCurrentWindow().close();
    });
  }, []);

  const rows = useMemo(
    () => (snapshot ? selectedRows(snapshot, settings.accounts) : []),
    [snapshot, settings.accounts],
  );

  // Reading the settings needs the panel fully legible, whatever the idle opacity is.
  const solid = hovered || showSettings || (settings.showSystem && sysAlert);
  const opacity = solid ? settings.hoverOpacity : settings.opacity;

  return (
    <div
      className="ovRoot"
      style={{ "--ov-opacity": opacity } as React.CSSProperties}
      onMouseEnter={() => setHovered(true)}
      onMouseLeave={() => setHovered(false)}
    >
      <header className="ovBar" data-tauri-drag-region>
        <span className="ovTitle" data-tauri-drag-region>
          Quota
        </span>
        <button className="ovIcon" onClick={() => void refresh()} title="Làm mới quota" disabled={refreshing}>
          {refreshing ? <Loader2 className="ovSpin" size={12} /> : <RefreshCw size={12} />}
        </button>
        <button
          className={`ovIcon ${showSettings ? "on" : ""}`}
          onClick={() => setShowSettings((open) => !open)}
          title="Chọn account hiển thị"
        >
          <Settings2 size={12} />
        </button>
        <button className="ovIcon" onClick={close} title="Ẩn overlay (bật lại ở menu bar)">
          <X size={12} />
        </button>
      </header>

      {showSettings ? (
        <OverlaySettingsPanel
          snapshot={snapshot}
          settings={settings}
          onChange={(next) => void save(next)}
        />
      ) : (
        <div className="ovBody">
          {snapshot === null ? (
            <p className="ovHint">Đang tải…</p>
          ) : rows.length === 0 ? (
            <p className="ovHint">Chưa chọn account nào — bấm ⚙ để chọn.</p>
          ) : (
            rows.map((row) => <OverlayRow key={row.key} row={row} compact={settings.compact} />)
          )}
        </div>
      )}

      {settings.showSystem && <SystemFooter onAlert={setSysAlert} />}

      {/* Frameless windows have no visible edge to grab, so give resizing an explicit handle. */}
      <span
        className="ovGrip"
        title="Kéo để đổi kích thước"
        onMouseDown={(event) => {
          event.preventDefault();
          void getCurrentWindow().startResizeDragging("SouthEast");
        }}
      />
    </div>
  );
}

function OverlayRow({ row, compact }: { row: Row; compact: boolean }) {
  const quota = row.quota;
  // Compact mode keeps one bar per row — the shortest window, which is the one that runs out first.
  const windows = compact ? rowWindows(quota).slice(0, 1) : rowWindows(quota);

  return (
    <div className={`ovRow ${compact ? "compact" : ""}`}>
      <div className="ovRowHead">
        <span className="ovTool">{row.toolLabel}</span>
        <span className="ovName" title={`${row.toolLabel} · ${row.name}`}>
          {row.name}
        </span>
        {row.active && <span className="ovDot" title="Đang dùng" />}
        {quota?.plan && <span className="ovPlan">{quota.plan}</span>}
      </div>

      {quota?.error && quota.rateLimitedUntil && windows.some((w) => w.percentUsed != null) ? (
        // Rate limited: keep the last good bars (dimmed) and say so in one line.
        <>
          <div className="ovStale">
            {windows.map((window, index) => (
              <OverlayBar
                key={`${window.label}-${index}`}
                label={shortWindowLabel(window.label)}
                percent={window.percentUsed}
                resetAt={window.resetAt}
              />
            ))}
          </div>
          <p className="ovWarn" title={quota.error}>
            {quota.error}
          </p>
        </>
      ) : quota?.error ? (
        <p className="ovErr" title={quota.error}>
          {quota.error}
        </p>
      ) : row.isApi ? (
        <p className="ovHint">API gateway — không có quota</p>
      ) : windows.length === 0 ? (
        <p className="ovHint">Chưa có dữ liệu quota</p>
      ) : (
        windows.map((window, index) => (
          <OverlayBar
            key={`${window.label}-${index}`}
            label={shortWindowLabel(window.label)}
            percent={window.percentUsed}
            // Cursor's buckets share one billing reset — repeating the same countdown on every
            // row wastes the little width the overlay has.
            resetAt={
              index > 0 && window.resetAt === windows[index - 1].resetAt ? null : window.resetAt
            }
          />
        ))
      )}
    </div>
  );
}

function OverlayBar({
  label,
  percent,
  resetAt,
}: {
  label: string;
  percent: number | null;
  resetAt: string | null;
}) {
  const value = Math.max(0, Math.min(100, percent ?? 0));
  const level = percent === null ? "unknown" : value >= 90 ? "high" : value >= 70 ? "mid" : "low";
  return (
    <div className="ovBarRow">
      <span className="ovBarLabel">{label}</span>
      <span className="ovBarTrack" data-level={level}>
        <span className="ovBarFill" style={{ width: `${value}%` }} />
      </span>
      <strong className="ovBarPct">{percent === null ? "?" : `${Math.round(value)}%`}</strong>
      {resetAt && (
        <span className="ovReset" title={`Reset lúc ${absoluteTime(resetAt)}`}>
          {countdown(resetAt)}
        </span>
      )}
    </div>
  );
}

type Level = "low" | "mid" | "high";

/** How often the footer re-reads CPU/RAM. Short enough to catch a runaway app within seconds;
 *  one read walks every process in ~10 ms. */
const SYSTEM_POLL_MS = 3_000;

const GB = 1024 ** 3;

function worst(...levels: Level[]): Level {
  return levels.includes("high") ? "high" : levels.includes("mid") ? "mid" : "low";
}

function byThreshold(value: number, mid: number, high: number): Level {
  return value >= high ? "high" : value >= mid ? "mid" : "low";
}

/** `5.6` / `24` / `0.4` — GB with one decimal below 10, whole numbers above. */
function gb(bytes: number) {
  const value = bytes / GB;
  return value >= 10 ? value.toFixed(0) : value.toFixed(1);
}

/** Footer: machine CPU / RAM plus the apps holding the most memory and CPU, so a runaway app
 *  (an Electron tool at 40 GB, a stuck build) shows up before the Mac freezes. */
function SystemFooter({ onAlert }: { onAlert: (alert: boolean) => void }) {
  const [load, setLoad] = useState<SystemLoad | null>(null);

  useEffect(() => {
    let alive = true;
    const read = () =>
      void api
        .getSystemLoad()
        .then((next) => alive && setLoad(next))
        .catch(() => undefined);
    read();
    const timer = window.setInterval(read, SYSTEM_POLL_MS);
    return () => {
      alive = false;
      window.clearInterval(timer);
    };
  }, []);

  const cpuLevel = load ? byThreshold(load.cpuPercent, 70, 90) : "low";
  const memShare = load && load.memoryTotal > 0 ? (load.memoryUsed / load.memoryTotal) * 100 : 0;
  // Swap growing is what actually makes macOS stutter, more than "RAM used" being high.
  const memLevel = load
    ? worst(byThreshold(memShare, 80, 92), byThreshold(load.swapUsed / GB, 2, 6))
    : "low";
  const hogMemShare =
    load?.topMemory && load.memoryTotal > 0 ? (load.topMemory.memory / load.memoryTotal) * 100 : 0;
  const hogMemLevel = byThreshold(hogMemShare, 25, 40);
  // Several cores pinned by one app for a while is the classic "fan spins up" case.
  const hogCpuLevel = load?.topCpu ? byThreshold(load.topCpu.cpuPercent, 200, 400) : "low";
  const alert = worst(cpuLevel, memLevel, hogMemLevel, hogCpuLevel) === "high";

  useEffect(() => {
    onAlert(alert);
  }, [alert, onAlert]);

  if (!load) return null;

  const loadLevel = byThreshold(load.loadOne, load.cores, load.cores * 1.5);
  const showSwap = load.swapUsed >= 2 * GB;
  // The culprit line only shows up once something is worth a look — idle, the footer is one line.
  const busy = worst(cpuLevel, loadLevel, memLevel, hogMemLevel, hogCpuLevel) !== "low";
  const sameHog = load.topMemory && load.topCpu && load.topMemory.name === load.topCpu.name;

  return (
    <footer className="ovSys">
      <div className="ovSysLine">
        <SystemMeter
          label="CPU"
          percent={load.cpuPercent}
          level={worst(cpuLevel, loadLevel)}
          value={`${Math.round(load.cpuPercent)}%`}
          title={`CPU toàn máy ${Math.round(load.cpuPercent)}% · load 1 phút ${load.loadOne.toFixed(1)}/${load.cores} nhân`}
        />
        <SystemMeter
          label="RAM"
          percent={memShare}
          level={memLevel}
          value={`${gb(load.memoryUsed)}/${gb(load.memoryTotal)}G`}
          title={`RAM đang dùng ${gb(load.memoryUsed)}/${gb(load.memoryTotal)} GB · swap ${gb(load.swapUsed)} GB`}
        />
      </div>
      {busy && (
        <div className="ovSysLine ovSysHogs">
          <span className="ovSysArrow">▲</span>
          {showSwap && (
            <span data-level={byThreshold(load.swapUsed / GB, 2, 6)} title="Swap — tăng nhiều là máy bắt đầu đơ">
              swap {gb(load.swapUsed)}G
            </span>
          )}
          {load.topMemory && (
            <span
              data-level={hogMemLevel}
              title={`App giữ nhiều RAM nhất: ${load.topMemory.name} · ${gb(load.topMemory.memory)} GB`}
            >
              {load.topMemory.name} {gb(load.topMemory.memory)}G
              {sameHog && load.topCpu && (
                <span data-level={hogCpuLevel}> · {Math.round(load.topCpu.cpuPercent)}%</span>
              )}
            </span>
          )}
          {load.topCpu && !sameHog && (
            <span
              data-level={hogCpuLevel}
              title={`App ăn CPU nhất: ${load.topCpu.name} · ${Math.round(load.topCpu.cpuPercent)}% (100% = 1 nhân)`}
            >
              {load.topCpu.name} {Math.round(load.topCpu.cpuPercent)}%
            </span>
          )}
        </div>
      )}
    </footer>
  );
}

/** One half of the footer: `CPU ▰▰▱ 46%`, bar colored by how close to the limit it is. */
function SystemMeter({
  label,
  percent,
  level,
  value,
  title,
}: {
  label: string;
  percent: number;
  level: Level;
  value: string;
  title: string;
}) {
  return (
    <span className="ovSysMeter" title={title}>
      <span className="ovSysLabel">{label}</span>
      <span className="ovBarTrack ovSysTrack" data-level={level}>
        <span className="ovBarFill" style={{ width: `${Math.max(0, Math.min(100, percent))}%` }} />
      </span>
      <span className="ovSysValue" data-level={level}>
        {value}
      </span>
    </span>
  );
}

function OverlaySettingsPanel({
  snapshot,
  settings,
  onChange,
}: {
  snapshot: AppSnapshot | null;
  settings: OverlaySettings;
  onChange: (next: OverlaySettings) => void;
}) {
  const rows = snapshot ? allRows(snapshot) : [];
  const picked = new Set(settings.accounts);

  const toggleAccount = (key: string) => {
    const next = picked.has(key)
      ? settings.accounts.filter((item) => item !== key)
      : [...settings.accounts, key];
    onChange({ ...settings, accounts: next });
  };

  return (
    <div className="ovBody ovSettings">
      <p className="ovSectionTitle">Account hiển thị</p>
      {rows.length === 0 ? (
        <p className="ovHint">Chưa có account nào.</p>
      ) : (
        rows.map((row) => (
          <button
            key={row.key}
            className={`ovPick ${picked.has(row.key) ? "on" : ""}`}
            onClick={() => toggleAccount(row.key)}
          >
            <span className="ovPickBox">{picked.has(row.key) && <Check size={10} />}</span>
            <span className="ovTool">{row.toolLabel}</span>
            <span className="ovName">{row.name}</span>
            {row.active && <span className="ovDot" title="Đang dùng" />}
          </button>
        ))
      )}
      <p className="ovHint">Bỏ chọn hết = tự hiện account đang dùng của mỗi CLI.</p>

      <p className="ovSectionTitle">Hiển thị</p>
      <label className="ovOpt">
        <input
          type="checkbox"
          checked={settings.compact}
          onChange={(event) => onChange({ ...settings, compact: event.target.checked })}
        />
        Gọn (mỗi dòng 1 thanh)
      </label>
      <label className="ovOpt">
        <input
          type="checkbox"
          checked={settings.clickThrough}
          onChange={(event) => onChange({ ...settings, clickThrough: event.target.checked })}
        />
        Cho chuột xuyên qua
      </label>
      <label className="ovOpt">
        <input
          type="checkbox"
          checked={settings.showSystem}
          onChange={(event) => onChange({ ...settings, showSystem: event.target.checked })}
        />
        Hiện CPU / RAM máy
      </label>
      {settings.clickThrough && (
        <p className="ovHint">
          Chuột xuyên qua nhưng overlay vẫn sáng lên khi rê tới. Tắt lại ở Settings trong cửa sổ
          chính (overlay không bấm được nữa).
        </p>
      )}
      <label className="ovOpt ovSlider">
        Lúc rảnh
        <input
          type="range"
          min={15}
          max={100}
          step={5}
          value={Math.round(settings.opacity * 100)}
          onChange={(event) => onChange({ ...settings, opacity: Number(event.target.value) / 100 })}
        />
        <span>{Math.round(settings.opacity * 100)}%</span>
      </label>
      <label className="ovOpt ovSlider">
        Khi rê chuột
        <input
          type="range"
          min={15}
          max={100}
          step={5}
          value={Math.round(settings.hoverOpacity * 100)}
          onChange={(event) =>
            onChange({ ...settings, hoverOpacity: Number(event.target.value) / 100 })
          }
        />
        <span>{Math.round(settings.hoverOpacity * 100)}%</span>
      </label>
    </div>
  );
}

/** `2g 14p` / `18p` / `hết hạn` — how long until the window resets. */
function countdown(value: string) {
  const target = new Date(value).getTime();
  if (Number.isNaN(target)) return "";
  const minutes = Math.round((target - Date.now()) / 60_000);
  if (minutes <= 0) return "sắp reset";
  if (minutes < 60) return `${minutes}p`;
  const hours = Math.floor(minutes / 60);
  if (hours < 24) return `${hours}g ${minutes % 60}p`;
  return `${Math.floor(hours / 24)}n ${hours % 24}g`;
}

function absoluteTime(value: string) {
  return new Intl.DateTimeFormat("vi-VN", { dateStyle: "short", timeStyle: "short" }).format(
    new Date(value),
  );
}
