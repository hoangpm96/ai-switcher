import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { listen } from "@tauri-apps/api/event";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { Check, Loader2, RefreshCw, Settings2, X } from "lucide-react";
import { api } from "./tauri";
import type { AppSnapshot, OverlaySettings, QuotaInfo, QuotaWindow, ToolId } from "./types";
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
  const solid = hovered || showSettings;
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
