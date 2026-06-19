# Changelog

All notable changes to **AI Account Switcher** are documented here.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

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

[0.4.0]: https://github.com/hoangpm96/ai-switcher/releases/tag/v0.4.0
[0.3.0]: https://github.com/hoangpm96/ai-switcher/releases/tag/v0.3.0
[0.2.0]: https://github.com/hoangpm96/ai-switcher/releases/tag/v0.2.0
[0.1.0]: https://github.com/hoangpm96/ai-switcher/releases/tag/v0.1.0
