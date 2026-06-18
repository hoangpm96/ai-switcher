# Changelog

All notable changes to **AI Account Switcher** are documented here.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

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
