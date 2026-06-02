import { useCallback, useEffect, useState } from "react";
import { listen } from "@tauri-apps/api/event";
import { AlertTriangle, BarChart3, Loader2, RefreshCw } from "lucide-react";
import { api } from "./tauri";
import type { DayUsage, ModelUsage, SessionUsage, TokenBreakdown, ToolUsage, UsageReport } from "./types";

const RANGES: { label: string; days: number }[] = [
  { label: "7d", days: 7 },
  { label: "30d", days: 30 },
  { label: "90d", days: 90 },
  { label: "All", days: 0 },
];

export function UsageView() {
  const [report, setReport] = useState<UsageReport | null>(null);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [selected, setSelected] = useState<string>("claude");
  const [range, setRange] = useState(30);

  const load = useCallback(async () => {
    setBusy(true);
    setError(null);
    try {
      setReport(await api.getUsage(range));
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
    } finally {
      setBusy(false);
    }
  }, [range]);

  useEffect(() => {
    void load();
  }, [load]);

  // The background poller refreshes the cache every 5 minutes → refetch with the current range.
  useEffect(() => {
    const unlisten = listen("usage-changed", () => void load());
    return () => {
      void unlisten.then((fn) => fn());
    };
  }, [load]);

  return (
    <section className="panel">
      <div className="panelHead">
        <div className="titleRow">
          <h2>Token Usage</h2>
          <PriceStatus report={report} />
        </div>
        <div className="actions">
          <div className="usageRange" role="group" aria-label="Time range">
            {RANGES.map((r) => (
              <button
                key={r.days}
                className={range === r.days ? "selected" : ""}
                onClick={() => setRange(r.days)}
                disabled={busy}
              >
                {r.label}
              </button>
            ))}
          </div>
          <button onClick={load} disabled={busy}>
            {busy ? <Loader2 className="spin" /> : <RefreshCw />}
            Refresh
          </button>
        </div>
      </div>

      <p className="usageLead">
        Token usage &amp; estimated cost from Claude Code and Codex local logs on this machine,
        totaled per tool across all accounts. Antigravity has no token logs and is not shown.
      </p>

      {error && (
        <div className="drift">
          <AlertTriangle />
          <span>{error}</span>
        </div>
      )}

      {report && (
        <>
          <div className="usageTabs">
            {report.tools.map((tool) => (
              <button
                key={tool.toolId}
                className={tool.toolId === selected ? "selected" : ""}
                onClick={() => setSelected(tool.toolId)}
              >
                {tool.displayName}
                {tool.estimate && <span className="estimateMini">≈ est</span>}
              </button>
            ))}
          </div>
          {(() => {
            const tool = report.tools.find((t) => t.toolId === selected) ?? report.tools[0];
            return tool ? (
              <ToolUsageSection tool={tool} priceUnavailable={report.priceStatus === "unavailable"} />
            ) : null;
          })()}
        </>
      )}

      {!report && !error && (
        <div className="empty">
          <Loader2 className="spin" />
          <span>Reading usage logs…</span>
        </div>
      )}
    </section>
  );
}

function PriceStatus({ report }: { report: UsageReport | null }) {
  if (!report) return null;
  if (report.priceStatus === "unavailable") {
    return <span className="badge warn" title="LiteLLM prices could not be loaded">Cost hidden</span>;
  }
  if (report.priceStatus === "cached") {
    const when = report.priceUpdatedAt ? formatDate(report.priceUpdatedAt) : "earlier";
    return <span className="badge muted" title={`Using saved LiteLLM prices from ${when}`}>Saved prices</span>;
  }
  return <span className="badge ok" title="LiteLLM prices loaded">Live prices</span>;
}

function ToolUsageSection({ tool, priceUnavailable }: { tool: ToolUsage; priceUnavailable: boolean }) {
  const empty = total(tool.total) === 0;
  return (
    <div className="usageToolBody">
      {empty ? (
        <div className="usageEmpty">
          <BarChart3 />
          <span>No usage yet. Use {tool.displayName} through the app to start tracking.</span>
        </div>
      ) : (
        <>
          <div className="usageStats">
            <StatTile label="Total cost" value={formatUsd(tool.totalCostUsd)} sub={`${formatTokens(total(tool.total))} tokens`} big />
            <StatTile label="Today" value={formatUsd(tool.todayCostUsd)} sub={`${formatTokens(total(tool.today))} tokens`} />
            <StatTile label="Output" value={formatTokens(tool.total.output)} sub="generated tokens" />
            <StatTile label="Cache read" value={formatTokens(tool.total.cacheRead)} sub="reused tokens" />
          </div>

          <TrendChart daily={tool.daily} priceUnavailable={priceUnavailable} />
          <ModelTable models={tool.byModel} />
          <SessionTable sessions={tool.sessions} />
        </>
      )}
    </div>
  );
}

function StatTile({ label, value, sub, big }: { label: string; value: string; sub: string; big?: boolean }) {
  return (
    <div className={`statTile ${big ? "big" : ""}`}>
      <span className="statLabel">{label}</span>
      <strong className="statValue">{value}</strong>
      <span className="statSub">{sub}</span>
    </div>
  );
}

/** Simple inline SVG bar chart of the last CHART_DAYS days (cost when priced, else tokens). */
// Totals/tables follow the selected range, but the bar chart stays readable by showing at most
// the 30 most recent days (so 90d / all time don't render hundreds of slivers).
const CHART_MAX_BARS = 30;

function TrendChart({ daily, priceUnavailable }: { daily: DayUsage[]; priceUnavailable: boolean }) {
  const days = daily.slice(-CHART_MAX_BARS);
  const [hover, setHover] = useState<number | null>(null);
  if (days.length === 0) return null;

  const useCost = !priceUnavailable && days.some((d) => d.costUsd != null);
  const valueOf = (d: DayUsage) => (useCost ? d.costUsd ?? 0 : total(d.tokens));
  const max = Math.max(...days.map(valueOf), 1);

  const width = 100;
  const height = 36;
  const gap = 1.5;
  const barW = (width - gap * (days.length - 1)) / days.length;

  const active = hover != null ? days[hover] : null;

  return (
    <div className="usageChart">
      <div className="usageChartHead">
        <span>{useCost ? "Daily cost" : "Daily tokens"} · last {days.length} days</span>
        {active ? (
          <span className="usageChartReadout">
            <strong>{active.date}</strong>
            {" · "}
            {formatTokens(total(active.tokens))} tokens
            {" · "}
            {formatUsd(active.costUsd)}
          </span>
        ) : (
          <span className="usageChartHint">hover a bar for the day</span>
        )}
      </div>
      <svg
        viewBox={`0 0 ${width} ${height}`}
        preserveAspectRatio="none"
        className="usageChartSvg"
        role="img"
        onMouseLeave={() => setHover(null)}
      >
        {days.map((d, i) => {
          const v = valueOf(d);
          const h = Math.max((v / max) * height, v > 0 ? 0.6 : 0);
          const x = i * (barW + gap);
          const label = `${d.date} · ${formatTokens(total(d.tokens))} tokens · ${formatUsd(d.costUsd)}`;
          return (
            <g key={d.date} onMouseEnter={() => setHover(i)}>
              {/* full-height hit area so thin bars are still easy to hover */}
              <rect x={x} y={0} width={barW + gap} height={height} fill="transparent" />
              <rect
                x={x}
                y={height - h}
                width={barW}
                height={h}
                rx={0.5}
                className={`usageBar ${hover === i ? "active" : ""}`}
              >
                <title>{label}</title>
              </rect>
            </g>
          );
        })}
      </svg>
      <div className="usageChartAxis">
        <span>{days[0].date.slice(5)}</span>
        <span>{days[days.length - 1].date.slice(5)}</span>
      </div>
    </div>
  );
}

function ModelTable({ models }: { models: ModelUsage[] }) {
  if (models.length === 0) return null;
  return (
    <div className="usageTable">
      <div className="usageTableHead">By model</div>
      <table>
        <thead>
          <tr>
            <th>Model</th>
            <th className="num">Tokens</th>
            <th className="num">Cost</th>
          </tr>
        </thead>
        <tbody>
          {models.map((m) => (
            <tr key={m.model}>
              <td><code>{m.model}</code></td>
              <td className="num">{formatTokens(total(m.tokens))}</td>
              <td className="num">{formatUsd(m.costUsd)}</td>
            </tr>
          ))}
        </tbody>
      </table>
    </div>
  );
}

function SessionTable({ sessions }: { sessions: SessionUsage[] }) {
  if (sessions.length === 0) return null;
  return (
    <div className="usageTable">
      <div className="usageTableHead">Recent sessions</div>
      <table>
        <thead>
          <tr>
            <th>Date</th>
            <th>Session</th>
            <th>Model</th>
            <th className="num">Tokens</th>
            <th className="num">Cost</th>
          </tr>
        </thead>
        <tbody>
          {sessions.map((s) => (
            <tr key={s.id + s.date}>
              <td>{s.date}</td>
              <td><code>{s.id.slice(0, 8)}</code></td>
              <td><code>{s.model}</code></td>
              <td className="num">{formatTokens(total(s.tokens))}</td>
              <td className="num">{formatUsd(s.costUsd)}</td>
            </tr>
          ))}
        </tbody>
      </table>
    </div>
  );
}

// --- helpers ---

function total(t: TokenBreakdown) {
  return t.input + t.output + t.cacheRead + t.cacheCreation;
}

function formatTokens(n: number) {
  if (n >= 1e9) return `${(n / 1e9).toFixed(2)}B`;
  if (n >= 1e6) return `${(n / 1e6).toFixed(2)}M`;
  if (n >= 1e3) return `${(n / 1e3).toFixed(1)}K`;
  return `${n}`;
}

function formatUsd(n: number | null) {
  if (n == null) return "—";
  if (n > 0 && n < 0.01) return `$${n.toFixed(4)}`;
  return `$${n.toFixed(2)}`;
}

function formatDate(value: string) {
  try {
    return new Intl.DateTimeFormat("en-US", { dateStyle: "short" }).format(new Date(value));
  } catch {
    return value;
  }
}
