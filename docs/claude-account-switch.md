# Claude Desktop: sharing Code sessions across accounts

Findings from a read-only look at `~/Library/Application Support/Claude` (macOS, Claude Desktop 2.31). No secrets were read or copied.

## On-disk format

Code-session metadata is per account and per organisation, in real folders:

```
claude-code-sessions/<accountUUID>/<orgUUID>/
    local_<uuid>.json        one session record
    deleted_<uuid>           deletion marker; content is a ms timestamp (no newline)
    archived-sessions.idx    {"v":1,"archived":["local_<uuid>",...]}  (sorted, no trailing newline)
    backlog/  scheduled-tasks.json  waiting-input     other state, not session lists
local-agent-mode-sessions/<accountUUID>/<orgUUID>/    scheduled-tasks.json, rpm/  (no session records)
```

- A record is JSON with `sessionId` (= `local_<uuid>`, equal to the file name), `cliSessionId` (the shared transcript under `~/.claude`), `createdAt`, `lastActivityAt`, `lastFocusedAt` (ms), `isArchived`, `title`, `cwd`, `model`, and many optional fields.
- Deletion is a marker file `deleted_<uuid>` next to where the record was; the record file is gone. In the sample, 50 markers, none with a live record.
- Archive state lives in the record (`isArchived`) **and** in the index. In the sample the index was exactly the set of records with `isArchived: true`.
- Folders under different accounts hold copies (identical bytes for most records); the active account's folder is simply the newest. Org ids repeat across accounts.
- Which account is active: `config.json` has `lastKnownAccountUuid`; sign-in itself is in `oauth:tokenCache*` (secret, never touched). `window-state.json` and `Local State` hold no account choice. There is **no non-secret setting that preselects the account on launch**, so Pulse reopens Claude and the owner chooses the account in Claude.
- No email or display name exists in any non-secret file; Pulse shows the first 8 characters of the account id.
- Directory symlinks between account folders do not work (Claude opens the folder with `O_DIRECTORY|O_NOFOLLOW`). Pulse refuses (blocker) if it finds a symlinked folder.

## Design

Real per-account folders; transcripts are never read or written; metadata is merged only while Claude is fully closed. Module: `core/src/claude_sync.rs`.

Merge, per session id across all included accounts' folders:

1. The furthest-along copy wins and is copied verbatim to every folder (add or update): most `completedTurns`, then newest `latestUserFrameAt`, then record time (newest of `lastActivityAt`, `lastFocusedAt`, `createdAt`). Time alone was wrong: Claude Desktop bumps `lastActivityAt` on a stale copy just by listing it after a sign-in, which let a copy with a hundred fewer turns and no archive flag overwrite the real, archived one (seen 2026-10-10). Archive state travels inside the winning record.
2. Same newest time with different content: **conflict**. Every folder keeps its own file; nothing is added elsewhere; it is reported.
3. Deletion marker newer than or equal to the newest record: delete the record everywhere and put the marker everywhere. A record newer than the marker wins; the stale marker is removed.
4. `archived-sessions.idx` is rebuilt in each folder from the records that folder ends up with (sorted, Claude's exact byte format). An unrecognised index format is a blocker.
5. Unreadable records/markers are conflicts and left alone. Other files are untouched.

Apply: refuses if Claude runs (process table by executable path inside `Claude.app`); plans; validates every target (size, mtime, content hash) and every copy source; backs up each file it will change to `~/Library/Application Support/Pulse/claude-sync-backups/<YYYYMMDD-HHMMSS-mmm>/` with a manifest and journal (newest 10 kept); re-validates each file just before writing it; writes temp+fsync+rename with `O_EXCL|O_NOFOLLOW`; if a write fails, files already written are restored from the backup. Index files are written last. `restore` puts every file back, refuses if a file changed since the sync (`--force` overrides) and while Claude runs; it is idempotent.

Nothing detects accounts. Every account folder present under `claude-code-sessions` is merged; the owner presses the restart button after signing in to a new account. An account without a `claude-code-sessions` folder yet (Claude creates it on first sign-in) cannot receive sessions until it exists; press the button again after signing in once. Optionally `~/Library/Application Support/Pulse/claude-accounts.json` (CLI-only: `include|exclude <id>`) can exclude an account; absence from it never keeps an account out, and a missing or unreadable file means all accounts.

## Use

- Mac notch, Claude hover card: a small round button at the right of the "Claude Usage" header ("Restart Claude and sync chats"). It runs off the main thread: `NSRunningApplication.terminate()` on Claude if open, 20 s, then `forceTerminate()` and 10 s more; the sync is retried for up to 12 s while Claude helper processes wind down (CLI code `claude_running`); a failure stays on the button and is logged. Waits up to 20 s (never force-kills), runs the bundled `Contents/Helpers/pulse claude sync --apply --json`, then reopens Claude. The button is the only feedback: spinner while running, a checkmark for about 2 s on success, a red exclamation on failure with the error as its tooltip ("Claude is still open", or the CLI error). Claude is reopened even if the sync failed.
- Hub Settings, Accounts, Claude: every account folder under `claude-code-sessions/` and `local-agent-mode-sessions/` (union of the account-id directory names; nothing inside is read) is listed, signed in or not. Accounts never read fold into one "N accounts never seen signed in" line (expandable, each "no reading"); their name defaults to `Claude <first 8 characters of the id>`; its name can be edited at once (saved in `claude-account-usage.json` by id). The Active badge is the account in Claude Desktop's `lastKnownAccountUuid`; order is active, then last reading, then folder mtime. Forget is offered only for an account whose folder is gone. The Windows notch publishes the same list (`claudeAccounts` on the Claude row of `notch-state.json`, from `%LOCALAPPDATA%\Pulse\claude-account-usage.json`) and accepts the same `renameClaudeAccount` / `forgetClaudeAccount` commands.
- After the notch reopens Claude, and whenever `lastKnownAccountUuid` in Claude's `config.json` changes (polled by stat every 3 s, every 1 s for 60 s after the button), the notch drops the old account's cached Claude numbers and reads usage at once (`.fromSource`; the endpoint's 429 back-off still applies).
- CLI: `pulse claude accounts|known|backups [--json]`, `pulse claude sync --dry-run|--apply [--json]` (all accounts except explicit exclusions), `pulse claude auto [--json]` (sync if Claude is closed), `pulse claude include|exclude <id>`, `pulse claude restore <backup-id> [--force]`. `--root`, `--backups`, `--registry` point at a copy of the layout.

## Windows

Claude Desktop keeps the same layout under `%APPDATA%\Claude` (`claude-code-sessions`, `local-agent-mode-sessions`, `config.json` `lastKnownAccountUuid`); it is installed under `%LOCALAPPDATA%\AnthropicClaudepp-<version>\claude.exe`. Claude Code's command line is also `claude.exe` (Desktop even starts its own copy under `%APPDATA%\Claude\claude-code\`), so the notch only ever treats a process as Desktop by its image path.

- The Windows Claude card has the same small round button. It runs on a worker thread: `WM_CLOSE` to Desktop's `Chrome_WidgetWin_1` frames (never a terminate), waits up to 20 s for every Desktop process to exit, runs `Helpers\pulse.exe claude sync --apply --json` hidden (`CREATE_NO_WINDOW`; a non-zero exit shows the CLI's `error` text), then reopens Desktop through `%LOCALAPPDATA%\AnthropicClaude\claude.exe` whatever the sync said. Feedback is the button: spinner, a check for about 2 s, a red mark with the reason as a line on the card.
- Desktop's own close handler decides whether `WM_CLOSE` quits it: with `preferences.menuBarEnabled` false in `claude_desktop_config.json` ("tray disabled") closing the main window quits the app; with it on (Desktop's default) the window only hides to the tray and the app keeps running. In that case the button does not hide the window for nothing: it fails with "Claude keeps running in the system tray. Quit it from its tray icon (or turn the tray off in Claude's settings), then press again", and nothing is killed.
- The account list and the account book follow the Mac's (names default to `Claude <first 8>`, order tracked account, last reading, folder time; Forget only for gone folders). The signed-in address shown for Claude Code's own account comes from `oauthAccount.emailAddress` in `.claude.json`.
- `lastKnownAccountUuid` is read every 3 s (every 1 s for 60 s after the button), together with whether Desktop runs (its `lockfile` is held), and a change refetches the Claude reading at once.

## Limits

Only Claude Desktop Code sessions are merged; local-agent-mode sessions hold no session records here. Verification is the fixture journey `core/tests/claude_sync.rs` (CI only); nothing was run locally.

## Which account the notch shows, and where its usage comes from

Claude Desktop caches usage in Chromium's HTTP cache, `~/Library/Application Support/Claude/Cache/Cache_Data/*_0` (Simple Cache entries): the zstd body of `GET https://claude.ai/api/organizations/<orgUUID>/usage[?skip_spend=1|cedar_ember=1]` (`five_hour`, `seven_day`, `utilization`, `resets_at`, ...), dated by the response `Date:` header. The key carries the **organization** uuid, not the account uuid. `plan-usage-history.json` holds only `{t, org, u}` samples (also by organization). Desktop's own account is `lastKnownAccountUuid` in `config.json`; the organizations of an account are the folder names under `claude-code-sessions/<accountUUID>/` and `local-agent-mode-sessions/<accountUUID>/`. That folder-to-organization link is how a cache entry is tied to an account.

Tracked account (`ClaudeOAuthProvider.trackedAccountID`): `lastKnownAccountUuid` while Claude Desktop is running, otherwise Claude Code's `oauthAccount.accountUuid` in `~/.claude.json`. The ring, card and hub all follow it.

Source chain for the default Claude ring:

1. Desktop running and its account differs from Claude Code's: only Desktop's cache for that account's organizations (newest entry), and only if fresh (30 min, 2 min for live) and no window past its reset. Keychain, `claude /usage` and the usage endpoint are never used, because they describe the Claude Code account. No such reading: the ring shows the dim unknown state and the card says "No reading for <name> yet". Another account's numbers are never shown.
2. Desktop not running, or running with the same account as Claude Code: unchanged chain: Desktop cache (Claude Code's organization), then `claude /usage`, then the keychain token against the usage endpoint. The endpoint's 429 back-off still applies (it belongs to the endpoint; the Desktop-only path makes no request).

On a Desktop account switch (`lastKnownAccountUuid` change, or Desktop starting or quitting) the displayed reading and the last-good copy are dropped at once, held provider state is cleared, and usage is refetched `.fromSource`. The account book keeps each account's last reading under its own id; the old account still shows in the hub with its time. A reading is saved under the tracked account's id only; the card name is the chosen name, else the address (Claude Code's account only), else `Claude <first 8 of id>`. Nothing is written to Claude's files and no token is read.

## Reopening the chats that were running

Quitting Desktop, or switching account inside it, ends every Code chat's CLI. Desktop starts a chat's CLI again (`--resume <cliSessionId>`) only when that chat's page is shown. On launch it brings back only chats that were mid-turn, and it sends those a "continue" message. Idle chats stay closed, and so they drop off the Pulse bridge.

- **`pulse claude remember`** records the running chats in `claude-open-chats.json`, next to `claude-sync-backups`.
  - What it records: the Desktop-hosted chats in `~/.claude/sessions/*.json` (`entrypoint: claude-desktop`, live pid, matching start time), keyed by Desktop's record id `hostSessionId` (`local_<uuid>`).
  - When it runs: the notch calls it about once a minute while Desktop runs, and once more just before the restart's close. An account switch inside Desktop ends the chats before the button is pressed, so the earlier records are what it uses.
  - Chats not seen for a day are forgotten.
- **`pulse claude reopen [--dry-run]`** runs after the restart reopens Desktop, and opens the last running set again.
  - Which chats: those seen within 3 minutes of the newest record. Nothing is reopened if that record is more than 30 minutes old.
  - What it skips: chats that are already running, archived, scheduled tasks, or missing from the signed-in account.
  - How it opens them: one at a time with Desktop's own `claude://code/continue?session=local_<uuid>` link. The most recently focused chat goes last, so it is the page left showing. After each link it waits for that chat's CLI to start.
  - If a chat does not start, it stops. Desktop is not taking links in that case: it is signed out, links are turned off, or it is not running.
  - Nothing is sent to any chat.
