import { useCallback, useEffect, useState } from "react";
import { listen } from "@tauri-apps/api/event";
import { Download, Loader2, RefreshCw, Sparkles, X } from "lucide-react";
import { api } from "./tauri";
import type { UpdateState } from "./types";

/*
 * In-app updates, modelled on Reqwise Figma's: a card in the sidebar when a newer version is out
 * ("what's new" + "Update & restart"), a progress bar while it downloads, and a note after the app
 * reopened on the new version. The backend (`updater.rs`) checks on its own; Settings has a manual
 * check. Wording follows the system language: Vietnamese on a Vietnamese Mac, English otherwise.
 */

const VI = typeof navigator !== "undefined" && navigator.language.toLowerCase().startsWith("vi");
const L = (en: string, vi: string) => (VI ? vi : en);

/** The release notes are a JSON list of `{ en, vi }` lines; fall back to plain text lines. */
export function noteLines(notes?: string | null): string[] {
  if (!notes) return [];
  try {
    const parsed: unknown = JSON.parse(notes);
    if (Array.isArray(parsed)) {
      return parsed
        .map((line) =>
          typeof line === "string"
            ? line
            : line && typeof line === "object"
              ? String((line as Record<string, unknown>)[VI ? "vi" : "en"] ?? (line as Record<string, unknown>).en ?? "")
              : "",
        )
        .filter(Boolean);
    }
  } catch {
    // plain text
  }
  return notes
    .split("\n")
    .map((line) => line.replace(/^[-*•]\s*/, "").trim())
    .filter(Boolean);
}

export function useUpdates() {
  const [state, setState] = useState<UpdateState | null>(null);
  const [installError, setInstallError] = useState<string | null>(null);

  useEffect(() => {
    let alive = true;
    api
      .updateStatus()
      .then((next) => alive && setState(next))
      .catch(() => undefined);
    const unlisten = listen<UpdateState>("update-changed", (event) => setState(event.payload));
    return () => {
      alive = false;
      void unlisten.then((stop) => stop());
    };
  }, []);

  const check = useCallback(async () => {
    try {
      setState(await api.updateCheck());
    } catch {
      // the state carries the error
    }
  }, []);

  const install = useCallback(async () => {
    setInstallError(null);
    try {
      await api.updateInstall(); // the app restarts on success
    } catch (err) {
      setInstallError(err instanceof Error ? err.message : String(err));
    }
  }, []);

  const seen = useCallback(async () => {
    try {
      setState(await api.updateSeen());
    } catch {
      setState((prev) => (prev ? { ...prev, justUpdated: null } : prev));
    }
  }, []);

  return { state, check, install, seen, installError };
}

function percent(u: UpdateState) {
  return u.total ? Math.min(100, Math.round((u.downloaded / u.total) * 100)) : null;
}

/** Sidebar card: shown while a newer version is on offer, downloading or installing. */
export function UpdateCard({
  state,
  onInstall,
}: {
  state: UpdateState | null;
  onInstall: () => void;
}) {
  const [dismissed, setDismissed] = useState<string | null>(null);
  if (!state?.info || state.phase === "idle") return null;
  const { info } = state;
  if (state.phase === "available" && dismissed === info.version) return null;
  const notes = noteLines(info.notes);
  const busy = state.phase === "downloading" || state.phase === "installing";
  const pct = percent(state);

  return (
    <div className="updateCard">
      <div className="updateHead">
        <Sparkles size={14} />
        <strong>{L(`Version ${info.version} is out`, `Đã có bản ${info.version}`)}</strong>
        {!busy && (
          <button
            className="updateClose"
            onClick={() => setDismissed(info.version)}
            title={L("Later", "Để sau")}
            aria-label={L("Later", "Để sau")}
          >
            <X size={12} />
          </button>
        )}
      </div>
      {notes.length > 0 && (
        <ul className="updateNotes">
          {notes.slice(0, 3).map((line) => (
            <li key={line}>{line}</li>
          ))}
          {notes.length > 3 && <li className="more">{L(`+${notes.length - 3} more`, `+${notes.length - 3} thay đổi khác`)}</li>}
        </ul>
      )}
      {state.phase === "downloading" && (
        <div className="updateProgress" title={pct === null ? undefined : `${pct}%`}>
          <span style={{ width: `${pct ?? 8}%` }} />
        </div>
      )}
      {state.error && state.phase === "available" && <p className="updateError">{state.error}</p>}
      <button className="primary updateBtn" onClick={onInstall} disabled={busy}>
        {busy ? <Loader2 className="spin" size={13} /> : <Download size={13} />}
        {state.phase === "downloading"
          ? L(`Downloading… ${pct ?? 0}%`, `Đang tải… ${pct ?? 0}%`)
          : state.phase === "installing"
            ? L("Reopening…", "Đang mở lại…")
            : L("Update & restart", "Cập nhật & mở lại")}
      </button>
      {!busy && <small className="updateHint">{L("The app reopens by itself in a few seconds.", "App tự mở lại sau vài giây.")}</small>}
    </div>
  );
}

/** One-time note after the app reopened on a version it just installed. */
export function UpdatedNote({ state, onClose }: { state: UpdateState | null; onClose: () => void }) {
  const info = state?.justUpdated;
  if (!info) return null;
  const notes = noteLines(info.notes);
  return (
    <div className="updatedNote" role="status">
      <div className="updateHead">
        <Sparkles size={14} />
        <strong>{L(`Updated to ${info.version}`, `Đã lên bản ${info.version}`)}</strong>
        <button className="updateClose" onClick={onClose} title={L("Close", "Đóng")} aria-label={L("Close", "Đóng")}>
          <X size={12} />
        </button>
      </div>
      {notes.length > 0 && (
        <ul className="updateNotes">
          {notes.map((line) => (
            <li key={line}>{line}</li>
          ))}
        </ul>
      )}
    </div>
  );
}

function clock(iso: string) {
  const date = new Date(iso);
  if (Number.isNaN(date.getTime())) return "";
  return new Intl.DateTimeFormat(VI ? "vi-VN" : "en-US", { hour: "2-digit", minute: "2-digit", hour12: false }).format(date);
}

/** Settings section: the running version, a manual check and the result. */
export function UpdateSettings({
  state,
  onCheck,
  onInstall,
  installError,
}: {
  state: UpdateState | null;
  onCheck: () => void;
  onInstall: () => void;
  installError: string | null;
}) {
  const busy = state?.phase === "downloading" || state?.phase === "installing";
  const status = !state
    ? ""
    : state.checking
      ? L("Checking…", "Đang kiểm tra…")
      : state.info && state.phase !== "idle"
        ? L(`Version ${state.info.version} is available.`, `Đã có bản ${state.info.version}.`)
        : state.error
          ? L(`Couldn't check: ${state.error}`, `Không kiểm tra được: ${state.error}`)
          : state.checkedAt
            ? L(`Up to date · checked at ${clock(state.checkedAt)}`, `Đang là bản mới nhất · kiểm tra lúc ${clock(state.checkedAt)}`)
            : L("Checks for updates by itself every few hours.", "App tự kiểm tra bản mới vài giờ một lần.");

  return (
    <div className="settingsSection">
      <div className="settingsSectionHead">
        <Download />
        <div>
          <strong>{L("Updates", "Cập nhật")}</strong>
          <small>
            {L("Version", "Phiên bản")} {state?.current ?? ""} · {status}
          </small>
        </div>
      </div>
      <div className="updateSettingsRow">
        <button onClick={onCheck} disabled={!state || state.checking || busy}>
          {state?.checking ? <Loader2 className="spin" size={13} /> : <RefreshCw size={13} />}
          {L("Check for updates", "Kiểm tra bản mới")}
        </button>
        {state?.info && state.phase !== "idle" && (
          <button className="primary" onClick={onInstall} disabled={busy}>
            {busy ? <Loader2 className="spin" size={13} /> : <Download size={13} />}
            {state.phase === "downloading"
              ? L(`Downloading… ${percent(state) ?? 0}%`, `Đang tải… ${percent(state) ?? 0}%`)
              : state.phase === "installing"
                ? L("Reopening…", "Đang mở lại…")
                : L(`Update to ${state.info.version} & restart`, `Cập nhật lên ${state.info.version} & mở lại`)}
          </button>
        )}
      </div>
      {installError && <p className="updateError">{installError}</p>}
    </div>
  );
}
