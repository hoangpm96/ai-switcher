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
- **Auto Session.** Anchor each Claude/Codex account's 5-hour reset to your work rhythm — the app sends a prime request at a time you pick, then verifies the provider's reset state before reporting success. It can optionally wake the Mac and run a headless worker under the correct macOS user, and provides a **Prime ngay** button for on-demand requests.

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

- Give each Claude/Codex subscription account **one daily prime time**; the app sends a minimal "hi" to request a fresh 5-hour window, then confirms the provider's reset state. Primes at most once per day per account. Set or change a time after today's slot has passed and the first prime is the **next** occurrence — not an immediate catch-up the same evening.
- Priming runs the account's own `claude` / `codex` CLI (so it can refresh its token), with a direct HTTP fallback. The background invocation is isolated from projects, plugins, hooks, MCP servers, history, and shared config paths so it does not traverse macOS-protected folders such as Desktop, Documents, Downloads, or mounted volumes. Scheduled/extend attempts are verified before success is recorded and retry within the bounded catch-up/deadline window when confirmation is not yet visible. Each attempt — success, hold, skip, fail, or late catch-up — is written to an activity log with the trigger source (`SCHEDULE`, `AUTO-EXTEND`, `EXTEND`, or `MANUAL`) plus an attempt id, with a per-day stats summary.
- Optionally install a **one-time privileged helper** so the Mac wakes itself ~10 minutes before a prime via `pmset`, plus an optional per-user headless daemon so each macOS profile runs only its own configured accounts even when the GUI is not scheduled during DarkWake. The app does not store or enter your macOS password. After sleep, the login Keychain may remain locked; in that case credential access is skipped and retried after that user unlocks the Mac. Without the helper/daemon, priming runs whenever the machine is awake / the app is open. A missed time is caught up only within a **bounded window** (~60 min past the anchor) — wake or open the app hours later and that day's anchor is treated as missed, so it never fires a stray prime into a still-live window late at night.
- **Prime ngay (on demand).** When the provider reports that an account has no active five-hour window, the card shows a manual prime button. After clicking it, the UI first says that the request was sent and is awaiting confirmation; it says a session opened only after the reset state is verified. If the provider cannot prove a new fixed window, the app reports that clearly instead of claiming success.
- **On-demand extend:** when a window is about to end (≤30 min) the app prompts on the account to open the next one the instant the current ends; a per-account toggle can do this automatically without asking, deferring to your scheduled anchor time when that falls inside the upcoming window.
- Quota for every account (including the machine default) is read live from the provider, so the displayed usage and reset time stay current and a refresh always reflects the real state.

#### Prime and quota troubleshooting

- **Claude quota returns HTTP 401 or 429:** the app runs one isolated Claude CLI refresh, reloads the profile token, and retries the usage endpoint once. Recovery is limited to once per account every five minutes to avoid repeated CLI launches or permission prompts. If it still fails, open that account's dedicated Claude command once after unlocking the Mac, then refresh in the app.
- **Prime reports a CLI error:** the activity log includes the concise CLI stderr reason. Confirm that the account's dedicated command can start and that the profile is still logged in.
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

Pushing a version tag like `v0.5.10` triggers the GitHub Actions workflow (`.github/workflows/release.yml`), which builds a universal macOS `.dmg` and publishes a GitHub Release with the artifact attached. Bump the version in `package.json`, `package-lock.json`, `src-tauri/tauri.conf.json`, `src-tauri/Cargo.toml` and `src-tauri/Cargo.lock` first, then:

```bash
git tag v0.5.10
git push origin main v0.5.10
```

See [CHANGELOG.md](CHANGELOG.md) for the per-version history and
[the v0.5.10 release notes](docs/releases/v0.5.10.md) for the current release.

## License

No license file yet — add one (e.g. MIT) before sharing widely if you want to allow reuse.
