# Protocol compatibility record

Checked on 2026-07-30.

## Sources and observed version

- OpenAI Codex App Server README:
  <https://github.com/openai/codex/blob/main/codex-rs/app-server/README.md>
- OpenAI Codex protocol source/schema:
  <https://github.com/openai/codex/tree/main/codex-rs/app-server-protocol>
- eframe 0.35.0:
  <https://docs.rs/eframe/0.35.0/eframe/>
- egui 0.35.0:
  <https://docs.rs/egui/0.35.0/egui/>

The development machine exposed
`/Applications/ChatGPT.app/Contents/Resources/codex` as
`codex-cli 0.146.0-alpha.3.1`.

The implementation generated a stable JSON Schema bundle with:

```sh
codex app-server generate-json-schema --out <temporary-directory>
```

It also performed a real isolated-home probe. The accepted handshake was:

```json
{"method":"initialize","id":1,"params":{"clientInfo":{"name":"gpt_usage_monitor_probe","title":"GPT Usage Monitor Probe","version":"0.1.0"},"capabilities":{}}}
{"method":"initialized"}
```

The response included `userAgent`, `codexHome`, `platformFamily`, and
`platformOs`. An unauthenticated `account/rateLimits/read` returned a standard
error envelope with code `-32600` and the message that Codex account
authentication was required.

## Stable methods used

- `account/read` with `{ "refreshToken": false }`
- `account/login/start` with `chatgpt` or `chatgptDeviceCode`
- `account/login/completed`
- `account/login/cancel`
- `account/logout`
- `account/rateLimits/read`
- `account/usage/read` as optional enrichment

The wire transport is JSONL over stdio and omits the JSON-RPC `jsonrpc` header.
The client sends one request at a time but still matches response IDs, consumes
notifications, handles partial stdout reads, rejects unexpected server
requests, and tolerates unknown JSON fields.

## Rate-limit evolution

Current generated schemas contain both:

- `rateLimits`: the backward-compatible historical snapshot.
- `rateLimitsByLimitId`: a multi-bucket map keyed by metered `limitId`.

The domain adapter always prefers a non-empty map and falls back to the
historical snapshot. Within each snapshot it reads every non-null `primary` and
`secondary` window. `usedPercent` is clamped for display and remaining percent
is `100 - used`. `resetsAt` is documented as Unix seconds; the adapter also
accepts millisecond or microsecond magnitudes for defensive compatibility.

## Credential storage decision

The current Codex config schema documents `cli_auth_credentials_store` as:

- `file` (default): credentials in the Codex home;
- `keyring`: operating-system credential store;
- `auto`: keyring where available, otherwise file.

This application explicitly selects `file` for every short-lived App Server.
That gives deterministic isolation by account `CODEX_HOME` on both supported
platforms. Codex remains the sole reader/writer of the credential file. The
tradeoff is that same-user processes can read the file, so the directory is
permission-restricted and the README warns users never to share it.

## Maintenance

When Codex changes:

1. regenerate the schema with the intended minimum Codex version;
2. update raw parsing only in `protocol`, `app_server`, and `rate_limits`;
3. add a fixture to `tests/fixtures/mock_codex.js`;
4. run format, clippy, tests, release build, and a logged-in manual smoke test.
