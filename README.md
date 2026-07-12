# AI Account Switcher

A native macOS app to manage and switch between **multiple accounts** for AI coding tools — **Claude Code**, **Codex**, and **Antigravity IDE** — from one place.

Built with [Tauri](https://tauri.app) (Rust + React).

> ⚠️ Using multiple subscription accounts may violate a provider's terms of service. This app only manages logins locally on your machine — use at your own discretion.

## ⬇️ Download

Get the latest **`.dmg`** from the [**Releases**](https://github.com/hoangpm96/ai-switcher/releases/latest) page — download the `.dmg` under **Assets**, open it, and drag **AI Account Switcher** to Applications.

> First launch only: the app is unsigned, so right-click it → **Open** (or run `xattr -cr "/Applications/AI Account Switcher.app"`). See [Install](#install) below.

## Features

- **One window for every tool.** Log in, switch, rename, and remove accounts per tool.
- **Menu-bar quick switch.** A tray icon in the macOS menu bar lists your Claude & Codex accounts (with quota % and plan) so you can switch without opening the window. Closing the window hides the app to the tray; it keeps polling quota in the background.
- **Quota at a glance.** Reads 5-hour / weekly usage for Claude & Codex and per-model quota for Antigravity, shows your **subscription plan** (Plus / Pro / Max) when the API reports it, and exposes Codex usage-limit reset credits in a per-account modal.
- **Per-tool auto-switch.** Configure separately for Claude and Codex — the bare command falls back to another account when the active one nears its quota.
- **Usage & cost tab.** Token usage and estimated cost per tool, plus an aggregated **All** view across tools, charted over a selectable date range.
- **Local API gateway.** Expose Claude/Codex subscription accounts through a local OpenAI/Anthropic-compatible server with API keys, model combos, fallback rotation, cooldown handling, and gateway usage tracking.
- **On-demand session prime.** A **Prime ngay** button on each Claude/Codex account opens a fresh 5-hour window when you want one, using the account's existing token over plain HTTP — it never spawns a CLI or rotates a token, so it can't disturb a running session.

### Claude Code & Codex (CLI)

- Each account logs into its own isolated config dir and gets a **dedicated command** (`claude-<name>`, `codex-<name>`) so you can run several accounts in parallel across terminals.
- The bare `claude` / `codex` command **follows the account you select** (via a shell hook + an "active profile" file). Run `aisw` in an already-open terminal to sync it to the latest selection.
- Chat sessions are **shared across accounts** in the same project, so you can resume work regardless of which account created it.
- API/proxy accounts can point Claude Code or Codex at an external gateway, with one pinned model per generated launcher.

### Local API Gateway

- Start a local server on `127.0.0.1:8783` by default and call it with OpenAI or Anthropic-compatible clients.
- Create local gateway API keys, enable the subscription accounts that may serve requests, and define named **combos** that resolve to ordered model fallbacks.
- Create virtual Claude/Codex CLI accounts that point at the local gateway and pin a combo or single discovered model.
- Gateway usage is tracked separately by combo, key, account, and tool.

### Session prime

> Earlier versions offered a scheduled "Auto Session" prime (a daily prime time, extend reminders, and macOS wake daemons). That has been **removed in 0.7.0** — every background/scheduled prime path could disturb a running CLI session (a background token refresh could log an interactive `claude` session out and force a manual `/login`). Only the manual, on-demand button remains.

- **Prime ngay (on demand).** When the provider reports that an account has no active five-hour window, the card shows a manual prime button. Clicking it sends one minimal request directly over HTTP using the account's existing OAuth token — it never starts the Claude/Codex agent runtime, never spawns a CLI, and never rotates a token, so it cannot trigger macOS protected-folder prompts or disturb a running session. It runs a single attempt (send once, then confirm briefly); if confirmation doesn't land in time, press it again. The UI first says the request was sent and is awaiting confirmation, and reports an opened session only after the reset state is verified.
- Quota for every account (including the machine default) is read live from the provider, so the displayed usage and reset time stay current and a refresh always reflects the real state.
- **Codex usage-limit reset credits.** Codex account cards show a small reset-credit icon in the bottom action row when the provider reports reset-credit data. Click it to see how many resets remain and, when available, each reset credit's expiry time. Accounts with no available credits still show `0 available`; API/proxy accounts do not expose this provider quota data.
- **Upgrading from an older version?** If you had the scheduled-prime wake daemons installed, open **Settings** — when leftover daemons are detected you'll see a one-tap **Gỡ daemon cũ** button that removes them (one admin prompt). The app also clears any stale wake schedule on first launch.

#### Prime and quota troubleshooting

- **Claude quota returns HTTP 401 or 403:** the stored OAuth token has expired. The app does **not** refresh it — refreshing (even via the `claude` CLI) can evict a live terminal session. Open that account's `claude` CLI and log in again; the app's **Làm mới token** button only re-reads the token (it clears a transient 401 if the token is actually still valid).
- **Prime reports a send error:** the prime sends directly over HTTP for Claude/Codex. If a send fails, confirm the profile is logged in and that quota can be refreshed.
- **Codex prime takes a little while to confirm:** sending the prime anchors the Codex five-hour window, but the freshly anchored reset reads close to "now + 5h" for the first minute or so. The app confirms by watching the reset settle to a fixed value (typically within 15–30 seconds) rather than assuming success, so a manual prime may show "awaiting confirmation" briefly before reporting the opened session. If it runs out of time, it says the request was sent but not yet confirmed — wait a moment, refresh quota, and prime again if needed.
- **No Prime ngay button:** the button appears only when quota was read successfully and the provider reports no active five-hour window. Authentication, network, or unknown quota state fails closed rather than offering a prime the app cannot safely verify.

### Antigravity IDE (GUI)

- Switching swaps the saved login token in the IDE's `state.vscdb`. The app quits and reopens the IDE around each swap so the token isn't clobbered.
- **Sign in new account** puts the IDE at a logged-out screen so you can add an account that was never signed in.
- Accounts are identified by their **Google avatar**, and duplicates of the same account are detected automatically.

## Install

1. Download the latest `.dmg` from the [Releases](https://github.com/hoangpm96/ai-switcher/releases/latest) page.
2. Open the `.dmg` and drag **AI Account Switcher** to Applications.

The app is **not code-signed** (no paid Apple Developer account), so macOS Gatekeeper will warn on first launch. To open it:

- **Right-click** the app in Applications → **Open** → **Open** in the dialog, **or**
- Run once in Terminal:

  ```bash
  xattr -cr "/Applications/AI Account Switcher.app"
  ```

You only need to do this the first time.

## Build from source

Prerequisites: [Rust](https://rustup.rs), [Node.js](https://nodejs.org) 20+, and the Tauri macOS prerequisites (Xcode Command Line Tools).

```bash
npm install
npm run tauri dev      # run in development
npm run tauri build    # produce a .dmg in src-tauri/target/release/bundle/dmg
```

## Releasing

Pushing a version tag like `v0.6.3` triggers the GitHub Actions workflow (`.github/workflows/release.yml`), which builds a universal macOS `.dmg` and publishes a GitHub Release with the artifact attached. Bump the version in `package.json`, `package-lock.json`, `src-tauri/tauri.conf.json`, `src-tauri/Cargo.toml` and `src-tauri/Cargo.lock` first, then:

```bash
git tag v0.6.3
git push origin main v0.6.3
```

See [CHANGELOG.md](CHANGELOG.md) for the per-version history and
[the v0.7.2 release notes](docs/releases/v0.7.2.md) for the current release.

## License

No license file yet — add one (e.g. MIT) before sharing widely if you want to allow reuse.
