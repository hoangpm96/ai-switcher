# AI Account Switcher

A native macOS app to manage and switch between **multiple accounts** for AI coding tools — **Claude Code**, **Codex**, and **Antigravity IDE** — from one place.

Built with [Tauri](https://tauri.app) (Rust + React).

> ⚠️ Using multiple subscription accounts may violate a provider's terms of service. This app only manages logins locally on your machine — use at your own discretion.

## Features

- **One window for every tool.** Log in, switch, rename, and remove accounts per tool.
- **Quota at a glance.** Reads 5-hour / weekly usage for Claude & Codex and per-model quota for Antigravity, with an optional **auto-switch** when the active account runs out.

### Claude Code & Codex (CLI)

- Each account logs into its own isolated config dir and gets a **dedicated command** (`claude-<name>`, `codex-<name>`) so you can run several accounts in parallel across terminals.
- The bare `claude` / `codex` command **follows the account you select** (via a shell hook + an "active profile" file). Run `aisw` in an already-open terminal to sync it to the latest selection.
- Chat sessions are **shared across accounts** in the same project, so you can resume work regardless of which account created it.

### Antigravity IDE (GUI)

- Switching swaps the saved login token in the IDE's `state.vscdb`. The app quits and reopens the IDE around each swap so the token isn't clobbered.
- **Sign in new account** puts the IDE at a logged-out screen so you can add an account that was never signed in.
- Accounts are identified by their **Google avatar**, and duplicates of the same account are detected automatically.

## Install

1. Download the latest `.dmg` from the [Releases](../../releases) page.
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

Pushing a tag like `v0.1.0` triggers the GitHub Actions workflow (`.github/workflows/release.yml`), which builds a universal macOS `.dmg` and attaches it to a draft GitHub Release:

```bash
git tag v0.1.0
git push origin v0.1.0
```

## License

No license file yet — add one (e.g. MIT) before sharing widely if you want to allow reuse.
