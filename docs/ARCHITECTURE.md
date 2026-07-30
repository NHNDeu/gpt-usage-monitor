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

## Account isolation

Every local account has a random UUID and a dedicated directory below the
standard application data directory. Only that child process receives the
corresponding `CODEX_HOME`; global environment variables are not changed.
The child is also launched with
`cli_auth_credentials_store="file"` so Codex stores the managed ChatGPT login
inside that isolated home. The monitor does not parse or copy `auth.json`.
Directories are mode `0700` and configuration files mode `0600` on Unix.

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

The application retains the `tokio::process::Child` handle; it never searches
for processes by name. Normal completion closes stdin and waits briefly.
Timeout or cancellation kills only that owned child. `kill_on_drop` is the
last-resort guard. Closing the window cancels every token and joins worker
threads before the process exits. No tray process, daemon, scheduler, or
periodic task is created.
