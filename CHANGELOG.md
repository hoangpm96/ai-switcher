# Changelog

All notable changes to **AI Account Switcher** are documented here.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

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

[0.3.0]: https://github.com/hoangpm96/ai-switcher/releases/tag/v0.3.0
[0.2.0]: https://github.com/hoangpm96/ai-switcher/releases/tag/v0.2.0
[0.1.0]: https://github.com/hoangpm96/ai-switcher/releases/tag/v0.1.0
