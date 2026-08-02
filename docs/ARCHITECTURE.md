# Architecture

## Boundaries

Codex Usage Monitor is one native `eframe` window plus short-lived worker
threads. It is not a Codex client and it never starts a model turn.

```text
egui UI
  │ commands / typed worker events
  ▼
worker coordinator ── one account at a time for Refresh All
  │
  ├─ CODEX_HOME=<app data>/accounts/<uuid>
  ├─ codex app-server --stdio
  │    ├─ initialize → initialized
  │    ├─ account/read
  │    ├─ account/rateLimits/read
  │    └─ account/usage/read (optional)
  └─ close stdin → wait → kill owned child on timeout

optional desktop switch transaction (explicit opt-in)
  ├─ parse only identity fields from target/global auth.json
  ├─ target account/read → stop verified Codex desktop host
  ├─ recovery backup → safe managed-account save-back
  ├─ same-directory atomic replace of global auth.json
  ├─ restart only if the host was running before the switch
  └─ global account/read → commit, or stop host and roll back
```

## Modules

- `app` owns persistent configuration, UI-facing runtime state, and event
  handling.
- `ui` renders the single window and translates clicks into application
  actions. It never consumes raw App Server JSON.
- `worker` runs blocking user journeys away from the UI thread and serializes
  Refresh All.
- `app_server` owns exactly one child process, its pipes, timeout/cancellation,
  login notifications, and deterministic shutdown.
- `protocol` is the tolerant wire adapter for newline-delimited JSON RPC
  messages. App Server omits the normal `jsonrpc` header.
- `rate_limits` converts current and legacy response shapes into stable domain
  models. It prefers `rateLimitsByLimitId`, then falls back to `rateLimits`.
- `codex_locator` probes a custom executable or common platform locations,
  including Finder-launch-safe macOS locations.
- `storage` owns schema-versioned JSON configuration and account directories.
- `logging` keeps a capped, rotated, redacted diagnostic log.
- `platform` installs a CJK system font, window icon, and panic hook.
- `account_switch` owns credential identity extraction, managed-account matching,
  restricted recovery backups, same-directory atomic replacement, and rollback.
- `desktop_host` owns conservative macOS/Windows host identity checks, graceful
  quit, bounded PID-specific termination, captured App Server cleanup, and restart.

## Account isolation

Every local account has a random UUID and a dedicated directory below the
standard application data directory. Only that child process receives the
corresponding `CODEX_HOME`; global environment variables are not changed.
The child is also launched with
`cli_auth_credentials_store="file"` so Codex stores the managed ChatGPT login
inside that isolated home. The default quota path does not parse or copy
`auth.json`. The optional desktop switch path reads only the identity-bearing
structure required to prevent cross-account writes, copies the file locally,
and never exposes token values. Directories are mode `0700` and configuration or
credential files mode `0600` on Unix.

## Desktop account switch invariant

The current global credential is never assigned from a selection cache or a
display name. `tokens.account_id` or the ID-token ChatGPT account claim is the
primary key. A complete email is considered only when the token marks it as
verified. A match must be unique; ambiguous or unknown credentials are backed
up but never written into a managed account home.

The worker admits exactly one desktop operation and blocks refresh, login,
logout, delete, and another switch for the duration. Cancellation is honored
while validating the target. Once host shutdown begins, the transaction runs to
commit or rollback and application exit joins that worker.

macOS checks the enclosing app's `Info.plist`, accepts `com.openai.codex`, and
excludes `com.openai.chat`. Windows requires a candidate process name plus
verified install-path/package or embedded-CLI evidence. Any forced termination
addresses a revalidated PID; there is no `killall ChatGPT` or name-wide
`taskkill` path.

## Login flow

1. The UI creates the local account directory.
2. A temporary App Server is initialized.
3. `account/login/start` selects `chatgpt` or `chatgptDeviceCode`.
4. Only an HTTPS URL on an OpenAI-owned host is accepted.
5. The system browser opens and the worker waits for
   `account/login/completed`.
6. `account/read` verifies the resulting ChatGPT account and the app immediately
   reads limits.
7. The process is shut down.

Cancellation sends `account/login/cancel` on a fresh, bounded request before
shutdown.

## Query and cache flow

`account/read` produces the ChatGPT email and plan. The complete email is shown
in the account card and persisted only in the permission-restricted local
configuration; it is never used as the account identity or written to logs.
`account/rateLimits/read` is transformed into a list of all returned
primary/secondary windows for every `limitId`. The last successful domain
snapshot is saved with its UTC query timestamp. A later failure leaves that
snapshot visible and the UI labels it as cached and, after the configured
threshold, stale.

`account/usage/read` is optional. Failure of that secondary endpoint does not
discard valid quota data.

## Process exit guarantees

For normal account operations the application retains the
`tokio::process::Child` handle and never searches for a process by name. Normal
completion closes stdin and waits briefly; timeout or cancellation kills only
that owned child. The optional desktop switch enumerates host candidates, but a
name is never sufficient: bundle/package/path identity is revalidated before a
PID-specific signal. `kill_on_drop` is the last-resort guard. Closing the window
cancels every token and joins worker threads before the process exits. No tray
process, daemon, scheduler, or periodic task is created.
