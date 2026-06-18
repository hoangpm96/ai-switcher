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
- **Quota at a glance.** Reads 5-hour / weekly usage for Claude & Codex and per-model quota for Antigravity, and shows your **subscription plan** (Plus / Pro / Max) when the API reports it.
- **Per-tool auto-switch.** Configure separately for Claude and Codex — the bare command falls back to another account when the active one nears its quota.
- **Usage & cost tab.** Token usage and estimated cost per tool, plus an aggregated **All** view across tools, charted over a selectable date range.
- **Local API gateway.** Expose Claude/Codex subscription accounts through a local OpenAI/Anthropic-compatible server with API keys, model combos, fallback rotation, cooldown handling, and gateway usage tracking.
- **Auto Session.** Anchor each Claude/Codex account's 5-hour reset to your work rhythm — the app primes a fresh window at a time you pick (optionally waking the Mac itself), can open the next window the moment the current one ends so you keep coding without waiting, and gives you a **Prime ngay** button to open a new window on demand.

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

### Auto Session

- Give each Claude/Codex subscription account **one daily prime time**; the app sends a minimal "hi" then to open a fresh 5-hour window, so your reset clock lands when you actually start coding. Primes at most once per day per account.
- Priming runs the account's own `claude` / `codex` CLI (so it refreshes its own token), with a direct HTTP fallback. Each attempt — success, hold, skip, fail, or late catch-up — is written to an activity log, with a per-day stats summary.
- Optionally install a **one-time privileged helper** so the Mac wakes itself ~10 minutes before a prime via `pmset`, stays awake (`caffeinate`) through the prime, and sleeps again; without it, priming runs whenever the machine is awake / the app is open (a missed time is caught up on next launch).
- **Prime ngay (on demand).** When an account's window has ended, a button on the card opens the next 5-hour window right away — no need to drop to a terminal. It reports back whether a new window opened, the current one is still running, or the token needs a re-login.
- **On-demand extend:** when a window is about to end (≤30 min) the app prompts on the account to open the next one the instant the current ends; a per-account toggle can do this automatically without asking, deferring to your scheduled anchor time when that falls inside the upcoming window.
- Quota for every account (including the machine default) is read live from the provider, so the displayed usage and reset time stay current and a refresh always reflects the real state.

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

Pushing a version tag like `v0.5.5` triggers the GitHub Actions workflow (`.github/workflows/release.yml`), which builds a universal macOS `.dmg` and publishes a GitHub Release with the artifact attached. Bump the version in `package.json`, `package-lock.json`, `src-tauri/tauri.conf.json`, `src-tauri/Cargo.toml` and `src-tauri/Cargo.lock` first, then:

```bash
git tag v0.5.5
git push origin main v0.5.5
```

See [CHANGELOG.md](CHANGELOG.md) for the per-version history.

## License

No license file yet — add one (e.g. MIT) before sharing widely if you want to allow reuse.
