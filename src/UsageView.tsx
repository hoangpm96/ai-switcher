import { useCallback, useEffect, useMemo, useState } from "react";
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
  const [selected, setSelected] = useState<string>("all");
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

  const usageTools = useMemo(() => {
    if (!report) return [];
    return [buildAllUsage(report.tools), ...report.tools];
  }, [report]);

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
            {usageTools.map((tool) => (
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
            const tool = usageTools.find((t) => t.toolId === selected) ?? usageTools[0];
            return tool ? (
              <ToolUsageSection tool={tool} range={range} priceUnavailable={report.priceStatus === "unavailable"} />
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

function ToolUsageSection({ tool, range, priceUnavailable }: { tool: ToolUsage; range: number; priceUnavailable: boolean }) {
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

          <TrendChart daily={tool.daily} range={range} priceUnavailable={priceUnavailable} />
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
// the 45 most recent days (so 90d / all time don't render hundreds of slivers).
const CHART_MAX_BARS = 45;

function TrendChart({
  daily,
  range,
  priceUnavailable,
}: {
  daily: DayUsage[];
  range: number;
  priceUnavailable: boolean;
}) {
  const [hover, setHover] = useState<number | null>(null);

  // Build a continuous calendar window ending today so days with no usage still show as a 0 bar
  // (otherwise "7d" would render fewer bars than 7 when some days went unused).
  const windowLen = range > 0 ? Math.min(range, CHART_MAX_BARS) : CHART_MAX_BARS;
  const byDate = new Map(daily.map((d) => [d.date, d] as const));
  const today = localToday();
  const days: DayUsage[] = Array.from({ length: windowLen }, (_, i) => {
    const date = addDays(today, -(windowLen - 1 - i));
    return byDate.get(date) ?? { date, tokens: { input: 0, output: 0, cacheRead: 0, cacheCreation: 0 }, costUsd: null };
  });
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

function buildAllUsage(tools: ToolUsage[]): ToolUsage {
  const daily = mergeByDate(tools.flatMap((tool) => tool.daily));
  const byModel = mergeByModel(tools.flatMap((tool) => tool.byModel));
  const sessions = tools
    .flatMap((tool) =>
      tool.sessions.map((session) => ({
        ...session,
        id: `${tool.toolId}:${session.id}`,
        model: `${tool.displayName} / ${session.model}`,
      })),
    )
    .sort((a, b) => b.date.localeCompare(a.date))
    .slice(0, 20);

  return {
    toolId: "all",
    displayName: "All",
    estimate: tools.some((tool) => tool.estimate),
    total: sumTokens(tools.map((tool) => tool.total)),
    totalCostUsd: sumNullable(tools.map((tool) => tool.totalCostUsd)),
    today: sumTokens(tools.map((tool) => tool.today)),
    todayCostUsd: sumNullable(tools.map((tool) => tool.todayCostUsd)),
    daily,
    byModel,
    sessions,
  };
}

function mergeByDate(days: DayUsage[]): DayUsage[] {
  const byDate = new Map<string, { tokens: TokenBreakdown; costs: (number | null)[] }>();
  for (const day of days) {
    const current = byDate.get(day.date) ?? { tokens: zeroTokens(), costs: [] };
    current.tokens = addTokens(current.tokens, day.tokens);
    current.costs.push(day.costUsd);
    byDate.set(day.date, current);
  }
  return Array.from(byDate.entries())
    .map(([date, item]) => ({
      date,
      tokens: item.tokens,
      costUsd: sumNullable(item.costs),
    }))
    .sort((a, b) => a.date.localeCompare(b.date));
}

function mergeByModel(models: ModelUsage[]): ModelUsage[] {
  const byModel = new Map<string, { tokens: TokenBreakdown; costs: (number | null)[] }>();
  for (const model of models) {
    const current = byModel.get(model.model) ?? { tokens: zeroTokens(), costs: [] };
    current.tokens = addTokens(current.tokens, model.tokens);
    current.costs.push(model.costUsd);
    byModel.set(model.model, current);
  }
  return Array.from(byModel.entries())
    .map(([model, item]) => ({
      model,
      tokens: item.tokens,
      costUsd: sumNullable(item.costs),
    }))
    .sort((a, b) => total(b.tokens) - total(a.tokens));
}

function sumTokens(items: TokenBreakdown[]) {
  return items.reduce(addTokens, zeroTokens());
}

function addTokens(a: TokenBreakdown, b: TokenBreakdown): TokenBreakdown {
  return {
    input: a.input + b.input,
    output: a.output + b.output,
    cacheRead: a.cacheRead + b.cacheRead,
    cacheCreation: a.cacheCreation + b.cacheCreation,
  };
}

function zeroTokens(): TokenBreakdown {
  return { input: 0, output: 0, cacheRead: 0, cacheCreation: 0 };
}

function sumNullable(values: (number | null)[]) {
  const present = values.filter((value): value is number => value != null);
  return present.length > 0 ? present.reduce((sum, value) => sum + value, 0) : null;
}

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

function localToday() {
  return fmtDay(new Date());
}

function addDays(date: string, n: number) {
  const d = new Date(`${date}T00:00:00`);
  d.setDate(d.getDate() + n);
  return fmtDay(d);
}

function fmtDay(d: Date) {
  const y = d.getFullYear();
  const m = String(d.getMonth() + 1).padStart(2, "0");
  const day = String(d.getDate()).padStart(2, "0");
  return `${y}-${m}-${day}`;
}

function formatDate(value: string) {
  try {
    return new Intl.DateTimeFormat("en-US", { dateStyle: "short" }).format(new Date(value));
  } catch {
    return value;
  }
}
