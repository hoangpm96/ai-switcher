# AI Account Switcher Project Memory

## Shape

- Desktop app: Tauri 2 + React/Vite.
- Frontend lives in `src/`; backend commands and account logic live in `src-tauri/src/`.
- App state is persisted by `src-tauri/src/store.rs` under the OS app-data dir from:
  `ProjectDirs::from("dev", "hoangphan", "AI Account Switcher")`.
- Local app-data layout:
  - `state.json`: accepted disclaimer, accounts, auto-switch settings.
  - `accounts/<tool>/<account-id>/`: per-account profile dirs.
  - `active/<tool>.profile`: selected profile path for the bare CLI command.
  - `usage.json` and `litellm_prices.json`: token/cost caches.

## Core Backend Files

- `models.rs`: shared DTOs/enums for Tauri commands and React types.
- `store.rs`: persisted state paths and helpers.
- `tools.rs`: CLI detection, profile login, symlinks, launchers, shell hook, delete cleanup.
- `app_state.rs`: high-level account workflows: snapshot, add, switch, delete, refresh, auto-switch.
- `quota.rs`: quota readers for Claude, Codex, Cursor CLI, opencode Zen, Antigravity.
- `overlay.rs`: the always-on-top quota overlay window (second frameless window, label `overlay`).
- `usage.rs`: scans Claude/Codex JSONL logs and builds the Usage tab report.
- `pricing.rs`: LiteLLM price cache and model-price lookup.

## Account Model

- Claude/Codex machine default accounts point at `~/.claude` / `~/.codex`.
- Cursor CLI and opencode are managed the same way, with their own isolation mechanism:
  - opencode: profile dir is an `XDG_DATA_HOME`; credentials land in `<profile>/opencode/auth.json`.
  - Cursor: no config-dir variable exists. Login runs with `HOME=<profile>` +
    `AGENT_CLI_CREDENTIAL_STORE=file`, which writes `<profile>/.cursor/auth.json`
    (`accessToken` + `refreshToken`). Launchers must NOT move HOME (the agent runs the user's own
    shell commands): they export `CURSOR_AUTH_TOKEN` from that file with
    `AGENT_CLI_CREDENTIAL_STORE=memory` so the run can't touch the login keychain.
  - Neither supports API/proxy accounts, the API gateway pool, token-usage scanning, or
    auto-switch (no 5-hour window to react to).
- Additional Claude/Codex accounts are profile dirs under app data and are selected by exporting:
  - `CLAUDE_CONFIG_DIR=<profile>`
  - `CODEX_HOME=<profile>`
- The app does not wrap the real `claude`/`codex` binaries. It installs an idempotent shell hook in `~/.zshrc` and `~/.bashrc` if present.
- Per-account launcher commands are separate files in `~/.local/bin`, e.g. `claude-work`, `codex-pro`,
  `cursor-agent-work`, `opencode-alt`.
- The bare `cursor-agent` / `opencode` commands follow the selected account via shell FUNCTIONS in
  the hook (not exported variables): `XDG_DATA_HOME` and the Cursor token must stay scoped to that
  one invocation.
- Antigravity does not use profile env vars. It copy-swaps OAuth/profile keys inside the default IDE `state.vscdb`.

## Shared Config Rule

- Credentials must stay per account:
  - Claude OAuth is in macOS Keychain keyed by the profile-dir hash.
  - Codex OAuth is `auth.json` inside the profile.
  - API/proxy accounts use `api_key`, `config.toml`, or `settings.json` per account.
- Normal OAuth Claude/Codex accounts share user config/memory by symlinking profile entries back to default config:
  - Claude: `.claude.json`, `settings*.json`, `plugins`, `rules`, `commands`, `agents`.
  - Codex: `config.toml`, `rules`, `skills`, Codex memory/goals files.
- Session/history sharing is separate and always links back to the default config:
  - Claude: `projects`, `history.jsonl`.
  - Codex: `sessions`, `history.jsonl`.
- Do not apply shared config symlinks to API/proxy accounts, because their gateway/model/key config is intentionally account-specific.

## Frontend Notes

- `src/App.tsx` is the main UI: tool tabs, account cards, modals, auto-switch settings.
- `src/UsageView.tsx` renders token/cost usage.
- `src/OverlayApp.tsx` + `src/overlay.css` render the floating quota overlay; `src/main.tsx` picks it by window label.
- `src/tauri.ts` wraps invoke calls and contains mock data for browser/dev fallback.
- `src/types.ts` mirrors Rust DTOs.

## Build/Test

- Frontend build: `npm run build`.
- Rust tests/checks: run inside `src-tauri`, e.g. `cargo test` or `cargo check`.
- Full dev app: `npm run tauri dev`.

## Guardrails

- Keep edits scoped; avoid changing shell hook semantics unless account switching requires it.
- Do not delete user profile dirs or CLI config unless the user explicitly asks.
- When changing shared-profile behavior, preserve credential isolation and skip API/proxy accounts.
- Prefer idempotent repair/migration during startup (`ManagedState::heal_active_profiles`) so existing accounts self-heal.
