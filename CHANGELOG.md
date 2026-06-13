# Changelog

All notable changes to **AI Account Switcher** are documented here.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

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

[0.2.0]: https://github.com/hoangpm96/ai-switcher/releases/tag/v0.2.0
[0.1.0]: https://github.com/hoangpm96/ai-switcher/releases/tag/v0.1.0
