import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { listen } from "@tauri-apps/api/event";
import { AlertTriangle, BarChart3, ChevronDown, FolderKanban, LayoutDashboard, Loader2, RefreshCw, Users } from "lucide-react";
import { api } from "./tauri";
import type { DayUsage, ModelUsage, ProjectUsage, SessionUsage, TokenBreakdown, ToolUsage, UsageReport } from "./types";

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
  const [view, setView] = useState<"overview" | "projects" | "accounts">("overview");
  // A scan can take seconds, and `usage-changed` (or a range switch) can start another one while
  // the first is still running. Only the newest request may write state, so a slow earlier scan
  // can't overwrite fresher numbers — or update state after the tab is gone.
  const requestSeq = useRef(0);
  const mounted = useRef(true);

  useEffect(() => {
    mounted.current = true;
    return () => {
      mounted.current = false;
    };
  }, []);

  const load = useCallback(async () => {
    const ticket = (requestSeq.current += 1);
    setBusy(true);
    setError(null);
    try {
      const next = await api.getUsage(range);
      if (mounted.current && ticket === requestSeq.current) setReport(next);
    } catch (err) {
      if (mounted.current && ticket === requestSeq.current) {
        setError(err instanceof Error ? err.message : String(err));
      }
    } finally {
      if (mounted.current && ticket === requestSeq.current) setBusy(false);
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
        totaled per tool across all accounts. The Accounts view splits Claude usage per
        subscription login. Antigravity has no token logs and is not shown.
      </p>

      {error && (
        <div className="drift">
          <AlertTriangle />
          <span>{error}</span>
        </div>
      )}

      {report && (
        <>
          <div className="usageViewSwitch" role="group" aria-label="Usage view">
            <button className={view === "overview" ? "selected" : ""} onClick={() => setView("overview")}>
              <LayoutDashboard /> Overview
            </button>
            <button className={view === "projects" ? "selected" : ""} onClick={() => setView("projects")}>
              <FolderKanban /> Projects
            </button>
            <button className={view === "accounts" ? "selected" : ""} onClick={() => setView("accounts")}>
              <Users /> Accounts
            </button>
          </div>
          {view !== "accounts" && (
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
          )}
          {view === "accounts" ? (
            <AccountUsageSection
              tool={report.tools.find((t) => t.toolId === "claude")}
              range={range}
              priceUnavailable={report.priceStatus === "unavailable"}
            />
          ) : (
            (() => {
              const tool = usageTools.find((t) => t.toolId === selected) ?? usageTools[0];
              return tool ? (
                view === "overview" ? (
                  <ToolUsageSection tool={tool} range={range} priceUnavailable={report.priceStatus === "unavailable"} />
                ) : (
                  <ProjectUsageSection
                    tool={tool}
                    range={range}
                  />
                )
              ) : null;
            })()
          )}
        </>
      )}

      {!report && !error && (
        <div className="empty">
          <Loader2 className="spin" />
          <span>Reading usage logs… the first project scan can take around 30 seconds.</span>
        </div>
      )}
    </section>
  );
}

function ProjectUsageSection({
  tool,
  range,
}: {
  tool: ToolUsage;
  range: number;
}) {
  const [expanded, setExpanded] = useState<string | null>(null);
  const projects = tool.projects;
  if (projects.length === 0) {
    return (
      <div className="usageEmpty">
        <FolderKanban />
        <span>No project usage found in the selected range.</span>
      </div>
    );
  }
  const rangeLabel = range === 0 ? "All time" : `Last ${range} days`;
  return (
    <div className="projectUsage">
      <div className="projectUsageHead">
        <div>
          <strong>{projects.length} projects</strong>
          <span>{rangeLabel}</span>
        </div>
        <span>Open a project for daily, model, and session details</span>
      </div>
      <div className="projectGrid">
        {projects.map((project) => (
          <ProjectCard
            key={project.path}
            project={project}
            estimate={tool.estimate}
            range={range}
            expanded={expanded === project.path}
            onToggle={() => setExpanded(expanded === project.path ? null : project.path)}
          />
        ))}
      </div>
    </div>
  );
}

function ProjectCard({
  project,
  estimate,
  range,
  expanded,
  onToggle,
}: {
  project: ProjectUsage;
  estimate: boolean;
  range: number;
  expanded: boolean;
  onToggle: () => void;
}) {
  return (
    <article className={`projectCard ${expanded ? "expanded" : ""}`}>
      <div className="projectCardHead">
        <div className="projectIdentity">
          <FolderKanban />
          <div>
            <strong title={project.path}>{projectName(project.path)}</strong>
            <code title={project.path}>{project.path}</code>
          </div>
        </div>
        <div className="projectCost">
          <strong>{formatUsd(project.costUsd)}</strong>
          <span>{estimate ? "≈ " : ""}{formatTokens(total(project.tokens))} tokens</span>
        </div>
      </div>

      <div className="projectMeta">
        <span><strong>{project.sessionCount}</strong> sessions</span>
        <span>Last active <strong>{project.lastActive === "unknown" ? "—" : project.lastActive}</strong></span>
      </div>

      <button className={`projectExpand ${expanded ? "expanded" : ""}`} onClick={onToggle}>
        {expanded ? "Hide details" : "View details"}
        <ChevronDown />
      </button>
      {expanded && (
        <div className="projectDetail">
          <div className="usageStats projectStats">
            <StatTile label="Range cost" value={formatUsd(project.costUsd)} sub={`${formatTokens(total(project.tokens))} tokens`} big />
            <StatTile label="Output" value={formatTokens(project.tokens.output)} sub="generated tokens" />
            <StatTile label="Cache read" value={formatTokens(project.tokens.cacheRead)} sub="reused tokens" />
          </div>
          <TrendChart
            daily={project.daily}
            range={range}
            priceUnavailable={!project.daily.some((day) => day.costUsd != null)}
          />
          <ModelTable models={project.byModel} />
          <SessionTable sessions={project.sessions} />
        </div>
      )}
    </article>
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
          {(tool.unpricedModels?.length ?? 0) > 0 && (
            <p className="usageUnpriced" title={tool.unpricedModels.join(", ")}>
              <AlertTriangle size={13} />
              Chi phí dưới đây là mức tối thiểu — {tool.unpricedModels.length} model chưa có giá:{" "}
              {tool.unpricedModels.slice(0, 3).join(", ")}
              {tool.unpricedModels.length > 3 ? "…" : ""}
            </p>
          )}
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

function AccountUsageSection({
  tool,
  range,
  priceUnavailable,
}: {
  tool: ToolUsage | undefined;
  range: number;
  priceUnavailable: boolean;
}) {
  // Rows come sorted with the "" row (usage no account can be pinned on) last — it stays in the
  // list as one "Unknown account" block so the totals still add up to the Overview.
  const accounts = useMemo(() => tool?.accounts ?? [], [tool]);
  const knownCount = accounts.filter((a) => a.orgUuid !== "").length;
  // Earliest day with per-account data — anything before it can only be "Unknown account".
  const since = useMemo(
    () =>
      accounts
        .filter((a) => a.orgUuid !== "")
        .flatMap((a) => a.daily.map((d) => d.date))
        .filter((d) => d !== "unknown")
        .sort()[0],
    [accounts],
  );
  // null = never touched → default to every row.
  const [picked, setPicked] = useState<Set<string> | null>(null);
  // A refresh (or range switch) can drop rows — prune ids that no longer exist.
  useEffect(() => {
    setPicked((prev) => {
      if (prev === null) return prev;
      const ids = new Set(accounts.map((a) => a.orgUuid));
      const next = new Set([...prev].filter((id) => ids.has(id)));
      return next.size === prev.size ? prev : next;
    });
  }, [accounts]);
  const selected = useMemo(() => {
    const ids = new Set(accounts.map((a) => a.orgUuid));
    const base = picked ?? ids;
    return new Set([...base].filter((id) => ids.has(id)));
  }, [accounts, picked]);

  // null = every project (no filter); a set limits every number in the view to those projects.
  const [pickedProjects, setPickedProjects] = useState<Set<string> | null>(null);
  // Account rows with the per-project breakdown open, keyed by orgUuid.
  const [openProjects, setOpenProjects] = useState<Set<string>>(new Set());
  // Filter options = union of all accounts' project paths, tokens summed across accounts.
  const projectOptions = useMemo(() => {
    const byPath = new Map<string, number>();
    for (const a of accounts) {
      for (const p of a.projects) byPath.set(p.path, (byPath.get(p.path) ?? 0) + total(p.tokens));
    }
    return [...byPath.entries()]
      .map(([path, tokens]) => ({ path, tokens }))
      .sort((a, b) => b.tokens - a.tokens);
  }, [accounts]);
  // A refresh or range switch can drop projects — prune paths that no longer exist.
  useEffect(() => {
    setPickedProjects((prev) => {
      if (prev === null) return prev;
      const paths = new Set(projectOptions.map((o) => o.path));
      const next = new Set([...prev].filter((p) => paths.has(p)));
      if (next.size === 0) return null;
      return next.size === prev.size ? prev : next;
    });
  }, [projectOptions]);
  // Per-row view: the account's own rollups when unfiltered, else sums over its matching
  // projects — accounts with no usage in the selection drop out of the list entirely.
  const accountRows = useMemo(
    () =>
      accounts.flatMap((a) => {
        const projects = pickedProjects
          ? a.projects.filter((p) => pickedProjects.has(p.path))
          : a.projects;
        if (pickedProjects && projects.length === 0) return [];
        return [
          {
            account: a,
            projects,
            tokens: pickedProjects ? sumTokens(projects.map((p) => p.tokens)) : a.tokens,
            costUsd: pickedProjects ? sumNullable(projects.map((p) => p.costUsd)) : a.costUsd,
            // "unknown" sorts after every ISO date, so it must never win the max.
            lastActive: pickedProjects
              ? projects
                  .map((p) => p.lastActive)
                  .filter((d) => d !== "unknown")
                  .reduce((m, d) => (d > m ? d : m), "") || "unknown"
              : a.lastActive,
            daily: pickedProjects ? mergeByDate(projects.flatMap((p) => p.daily)) : a.daily,
            byModel: pickedProjects ? mergeByModel(projects.flatMap((p) => p.byModel)) : a.byModel,
            sessions: pickedProjects ? projects.flatMap((p) => p.sessions) : a.sessions,
          },
        ];
      }),
    [accounts, pickedProjects],
  );

  if (accounts.length === 0) {
    return (
      <div className="usageEmpty">
        <Users />
        <span>No per-account usage found for Claude Code in the selected range.</span>
      </div>
    );
  }

  const toggle = (orgUuid: string) => {
    const next = new Set(selected);
    if (next.has(orgUuid)) next.delete(orgUuid);
    else next.add(orgUuid);
    setPicked(next);
  };

  const toggleRowProjects = (orgUuid: string) => {
    setOpenProjects((prev) => {
      const next = new Set(prev);
      if (next.has(orgUuid)) next.delete(orgUuid);
      else next.add(orgUuid);
      return next;
    });
  };

  const filtering = pickedProjects !== null;
  // Summary only sees accounts that are checked AND (when filtering) have matching projects —
  // each row already carries its filtered rollups.
  const rows = accountRows.filter((r) => selected.has(r.account.orgUuid));
  const tokens = sumTokens(rows.map((r) => r.tokens));
  const costUsd = sumNullable(rows.map((r) => r.costUsd));
  const daily = mergeByDate(rows.flatMap((r) => r.daily));
  const byModel = mergeByModel(rows.flatMap((r) => r.byModel));
  const sessions = rows
    .flatMap((r) => r.sessions.map((s) => ({ ...s, model: `${r.account.label} / ${s.model}` })))
    .sort((a, b) => b.date.localeCompare(a.date) || total(b.tokens) - total(a.tokens))
    .slice(0, 30);
  const est = tool?.estimate ?? false;
  const rangeLabel = range === 0 ? "All time" : `Last ${range} days`;

  return (
    <div className="usageAccounts">
      <div className="usageAccountsHead">
        <div>
          <strong>{knownCount} Claude accounts</strong>
          <span>
            {rangeLabel} ·{" "}
            {filtering ? `${rows.length} of ${accountRows.length} shown` : `${selected.size} selected`}
          </span>
        </div>
        <div className="usageAccountToolbar">
          <button onClick={() => setPicked(new Set(accounts.map((a) => a.orgUuid)))}>Select all</button>
          <button onClick={() => setPicked(new Set())}>Clear</button>
        </div>
      </div>
      {projectOptions.length > 0 && (
        <ProjectFilter options={projectOptions} picked={pickedProjects} onChange={setPickedProjects} />
      )}
      <div className="usageAccountList">
        {accountRows.map((row) => {
          const a = row.account;
          const on = selected.has(a.orgUuid);
          const unknown = a.orgUuid === "";
          const projectsOpen = openProjects.has(a.orgUuid);
          return (
            <article
              key={a.orgUuid || "unknown"}
              className={`usageAccountRow ${on ? "selected" : ""} ${unknown ? "unknown" : ""}`}
            >
              <label className="usageAccountMain">
                <input type="checkbox" checked={on} onChange={() => toggle(a.orgUuid)} />
                <div className="usageAccountInfo">
                  <div className="usageAccountTitle">
                    <strong>{unknown ? "Unknown account" : a.label}</strong>
                    {a.removed && <span className="badge muted">Removed</span>}
                  </div>
                  {unknown && (
                    <span className="usageAccountNames">
                      {since
                        ? `Sessions with no account record — logged before ${since}, or run with an API key`
                        : "Sessions with no account record — logged before Claude Code recorded it, or run with an API key"}
                    </span>
                  )}
                  {a.accountNames.length > 0 && (
                    <span className="usageAccountNames">{a.accountNames.join(", ")}</span>
                  )}
                  <div className="usageAccountSplit">
                    <span>In <strong>{formatTokens(row.tokens.input)}</strong></span>
                    <span>Out <strong>{formatTokens(row.tokens.output)}</strong></span>
                    <span>Cache read <strong>{formatTokens(row.tokens.cacheRead)}</strong></span>
                    <span>Cache write <strong>{formatTokens(row.tokens.cacheCreation)}</strong></span>
                  </div>
                </div>
                <div className="usageAccountNums">
                  <strong>{formatUsd(row.costUsd)}</strong>
                  <span>{est ? "≈ " : ""}{formatTokens(total(row.tokens))} tokens</span>
                  <span>last {row.lastActive === "unknown" ? "—" : row.lastActive}</span>
                </div>
              </label>
              {row.projects.length > 0 && (
                <>
                  <button
                    className={`projectExpand usageAccountExpand ${projectsOpen ? "expanded" : ""}`}
                    onClick={() => toggleRowProjects(a.orgUuid)}
                  >
                    {row.projects.length} project{row.projects.length === 1 ? "" : "s"}
                    <ChevronDown />
                  </button>
                  {projectsOpen && (
                    <ul className="usageAccountProjects">
                      {row.projects.slice(0, 8).map((p) => (
                        <li key={p.path}>
                          <span className="usageAccountProjectName" title={p.path}>
                            {projectName(p.path)}
                          </span>
                          <span className="usageAccountProjectStats">
                            {formatTokens(total(p.tokens))} · {formatUsd(p.costUsd)}
                          </span>
                        </li>
                      ))}
                      {row.projects.length > 8 && (
                        <li className="usageAccountProjectsMore">+{row.projects.length - 8} more</li>
                      )}
                    </ul>
                  )}
                </>
              )}
            </article>
          );
        })}
      </div>
      {rows.length === 0 ? (
        <div className="usageEmpty">
          <Users />
          <span>
            {filtering && accountRows.length === 0
              ? "No account has usage in the selected projects."
              : "Select at least one account to see combined usage."}
          </span>
        </div>
      ) : (
        <>
          <div className="usageStats">
            <StatTile label="Total cost" value={formatUsd(costUsd)} sub={`${est ? "≈ " : ""}${formatTokens(total(tokens))} tokens`} big />
            <StatTile label="Input" value={formatTokens(tokens.input)} sub="prompt tokens" />
            <StatTile label="Output" value={formatTokens(tokens.output)} sub="generated tokens" />
            <StatTile label="Cache read" value={formatTokens(tokens.cacheRead)} sub="reused tokens" />
            <StatTile label="Cache write" value={formatTokens(tokens.cacheCreation)} sub="cached tokens" />
          </div>
          <TrendChart daily={daily} range={range} priceUnavailable={priceUnavailable} />
          <ModelTable models={byModel} />
          <SessionTable sessions={sessions} />
        </>
      )}
    </div>
  );
}

/** Project multi-select for the Accounts view — a compact dropdown with a search box, since
 *  the project count can reach the dozens and horizontal space is tight. `picked` is null for
 *  "All projects" (no filter); picking projects narrows every number in the section. */
function ProjectFilter({
  options,
  picked,
  onChange,
}: {
  options: { path: string; tokens: number }[];
  picked: Set<string> | null;
  onChange: (next: Set<string> | null) => void;
}) {
  const [open, setOpen] = useState(false);
  const [query, setQuery] = useState("");
  const boxRef = useRef<HTMLDivElement>(null);

  // Close on outside click / Escape — the selection stays applied either way.
  useEffect(() => {
    if (!open) return;
    const onPointer = (e: MouseEvent) => {
      if (boxRef.current && !boxRef.current.contains(e.target as Node)) setOpen(false);
    };
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") setOpen(false);
    };
    document.addEventListener("mousedown", onPointer);
    document.addEventListener("keydown", onKey);
    return () => {
      document.removeEventListener("mousedown", onPointer);
      document.removeEventListener("keydown", onKey);
    };
  }, [open]);

  const shown = useMemo(() => {
    const q = query.trim().toLowerCase();
    if (q === "") return options;
    return options.filter((o) => o.path.toLowerCase().includes(q));
  }, [options, query]);

  const active = picked !== null;
  const toggleOption = (path: string) => {
    const next = new Set(picked);
    if (next.has(path)) next.delete(path);
    else next.add(path);
    // Unchecking the last project is the same as choosing "All projects".
    onChange(next.size === 0 ? null : next);
  };

  return (
    <div className="projectFilter" ref={boxRef}>
      <button
        className={`projectFilterBtn ${active ? "active" : ""} ${open ? "open" : ""}`}
        onClick={() => setOpen((v) => !v)}
        aria-haspopup="true"
        aria-expanded={open}
      >
        <FolderKanban />
        {active ? `${picked.size} project${picked.size === 1 ? "" : "s"}` : "All projects"}
        <ChevronDown />
      </button>
      {open && (
        <div className="projectFilterMenu">
          <div className="projectFilterHead">
            <input
              type="text"
              autoFocus
              placeholder="Filter projects…"
              value={query}
              onChange={(e) => setQuery(e.target.value)}
            />
            {active && <button onClick={() => onChange(null)}>Clear</button>}
          </div>
          <label className="projectFilterOption projectFilterAll">
            <input type="checkbox" checked={!active} onChange={() => onChange(null)} />
            <span className="projectFilterName">All projects</span>
          </label>
          <div className="projectFilterList">
            {shown.map((o) => (
              <label key={o.path} className="projectFilterOption" title={o.path}>
                <input
                  type="checkbox"
                  checked={picked?.has(o.path) ?? false}
                  onChange={() => toggleOption(o.path)}
                />
                <span className="projectFilterName">{projectName(o.path)}</span>
                <span className="projectFilterTokens">{formatTokens(o.tokens)}</span>
              </label>
            ))}
            {shown.length === 0 && (
              <div className="projectFilterEmpty">No projects match "{query.trim()}".</div>
            )}
          </div>
        </div>
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
  const projects = mergeProjects(
    tools.flatMap((tool) =>
      tool.projects.map((project) => ({
        ...project,
        sessions: project.sessions.map((session) => ({
          ...session,
          id: `${tool.toolId}:${session.id}`,
          model: `${tool.displayName} / ${session.model}`,
        })),
      })),
    ),
  );
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

  const unpricedModels = Array.from(
    new Set(tools.flatMap((tool) => tool.unpricedModels ?? [])),
  ).sort();

  return {
    toolId: "all",
    displayName: "All",
    unpricedModels,
    estimate: tools.some((tool) => tool.estimate),
    total: sumTokens(tools.map((tool) => tool.total)),
    totalCostUsd: sumNullable(tools.map((tool) => tool.totalCostUsd)),
    today: sumTokens(tools.map((tool) => tool.today)),
    todayCostUsd: sumNullable(tools.map((tool) => tool.todayCostUsd)),
    daily,
    byModel,
    sessions,
    projects,
    accounts: [],
  };
}

function mergeProjects(projects: ProjectUsage[]): ProjectUsage[] {
  const byPath = new Map<string, ProjectUsage>();
  for (const project of projects) {
    const current = byPath.get(project.path);
    if (!current) {
      byPath.set(project.path, {
        ...project,
        tokens: { ...project.tokens },
        daily: [...project.daily],
        byModel: [...project.byModel],
        sessions: [...project.sessions],
      });
      continue;
    }
    current.tokens = addTokens(current.tokens, project.tokens);
    current.costUsd = sumNullable([current.costUsd, project.costUsd]);
    current.sessionCount += project.sessionCount;
    current.daily = mergeByDate([...current.daily, ...project.daily]);
    current.byModel = mergeByModel([...current.byModel, ...project.byModel]);
    current.sessions = [...current.sessions, ...project.sessions]
      .sort((a, b) => b.date.localeCompare(a.date) || total(b.tokens) - total(a.tokens))
      .slice(0, 30);
    if (project.lastActive > current.lastActive) current.lastActive = project.lastActive;
  }
  return Array.from(byPath.values()).sort(
    (a, b) => (b.costUsd ?? 0) - (a.costUsd ?? 0) || total(b.tokens) - total(a.tokens),
  );
}

function projectName(path: string) {
  const parts = path.replace(/\\/g, "/").split("/").filter(Boolean);
  return parts[parts.length - 1] ?? path;
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
