# AI Account Switcher

A native macOS app to manage and switch between **multiple accounts** for AI coding tools — **Claude Code**, **Codex**, and **Antigravity IDE** — from one place.

Built with [Tauri](https://tauri.app) (Rust + React).

> ⚠️ Using multiple subscription accounts may violate a provider's terms of service. This app only manages logins locally on your machine — use at your own discretion.

## ⬇️ Download

**[Download from store.xbirds.org/ai-account-switcher](https://store.xbirds.org/ai-account-switcher)** — free, universal (Apple Silicon + Intel), signed with a Developer ID and notarized by Apple. Open the `.dmg` and drag **AI Account Switcher** to Applications.

From 0.13.0 the app updates itself: when a new version is out, a card in the sidebar shows what's new and one click installs it.

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

1. Download the `.dmg` from [store.xbirds.org/ai-account-switcher](https://store.xbirds.org/ai-account-switcher).
2. Open the `.dmg` and drag **AI Account Switcher** to Applications.
3. Open the app. macOS asks once whether to open an app downloaded from the internet — click **Open**.

Since 0.13.0 the app is signed with a Developer ID and notarized by Apple, so no `xattr` or right-click → Open is needed, and it updates itself (Settings → Updates shows the version and checks by hand). Versions before 0.13.0 cannot update themselves: install 0.13.0 over them once. On the first launch after upgrading from an older, unsigned build, macOS may ask again whether the app may control Terminal (used to sign in) — allow it once.

## Build from source

Prerequisites: [Rust](https://rustup.rs), [Node.js](https://nodejs.org) 20+, and the Tauri macOS prerequisites (Xcode Command Line Tools).

```bash
npm install
npm run tauri dev      # run in development
npm run tauri build    # produce a .dmg in src-tauri/target/release/bundle/dmg
```

## Releasing

Releases are published from the release Mac straight to the store's server, like Reqwise Figma — not through GitHub. One command builds the universal app, signs it with the Developer ID certificate, notarizes it with Apple, signs the in-app update package with the updater key, uploads the installer, the update package and the manifest installed apps read, and checks them over https:

```bash
npm run publish-release            # checks + plan, changes nothing
npm run publish-release -- --yes   # publish
npm run publish-release -- --status
```

Before it: bump the version in `package.json`, `package-lock.json`, `src-tauri/tauri.conf.json`, `src-tauri/Cargo.toml` (and `src-tauri/Cargo.lock`), add the version to `CHANGELOG.md` and its short "what's new" lines (English + Vietnamese) to `release-notes.json`, and commit. The header of `scripts/publish.mjs` lists what goes where, the one-time setup (Apple identity, notarization profile, `ssh bahub`) and how to try an update on a test channel first.

**The updater key** (`~/.config/ai-switcher/updater.key`) signs every update package, and installed apps only accept packages signed by it. Keep a backup of it (password manager or an encrypted drive, not the repo or a cloud folder): without it, apps already installed can no longer update and everyone has to download the next version by hand.

See [CHANGELOG.md](CHANGELOG.md) for the per-version history and
[the v0.7.2 release notes](docs/releases/v0.7.2.md) for the current release.

## License

[MIT](LICENSE) © 2026 Hoang Phan.
