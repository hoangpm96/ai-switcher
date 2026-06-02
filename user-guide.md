# AI Account Switcher — User Guide

This guide explains how to use **AI Account Switcher** to keep several logins for
**Claude Code**, **Codex**, and **Antigravity IDE** on one Mac and switch between
them — including how parallel terminals, chat resume, and quota tracking behave.

> **Platform:** macOS only. The app uses AppleScript, the macOS Keychain, and your
> shell startup files (`~/.zshrc`, and `~/.bashrc` if present).

> **Terms of service:** Using multiple subscription accounts may violate a
> provider's terms. This app only manages logins — use it at your own discretion.
> The app shows this notice once on first launch ("Got it" to dismiss).

---

## 1. Core concepts (read this first)

The three tools work in two different ways. Understanding the split is the key to
using the app correctly.

| | Claude Code | Codex | Antigravity IDE |
|---|---|---|---|
| Kind of tool | CLI | CLI | GUI app |
| How you log in | `claude` OAuth in a Terminal window | `codex` OAuth in a Terminal window | You sign in inside the IDE itself |
| Where credentials live | macOS Keychain (per profile) | `auth.json` inside the profile | The IDE's own SQLite state file |
| How switching works | **Non-destructive** — points the bare command at a profile | **Non-destructive** — same as Claude | **Copy-swap** — overwrites the IDE's token, then restarts the IDE |
| Per-account command | Yes — `claude-<name>` | Yes — `codex-<name>` | None (it's a GUI) |
| Run several accounts at once | Yes (one per terminal) | Yes (one per terminal) | No — one active account at a time |
| Chat history across accounts | **Shared** | **Shared** | **Per account** |
| Quota tracked per account | Yes | Yes | Yes (per model) |

### Two ways to select an account (Claude & Codex)

1. **The "Use" button → the bare command.**
   Clicking **Use** on an account makes the plain `claude` (or `codex`) command
   use that account. This is the *active* account. It works by writing the
   chosen profile path to an "active" file and installing a small shell hook —
   it does **not** copy or move any credentials.

2. **The per-account command → parallel use.**
   Every Claude/Codex account also gets its own command, e.g. `claude-work` or
   `codex-pro`. That command **always** uses that one account regardless of what
   "Use" is set to. This is how you run two accounts side by side in two
   terminals.

So: `claude` follows whatever you last clicked **Use** on; `claude-work` is
pinned to the "work" account forever.

### The shell hook (`aisw`)

The first time you switch a Claude/Codex account, the app adds an idempotent
block to `~/.zshrc` (and `~/.bashrc` if it exists), bracketed by:

```sh
# >>> ai-account-switcher >>>
aisw() {
  if [ -r ~/.config/ai-account-switcher/active/claude.profile ]; then export CLAUDE_CONFIG_DIR="$(cat ...)"; else unset CLAUDE_CONFIG_DIR; fi
  if [ -r ~/.config/ai-account-switcher/active/codex.profile  ]; then export CODEX_HOME="$(cat ...)";       else unset CODEX_HOME;       fi
  [ -n "$1" ] && echo "AI Account Switcher: synced the account for this terminal."
}
aisw >/dev/null 2>&1
# <<< ai-account-switcher <<<
```

- **New terminals** automatically pick up the active account (the hook runs at
  shell startup).
- **Terminals that are already open** keep their old account until you run
  `aisw` in them (or open a new terminal). Running `aisw` re-reads the active
  file and re-exports the environment for that one terminal.

### The "Machine default" account

The app always shows a read-only **Machine default** account. This is your
baseline login — plain `~/.claude` / `~/.codex`. When no account is selected
with "Use" (the active file is empty), the bare command falls back to this. You
can't delete it; you can only "forget" managed accounts you added.

---

## 2. Requirements

Before adding accounts:

- **macOS.**
- For **Claude**: the `claude` CLI installed and on your `PATH`.
- For **Codex**: the `codex` CLI installed and on your `PATH`.
- For **Antigravity**: **Antigravity IDE** installed at
  `/Applications/Antigravity IDE.app` (or the `antigravity-ide` binary on PATH).
- A shell that is **zsh** (macOS default) or **bash**. The app edits your rc files.

If a tool isn't installed, its tab shows **"Tool not installed"** and its Add
buttons are disabled.

---

## 3. Claude Code

### 3.1 Add an account

1. Open the **Claude Code** tab.
2. Click **Add account**.
3. Fill in:
   - **Account name** — free text, max 20 characters. Leave blank to auto-name
     (e.g. "Claude 1", "Claude 2").
   - **Custom command (required)** — the suffix for this account's parallel
     command. The app forces the `claude-` prefix and allows only `a-z 0-9 - _`.
     Example: type `work` → you get the command `claude-work`.
4. Click **Create & login**. The app:
   - creates a private profile directory for this account,
   - opens a **Terminal** window running the Claude login against that profile,
   - shows the account as **"Signing in"** with *"Waiting for login in Terminal —
     the app will detect it."*
5. In the Terminal window, follow the prompts and approve the login in your
   browser. When it's done you'll see a line telling you to return to the app —
   you can close that Terminal window.
6. The app **auto-detects** the finished login (it polls for a few minutes),
   flips the account to **Ready**, and reads its quota. You don't click anything
   else.

> Each account's credentials are stored in the macOS Keychain under a
> profile-specific entry — they are never copied between accounts.

### 3.2 Switch which account the bare `claude` uses

- Click **Use** on the account you want.
- Toast: *"Now using: <name> (Claude Code)"*.
- Notice: *"Selected <name>. A new terminal running `claude` uses it right away;
  in an open terminal run `aisw` to sync."*

So after clicking **Use**: open a new terminal and just run `claude` — or, in a
terminal you already have open, run `aisw` first, then `claude`.

### 3.3 Run two accounts in parallel

Use the per-account commands. For example, with accounts `work` and `personal`:

```sh
# Terminal A — work account
claude-work

# Terminal B — personal account
claude-personal
```

These ignore the "Use" selection and always target their own account, so you can
run both at the same time. The command for an account is shown on its card — click
the chip to copy it. If an account has no command yet, click **Set custom
command** (or the terminal icon) to create one (`claude-…`, only `a-z 0-9 - _`).

### 3.4 Resuming chats after switching

**Chat history and projects are shared across all your Claude accounts.** The app
symlinks Claude's `projects/` (conversation transcripts per project) and
`history.jsonl` (prompt history) so every profile sees the same data. Switching
accounts changes *who you're billed as / whose quota is used* — it does **not**
hide or reset your conversations.

That means normal resume works regardless of which account is active:

```sh
claude --resume      # pick a past conversation to continue
claude --continue    # continue the most recent conversation
```

> Practical note: quota and rate limits are **per account**, but the conversation
> list is shared. You can start a chat on one account, switch the active account,
> and resume the same conversation under the other account.

### 3.5 Quota

- Each card shows a **5-hour limit** and a **Weekly limit** bar, plus a reset
  time.
- Click **Refresh quota** (top right of the panel, or the global refresh icon) to
  re-read.
- Quota is read from Claude's official usage endpoint using that account's token,
  and cached briefly to avoid hammering the endpoint.
- If it can't be read, the card shows an error string instead of bars — switching
  still works; quota reading is never blocking.

### 3.6 Auto-switch (optional)

The Claude/Codex panels have an **"Auto-switch account when quota runs out"**
toggle:

- When the **active** (bare-command) account is nearly out, the app auto-switches
  the bare command to the account with the most quota left.
- You pick the threshold: **90% / 95% / 100% (fully out)**.
- Per-account `claude-…` commands are **not** affected.
- It applies to **new terminals**; you'll get a banner like *"Claude Code is out
  of quota — auto-switched to <name>. Open a new terminal to apply."*

### 3.7 Rename / set command / delete

On each card:

- **Pencil** → rename (max 20 chars).
- **Terminal icon** → set or change the custom command.
- **Trash** → delete the managed account (removes it from the app and its
  launcher; Machine default can't be deleted).

---

## 4. Codex

Codex behaves **exactly like Claude Code**, with these differences:

- Login runs the `codex` OAuth flow in a Terminal window; the token is saved to an
  `auth.json` file inside the account's profile (not the Keychain).
- The per-account command prefix is `codex-` (e.g. `codex-pro`).
- The bare `codex` command follows the **Use** selection; `aisw` syncs open
  terminals; new terminals pick it up automatically.

### 4.1 Add, switch, parallel

Same steps as Claude — see §3.1–3.3. Replace `claude` with `codex`:

```sh
codex            # active account (set by "Use")
codex-pro        # always the "pro" account
codex-dev        # always the "dev" account
```

### 4.2 Resuming chats

Codex session history is also **shared across accounts** — the app symlinks
Codex's `sessions/` (conversation rollouts) and `history.jsonl`. Resume your
previous Codex session as usual; switching the active account doesn't erase it.

### 4.3 Quota & auto-switch

Same as Claude (§3.5–3.6): 5-hour + weekly bars, **Refresh quota**, and the
auto-switch toggle with 90/95/100% thresholds.

---

## 5. Antigravity IDE

Antigravity is a GUI app, so it works differently: **there is one active login at
a time**, and switching means the app rewrites the IDE's stored token and
restarts the IDE.

### 5.1 How it stores logins

Antigravity keeps its login token in its own SQLite state file:

```
~/Library/Application Support/Antigravity IDE/User/globalStorage/state.vscdb
```

The app reads/writes the OAuth token (and your profile/avatar URL) there. Because
the IDE only writes the token to disk **when it quits**, the app quits and
reopens the IDE around every save/switch — this is expected.

### 5.2 Save the currently signed-in account

1. **Sign into Antigravity IDE** with the account you want to save (the app does
   **not** log in for you — it captures the session that's already signed in).
2. In the app's **Antigravity** tab, click **Save current account**.
3. Optionally give it a name (max 20 chars; auto-named if blank).
4. Confirm **Save this account**. The app will **quit and reopen** the IDE to
   capture the right token, then add the account to the list.

If the IDE isn't signed in, you'll get:
*"Antigravity IDE isn't signed in — sign into the account you want to save first,
then click Save."*

Duplicate protection: if you try to save an account that's already saved (matched
by profile/avatar, or token as a fallback), the app tells you
*"This account is already saved"* and won't add a duplicate.

### 5.3 Add a second (or third) account

1. **First, save the account you're currently signed into** (§5.2) so you don't
   lose that session.
2. Click **Sign in new account**. The app:
   - quits the IDE, clears the stored token, and reopens the IDE at the
     **sign-in screen**,
   - shows: *"Antigravity IDE opened at the sign-in screen. After signing into the
     new account, click 'Save current account'."*
3. Sign into the new account inside the IDE.
4. Click **Save current account** again to save the new one.

> If you click "Sign in new account" while the current session is **not** yet
> saved, the app refuses and tells you to save the signed-in account first — this
> prevents losing a session you never captured.

### 5.4 Switch between saved accounts

- Click **Use** on a saved account.
- The app **quits the IDE, writes that account's token into `state.vscdb`, and
  reopens the IDE**.
- Toast: *"Switched to: <name>"*; notice: *"Loaded account <name>. Antigravity IDE
  is restarting to apply."*
- Whatever you had open in the IDE closes during the swap — save your work first.

### 5.5 Resuming work after switching

Unlike Claude/Codex, Antigravity sessions are **per account** (they live with
each account's token, not in a shared folder). After switching, the IDE comes back
up signed in as the selected account; you resume that account's own state inside
the IDE.

### 5.6 Quota

- Antigravity quota is shown **per model** ("Quota by model"), not as a single
  5-hour/weekly pair.
- Quota can only be read **while Antigravity IDE is running** (the app talks to the
  IDE's local language-server). If the IDE is closed you'll see a message asking
  you to open Antigravity IDE to read quota.

### 5.7 Things Antigravity does *not* have

- **No custom/parallel commands** — it's a GUI; you can't run two Antigravity
  accounts at once.
- **No auto-switch** — the auto-switch toggle is only shown for Claude/Codex.

---

## 6. Where things are stored

| What | Location |
|---|---|
| App state (account list, names, quota snapshots) | `~/.config/ai-account-switcher/state.json` |
| Per-account profiles | `~/.config/ai-account-switcher/accounts/<tool>/<id>/` |
| Active selection (bare command target) | `~/.config/ai-account-switcher/active/claude.profile`, `codex.profile` |
| Per-account commands | `~/.local/bin/claude-…`, `~/.local/bin/codex-…` |
| Claude credentials | macOS Keychain (per-profile entry) |
| Codex credentials | `auth.json` inside the account's profile |
| Antigravity token | The IDE's `state.vscdb` (copied in/out on switch) |
| Shell hook | `~/.zshrc` (and `~/.bashrc` if present), between the `ai-account-switcher` markers |

> Make sure `~/.local/bin` is on your `PATH` so the `claude-…` / `codex-…`
> commands resolve. If they don't run, that's the usual cause.

---

## 7. Troubleshooting

**"`claude` still uses the old account in a terminal I had open."**
The shell hook only re-reads the active account at startup. Run `aisw` in that
terminal, or open a new one.

**"`claude-work` isn't found."**
`~/.local/bin` probably isn't on your `PATH`. Add it
(`export PATH="$HOME/.local/bin:$PATH"`) and reload your shell.

**"Login never completes."**
Finish the browser approval in the Terminal window the app opened. The app polls
for a few minutes; if it times out, click **Add account** again. Make sure the
`claude` / `codex` CLI is installed and on your `PATH`.

**"Quota shows an error / 'No quota data yet'."**
Click **Refresh quota**. For Antigravity, the IDE must be running. Quota errors
never block switching.

**"Antigravity says it isn't signed in when I click Save."**
Sign into the IDE first with the account you want, then Save. The app captures an
existing session; it doesn't log in for you.

**"Switching Antigravity closed my IDE."**
Expected — every Antigravity save/switch quits and reopens the IDE so it flushes
and reloads the token. Save your work before switching.

**"I want to undo the shell changes."**
Delete the block between `# >>> ai-account-switcher >>>` and
`# <<< ai-account-switcher <<<` in `~/.zshrc` (and `~/.bashrc`). After that, the
bare `claude` / `codex` use the machine default, and the per-account `…-` commands
still work on their own.

---

## 8. Quick reference

```sh
# Claude
claude                 # active account (set by "Use" in the app)
claude-work            # the "work" account, always
claude --resume        # resume a past conversation (history shared across accounts)
aisw                   # sync the active account into an already-open terminal

# Codex
codex                  # active account
codex-pro              # the "pro" account, always
aisw                   # same sync command (handles both tools)
```

| Tool | Add | Switch active | Parallel | Resume chat |
|---|---|---|---|---|
| Claude | Add account → login in Terminal | **Use** + new terminal / `aisw` | `claude-<name>` | Shared history, `--resume` |
| Codex | Add account → login in Terminal | **Use** + new terminal / `aisw` | `codex-<name>` | Shared history, resume as usual |
| Antigravity | Sign in inside IDE → **Save current account** | **Use** (quits & reopens IDE) | Not supported | Per-account, resumes inside IDE |
