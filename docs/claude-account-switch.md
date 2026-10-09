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

1. Record time = newest of `lastActivityAt`, `lastFocusedAt`, `createdAt`. The newest record wins and is copied verbatim to every folder (add or update). Archive state travels inside it.
2. Same newest time with different content: **conflict**. Every folder keeps its own file; nothing is added elsewhere; it is reported.
3. Deletion marker newer than or equal to the newest record: delete the record everywhere and put the marker everywhere. A record newer than the marker wins; the stale marker is removed.
4. `archived-sessions.idx` is rebuilt in each folder from the records that folder ends up with (sorted, Claude's exact byte format). An unrecognised index format is a blocker.
5. Unreadable records/markers are conflicts and left alone. Other files are untouched.

Apply: refuses if Claude runs (process table by executable path inside `Claude.app`); plans; validates every target (size, mtime, content hash) and every copy source; backs up each file it will change to `~/Library/Application Support/Pulse/claude-sync-backups/<YYYYMMDD-HHMMSS-mmm>/` with a manifest and journal (newest 10 kept); re-validates each file just before writing it; writes temp+fsync+rename with `O_EXCL|O_NOFOLLOW`; if a write fails, files already written are restored from the backup. Index files are written last. `restore` puts every file back, refuses if a file changed since the sync (`--force` overrides) and while Claude runs; it is idempotent.

Nothing detects accounts. Every account folder present under `claude-code-sessions` is merged; the owner presses the restart button after signing in to a new account. An account without a `claude-code-sessions` folder yet (Claude creates it on first sign-in) cannot receive sessions until it exists; press the button again after signing in once. Optionally `~/Library/Application Support/Pulse/claude-accounts.json` (CLI-only: `include|exclude <id>`) can exclude an account; absence from it never keeps an account out, and a missing or unreadable file means all accounts.

## Use

- Mac notch, Claude hover card: a small round button at the right of the "Claude Usage" header ("Restart Claude and sync chats"). It runs off the main thread: `NSRunningApplication.terminate()` on Claude if open and waits up to 20 s (never force-kills), runs the bundled `Contents/Helpers/pulse claude sync --apply --json`, then reopens Claude. The button is the only feedback: spinner while running, a checkmark for about 2 s on success, a red exclamation on failure with the error as its tooltip ("Claude is still open", or the CLI error). Claude is reopened even if the sync failed.
- Hub Settings, Accounts, Claude: every account folder under `claude-code-sessions/` and `local-agent-mode-sessions/` (union of the account-id directory names; nothing inside is read) is listed, signed in or not. An account with no reading shows "No reading yet" and its name defaults to `Claude <first 8 characters of the id>`; its name can be edited at once (saved in `claude-account-usage.json` by id). The Active badge is the account in Claude Desktop's `lastKnownAccountUuid`; order is active, then last reading, then folder mtime. Forget is offered only for an account whose folder is gone. Windows does not publish this list (no Windows notch-to-hub bridge yet).
- After the notch reopens Claude, and whenever `lastKnownAccountUuid` in Claude's `config.json` changes (polled by stat every 3 s, every 1 s for 60 s after the button), the notch drops the old account's cached Claude numbers and reads usage at once (`.fromSource`; the endpoint's 429 back-off still applies).
- CLI: `pulse claude accounts|known|backups [--json]`, `pulse claude sync --dry-run|--apply [--json]` (all accounts except explicit exclusions), `pulse claude auto [--json]` (sync if Claude is closed), `pulse claude include|exclude <id>`, `pulse claude restore <backup-id> [--force]`. `--root`, `--backups`, `--registry` point at a copy of the layout.

## Limits

Windows compiles but reports "not supported yet" unless `%APPDATA%\Claude\claude-code-sessions` exists; its layout is unverified. Only Claude Desktop Code sessions are merged; local-agent-mode sessions hold no session records here. Verification is the fixture journey `core/tests/claude_sync.rs` (CI only); nothing was run locally.
