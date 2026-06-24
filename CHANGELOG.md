# Changelog

All notable changes to **AI Account Switcher** are documented here.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [0.5.14] - 2026-06-24

### Added

- **"Clean up old account data" in the Auto Session tab.** Lists leftover profile folders from
  accounts you've removed, with each folder's size and an "in use" warning when it was modified
  recently (likely a running CLI session). You delete each one explicitly — nothing is removed
  automatically.

### Fixed

- **Stopped automatically deleting leftover account folders on startup.** A folder that isn't in the
  app's account list can still be a live profile another CLI session uses directly, so silently
  removing it could destroy that session's data. Cleanup is now opt-in via the action above.
- **Preserve data when linking an account into the shared store.** Turning an account's real
  transcript/memory folder into a shared link now moves it aside first and only discards it once
  every file is confirmed present in the shared store — closing a window where a file could be lost
  to a concurrent write or a name collision.

## [0.5.13] - 2026-06-24

### Fixed

- **No more "Please run /login" (401) when switching Claude accounts.** Reading an account's quota
  no longer refreshes its OAuth token. Since v0.5.12 the app refreshed near-expiry tokens itself and
  wrote the rotated credential back to the keychain — but OAuth refresh tokens are single-use, so
  rotating one invalidated the token a live Claude Code session on that account still held, forcing a
  re-login (the account's stored credential could even be left with an empty refresh token). The app
  is now strictly read-only for tokens: it reads the current access token and never refreshes,
  rotates, or writes credentials. Claude Code owns the token lifecycle, so switching accounts can no
  longer log a session out. The same read-only change applies to Codex (whose ~8-day tokens made the
  bug rare but possible). Trade-off: an account left unused past its access-token lifetime shows its
  quota as unavailable until you next open its CLI, which renews the token conflict-free.
- **"Prime now" returns instantly instead of spinning for up to ~2.5 minutes.** The button used to
  block while the whole send-and-confirm ran (the confirmation polling alone could sleep that long).
  It now starts the prime in the background, returns immediately, and reports the final result via a
  notification when it completes.
- **Claude prime confirmation is faster and more reliable.** Confirmation now treats the usage
  endpoint's active-session signal (a window that just transitioned to active) as proof the window
  opened, instead of waiting for the reset timestamp to change — which the provider can take many
  minutes to report, occasionally causing a scheduled prime to miss its deadline. It reads first and
  only sleeps if not yet confirmed, so the common "opened instantly" case returns in about a second.
  Priming an account whose window is still running correctly reports the existing window rather than a
  false new one.

### Changed

- **Newly added accounts share memory and chat history immediately.** The shared-store link for an
  account's transcripts/memory is now ensured on every refresh, so an account that was created but
  never switched into no longer keeps its memory to itself until the next switch, login, or restart.

## [0.5.12] - 2026-06-23

### Fixed

- **Scheduled and manual Codex primes now confirm reliably.** A bare "hi" anchors the Codex 5-hour
  window, but the freshly anchored reset sits at roughly "now + 5h" for the first ~90 seconds, so the
  previous two-reads-75s-apart drift check (and the later "reset clearly inside the window" check)
  often reported failure even though the session had opened. Confirmation now polls the live window
  and succeeds as soon as the reset epoch is observed **unchanged across two reads** (a fixed,
  anchored reset stays put, while a rolling placeholder advances with the clock) — typically within
  15–30 seconds — tolerating a slow anchor or a transient read error. The poll is bounded by both a
  poll count and a hard wall-clock budget so a scheduler tick is never cut off mid-confirmation.
- **Claude OAuth token refresh now happens over HTTP instead of launching the Claude CLI.** Reading a
  Claude account's quota when its token was near expiry previously spawned `claude` to refresh,
  which made macOS attribute the CLI's protected-folder preflight (Desktop/Documents/Downloads) to
  AI Account Switcher and pop a permission prompt. The refresh now uses the stored refresh token
  directly and writes the rotated credential back through the native keychain API — no subprocess,
  no folder prompt. Concurrent refreshes for the same account are serialized, and the credential
  write is retried to avoid stranding an account on a transient keychain error.

## [0.5.11] - 2026-06-23

### Fixed

- **Claude manual and scheduled primes now send directly over HTTP.** Prime no longer starts the
  Claude Code CLI just to send the minimal background request, avoiding macOS Apple Music/Media
  Library permission prompts caused by Claude's sandbox/tool preflights. The existing scheduler,
  pmset wake helper, retry, hold/defer, and reset-confirmation flow are unchanged.

## [0.5.10] - 2026-06-22

### Fixed

- **Codex “Prime ngay” now runs successfully outside a Git repository.** The app previously
  launched `codex exec hi` from `/`, so Codex rejected the request with exit 1 after the v0.5.9 PATH
  fix. Background Codex primes now use an ephemeral, read-only automation invocation with the
  account profile as the working directory and `--skip-git-repo-check`.
- **Background Claude prime/token refresh no longer scans user projects or triggers protected-folder
  permission prompts.** The isolated invocation disables project, plugin, hook, and MCP
  customizations, avoids session persistence, and runs inside the account profile instead of
  loading shared paths that may point at Desktop, Documents, Downloads, mounted volumes, or other
  macOS-protected locations.
- **CLI prime failures now include a concise stderr reason.** Future failures report the actual
  Codex/Claude error instead of only an opaque numeric exit code.
- **Claude quota now self-recovers from stale OAuth/Keychain state reported as HTTP 401 or 429.**
  The app runs one isolated Claude CLI refresh, reloads the persisted token, and retries the usage
  endpoint once—the same recovery users observed after opening `claude` manually.
- **Recovery is bounded per account.** Each Claude profile can trigger at most one recovery within
  five minutes; network errors and server 5xx responses do not invoke the CLI, preventing retry
  loops and excess requests.
- **Auto Session documentation now matches confirmation semantics.** A successful CLI/HTTP send is
  only a pending request; the app reports an opened session only after the provider reset state is
  verified. Locked login Keychains and unknown quota states remain retryable/fail-closed rather
  than being presented as successful primes.
- **Claude background requests no longer trigger macOS protected-folder prompts.** Safe mode alone
  still initialized Claude's built-in tool/sandbox layer. Prime and OAuth recovery now explicitly
  disable tools, settings sources, MCP, Chrome integration, slash commands, and prompt suggestions,
  keeping the invocation API-only while preserving OAuth refresh.
- **Codex prime no longer starts the Codex agent runtime or Git discovery.** Manual and scheduled
  primes now call Codex's response endpoint directly with the selected profile's OAuth token, then
  verify the live quota reset. This avoids Git/sandbox preflights for Desktop, Documents, Downloads,
  Media Library, and other protected locations.

## [0.5.9] - 2026-06-22

### Fixed

- **Codex manual and scheduled primes no longer fail with `CLI exit 127`.** GUI apps and the
  user-scoped LaunchDaemon now give CLI subprocesses a deterministic PATH containing common Node,
  Homebrew, npm, pnpm, Bun, Cargo, and system binary locations. This allows npm-installed Codex
  scripts using `#!/usr/bin/env node` to run while preserving each account's isolated `CODEX_HOME`.
- **Claude now shows “Prime ngay” when no five-hour session is active.** A successful Claude usage
  response with `resets_at: null` is treated as no anchored session even when utilization is
  non-zero or omitted; read/authentication errors remain unknown and fail closed.
- **Pending prime copy no longer claims a session is already opening.** The UI says the prime
  request was sent and is awaiting confirmation, and only reports an opened session after a new
  reset has been verified.

## [0.5.8] - 2026-06-21

### Fixed

- **Auto Session now verifies and retries scheduled primes before declaring success.** Codex primes
  require a stable fixed-reset proof, Claude primes require the reset window to move from the
  baseline, and both keep retrying within the bounded catch-up/deadline window instead of marking a
  send-only attempt as successful.
- **Prime attempts now finish crash-safely and log their source clearly.** Terminal state is
  finalized atomically across `prime-runtime.json`, `state.json`, and the activity log, so a crash
  cannot silently lose or duplicate a result. Activity log lines now include the trigger source
  (`SCHEDULE`, `AUTO-EXTEND`, `EXTEND`, or `MANUAL`) plus an attempt id, while terminal result
  lines use a separate marker so they are not hidden by earlier START/PENDING entries.

## [0.5.7] - 2026-06-20

### Added

- **Auto Session can now send due prime requests after a sleeping Mac wakes.** The pmset helper still
  schedules the wake, and a separate user-scoped LaunchDaemon runs the app in `--prime-headless`
  mode once per minute while awake. It runs under the current macOS user/profile, sets `HOME`/`USER`
  explicitly, and uses the app's own per-account config instead of root's `/var/root` state.

### Fixed

- **Headless and GUI priming are serialized across processes.** A file lock plus per-slot claim
  markers prevent the GUI scheduler and the headless daemon from sending the same scheduled/extend
  prime twice, even if one process still has stale in-memory state.
- **Locked Keychain is treated as retryable.** If Claude/Codex tokens cannot be read while the Mac
  is locked, the prime claim is released so later ticks can try again after the user unlocks. The UI
  now says it sends a prime request instead of guaranteeing a new session opened.

## [0.5.6] - 2026-06-20

### Fixed

- **Auto Session no longer logs "HOÃN" through the night or misses the morning anchor.** A held
  scheduled prime (the old 5h window was still live) used to re-attempt on every wake, re-logging
  "HOÃN", and planted a defer at the old window's end (~midnight) that stole the next morning's
  wake — so the 06:30 anchor never fired. A scheduled Hold now consumes the day's slot (one-shot,
  like a success) and does NOT arm a defer; only an armed extend defers-and-retries.
- **A daily anchor is only "due" within a bounded catch-up window.** The due test was a string
  compare (`now >= time`) that treated every minute after the anchor until midnight as due, so a
  06:30 anchor stayed due all evening and a late-waking machine fired a pointless prime into the
  live window. It now primes only within 60 minutes past the anchor, otherwise waits for tomorrow.
- **Setting a time in the evening schedules the next morning, not an immediate catch-up.** A new
  `activeFrom` date defers a freshly created/edited (or just-enabled) schedule to its next
  occurrence when today's anchor already passed beyond the catch-up window.
- **"Prime ngay" now shows correctly for Codex and ended Claude windows.** The button never
  appeared for Codex (its `/wham/usage` reset_at rolls forward until a request anchors the window)
  and stayed hidden for ended Claude windows (a null reset_at was read as "unknown → hide"). The
  backend now classifies the window and emits a provider-aware `primeAvailable` flag the UI reads,
  failing closed on a genuinely unknown state.

## [0.5.5] - 2026-06-18

### Fixed

- **Activity log no longer spams on an unrefreshable token.** A repeating skip/hold with an
  unchanged result (e.g. an expired token that can't refresh while offline) was appended to
  the log every minute. It now logs only on a change, a manual attempt, or a terminal outcome.
- **A manual "Prime ngay" (or a one-time extend) no longer cancels the daily scheduled prime.**
  Only a genuinely scheduled run now marks the day's slot consumed, so a manual prime at 06:00
  doesn't suppress the 11:00 daily anchor.
- **Changing an account's daily time no longer disarms an active auto-extend.** The pending
  defer that belongs to an armed extend is preserved across a schedule-time change.
- **The Mac wake is no longer dropped near a prime time.** A wake instant already in the past
  (e.g. computed at 10:55 for an 11:00 prime with a 10-minute lead) is rolled to the next day
  instead of erasing every account's wake for that recompute.
- **"Prime ngay" shows a neutral (blue) info toast when the window is still running** — not a
  green success toast — so a hold no longer looks like something was opened.

## [0.5.4] - 2026-06-18

### Fixed

- **"Out of quota" notification no longer repeats every few minutes.** It fired on every
  quota refresh while an account stayed exhausted, so the 5-minute poller re-notified for
  the same window over and over. It now fires only once — on the transition into exhausted —
  and again only if the account recovers and is exhausted anew.

## [0.5.3] - 2026-06-18

### Fixed

- **Expired Claude tokens now actually refresh — fixes recurring HTTP 401 on quota reads.**
  When a Claude OAuth token was within 5 minutes of expiry, the app ran `claude auth status`
  to refresh it — but that command only *reports* the login (it returns `loggedIn: true`
  without touching the token), so the expired token stayed in place and the next usage call
  returned 401. The app now runs a minimal `claude -p hi --max-turns 1`, which hits the API
  and makes the CLI refresh its OAuth token and write the new one back to the keychain.

## [0.5.2] - 2026-06-18

### Fixed

- **Notifications show a readable local time, not a raw UTC string.** The "Out of quota"
  notification printed the reset as `…T11:20:00+00:00`, which reads as 11:20 and gets
  mistaken for a morning time when it is actually 18:20 in a +7 zone. It now shows the
  local time with an explicit offset label, e.g. `resets at 18:20 (UTC+7)`. (This was not
  a wrong-timestamp bug — the instant was always correct, just displayed in UTC.)

## [0.5.1] - 2026-06-18

### Fixed

- **Codex quota now reads the live number for every account, including "Default (máy)".**
  The default account read its 5-hour usage from the local Codex rollout file, which only
  updates when the CLI runs — so it could show a months-old percentage that Refresh never
  changed, while a profile account on the same login showed the live number. All accounts
  now read the per-account usage endpoint first (live, correct per token); the rollout file
  is only a fallback if the endpoint is unreachable.
- **"Prime ngay" no longer shows a red error when the window is simply still running.**
  A hold (the current window hasn't ended yet, e.g. because another account on the same
  login just refreshed it) is reported as a neutral info toast, not a failure.

## [0.5.0] - 2026-06-18

### Added

- **"Prime ngay" — open a new 5-hour window on demand.** When an account's window has
  ended, a button on the Claude/Codex card opens the next window right away, so you
  don't have to drop to a terminal and send a message by hand. A toast reports the
  result (opened / old window still running / token expired / send failed). A manual
  attempt is a single fast send (no long retry), and a manual failure never consumes
  that day's scheduled prime.

### Fixed

- **Auto-extend no longer fires too early.** Previously, when a window had ≤30 minutes
  left, auto-extend armed immediately and the scheduler retried every minute — the old
  window was still running, so each attempt only logged "HOÃN", filling the log and
  often failing right at the boundary. Auto-extend now defers to just after the window
  actually ends (a small grace absorbs provider clock skew), priming once, cleanly.
- **Mac self-wake survives the gap before a prime.** The wake lead grew from 5 to 10
  minutes, and the app now holds the Mac awake (`caffeinate`) both while a prime runs
  and across the wait between the pmset wake and a deferred prime — so the machine no
  longer idle-sleeps before the window-ending it woke up for.
- **Disabling / toggling off auto-extend cancels any already-armed extend**, and the
  next wake is recomputed, so the Mac doesn't wake for a prime that's no longer going
  to happen. A scheduled defer left on a disabled account is also cleared.
- **Token refresh before quota reads now resolves the CLI's full path**, so a GUI launch
  with a minimal PATH can still refresh an expired Claude token instead of silently
  skipping it and 401-ing.

### Changed

- **Calmer extend UI.** The window-ending prompt is now one quiet inline line
  ("Phiên còn 20′ · Gia hạn") instead of an orange two-button banner on every card,
  and several accounts ending at once collapse into a single notification.

## [0.4.0] - 2026-06-16

### Added

- **Auto Session — anchor your 5-hour reset to your work rhythm.** A new tab that
  sends a minimal "hi" to a Claude/Codex subscription account at a time you choose,
  so a fresh 5-hour window opens before you start coding instead of mid-session.
  Each account has one daily prime time and primes at most once per day.
- **Mac self-wake (pmset).** Optionally install a one-time privileged helper so the
  Mac wakes itself ~5 minutes before a prime, runs it, and goes back to sleep — no
  need to keep the machine on. Asks for admin once at install; falls back to
  "prime when the machine is awake / app is open" when not installed.
- **On-demand extend.** When a 5-hour window is about to end (≤30 minutes), the app
  notifies and shows an "extend?" button on the account so you can open the next
  window the moment the current one ends and keep coding without waiting. It asks
  first by default; an optional per-account "auto-extend" toggle does it silently.
- **Prime priming via the account's own CLI.** Priming runs `claude -p` / `codex exec`
  with the account's config dir so the CLI refreshes its own token and uses the
  provider's exact endpoint — robust against token expiry (the 401 that bare token
  calls hit). A direct HTTP call remains a fallback.
- **Activity log + stats.** Every prime (success, hold, skip, fail, late) is logged
  with the exact reason; a per-day stats table summarizes outcomes, and the log file
  can be opened or revealed from the tab.
- **Live status on the account.** Subscription Claude/Codex accounts show their
  auto-prime state, anchor time, and an estimated next reset; the displayed quota
  refreshes immediately after a successful prime.

### Fixed

- Quota reads now refresh an expired access token before calling the usage endpoint
  for both Claude and Codex, fixing intermittent 401s.

## [0.3.0] - 2026-06-15

### Added

- **Local API gateway.** Run a local OpenAI/Anthropic-compatible server for Claude
  Code and Codex subscription accounts, with `/v1/models`, `/v1/messages`,
  `/v1/chat/completions`, `/v1/responses`, and Codex direct proxy endpoints.
- **9router-style combos.** Define named model combos with ordered fallback members
  and per-combo rotation strategy, then expose each combo as a gateway model.
- **Gateway API keys.** Create, reveal, copy, enable/disable, and delete local
  gateway keys from the app. Keys are masked in snapshots and persisted locally.
- **Subscription account rotation.** Toggle which Claude/Codex subscription
  accounts may serve gateway traffic, rotate with round-robin or fill-first, honor
  quota thresholds, and cool accounts down when upstreams ask clients to retry
  later.
- **Virtual CLI accounts.** Create local Claude/Codex launcher accounts that point
  at the local gateway and pin either a combo or a single discovered model.
- **Gateway usage report.** Track real token usage for streaming gateway requests
  by combo, key, account, and tool.

### Changed

- API/proxy account creation now uses a modal, fetches gateway models before
  choosing a model, supports Claude Code as well as Codex, and can optionally add
  Codex bypass flags to generated launchers.
- Notifications now use a unified top-right toast, and API tab controls use the
  app's current toggle/status/button styling.
- The release workflow now publishes GitHub Releases immediately when a version
  tag is pushed, instead of leaving them as drafts.

### Fixed

- Gateway forwarding now passes through upstream errors, retry headers, and retry
  cooldowns instead of hiding them behind fallback responses.
- Anthropic-to-Codex and OpenAI Responses translations now strip incompatible
  client-only fields, set required Codex `/responses` fields, preserve usable SSE
  envelopes, flatten chat tools, and drop nameless tools rejected by Anthropic.
- Gateway startup is non-blocking and no longer hangs on model discovery.
- Clipboard copy actions use the real Tauri clipboard plugin when running in the
  desktop app.

## [0.2.0] - 2026-06-14

### Added

- **Menu-bar tray.** A macOS menu-bar icon with a native menu to quick-switch
  Claude Code / Codex accounts without opening the window. The active account is
  marked with a checkmark and each row shows its quota % and plan. Includes a
  custom monochrome (template) tray icon that adapts to light/dark menu bars.
- **Close to tray.** Closing the main window hides it to the tray instead of
  quitting; the background quota poller keeps running. Reopen from the tray or by
  clicking the Dock icon. Quit from the tray menu.
- **Subscription plan badges.** Reads the plan (Plus / Pro / Max) from each tool's
  usage API and shows it next to the account name (and in the tray menu).
- **Per-tool auto-switch.** Claude and Codex now have independent auto-switch
  settings (enable + threshold) instead of one global toggle. Existing settings
  are migrated automatically.
- **Usage "All" tab.** An aggregated view that merges token usage and estimated
  cost across all tools, alongside the per-tool tabs.

### Changed

- **Tidier account cards.** Compact single-line quota rows with an inline reset
  time, the launcher command shown as a small inline chip next to the name, and a
  hidden profile fingerprint. Reduced padding and spacing.
- **Modernised borders.** Hairline borders with a subtle top edge-highlight and
  consistent design tokens (replacing ad-hoc border colours) across cards, panels,
  inputs, buttons and the auto-switch controls.

### Fixed

- The window could not be moved because the drag region had no permission. Granted
  `core:window:allow-start-dragging` so the title-bar area drags the window again.

## [0.1.0] - 2026-06-02

### Added

- Initial release. Manage and switch between multiple accounts for **Claude Code**,
  **Codex** and **Antigravity IDE** from one macOS app.
- Per-account isolated config dirs and dedicated `claude-<name>` / `codex-<name>`
  commands to run accounts in parallel; the bare `claude` / `codex` command follows
  the selected account (with an `aisw` sync helper).
- Antigravity login-token switching (quit/reopen the IDE around each swap), accounts
  identified by Google avatar with duplicate detection.
- Quota reading (5-hour / weekly for Claude & Codex, per-model for Antigravity) and a
  token usage + cost tab.
- API / proxy gateway accounts for Claude Code and Codex.
- Universal macOS `.dmg` release via GitHub Actions.

[0.5.11]: https://github.com/hoangpm96/ai-switcher/releases/tag/v0.5.11
[0.5.10]: https://github.com/hoangpm96/ai-switcher/releases/tag/v0.5.10
[0.5.9]: https://github.com/hoangpm96/ai-switcher/releases/tag/v0.5.9
[0.5.8]: https://github.com/hoangpm96/ai-switcher/releases/tag/v0.5.8
[0.5.7]: https://github.com/hoangpm96/ai-switcher/releases/tag/v0.5.7
[0.5.6]: https://github.com/hoangpm96/ai-switcher/releases/tag/v0.5.6
[0.5.5]: https://github.com/hoangpm96/ai-switcher/releases/tag/v0.5.5
[0.5.4]: https://github.com/hoangpm96/ai-switcher/releases/tag/v0.5.4
[0.5.3]: https://github.com/hoangpm96/ai-switcher/releases/tag/v0.5.3
[0.5.2]: https://github.com/hoangpm96/ai-switcher/releases/tag/v0.5.2
[0.5.1]: https://github.com/hoangpm96/ai-switcher/releases/tag/v0.5.1
[0.5.0]: https://github.com/hoangpm96/ai-switcher/releases/tag/v0.5.0
[0.4.0]: https://github.com/hoangpm96/ai-switcher/releases/tag/v0.4.0
[0.3.0]: https://github.com/hoangpm96/ai-switcher/releases/tag/v0.3.0
[0.2.0]: https://github.com/hoangpm96/ai-switcher/releases/tag/v0.2.0
[0.1.0]: https://github.com/hoangpm96/ai-switcher/releases/tag/v0.1.0
