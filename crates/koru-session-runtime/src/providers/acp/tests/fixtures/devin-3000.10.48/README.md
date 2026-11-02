# Devin CLI 3000.10.48 fixtures

Recorded 2026-11-02 on Windows 11 x64 from the `devin.exe` inside Cognition's
Devin desktop app (`devin 3000.10.48 (fcf7ba39)`), run as `devin acp` by a
disposable handshake driver: `initialize`, then `session/new {cwd,
mcpServers: []}` in a scratch folder, then stdin closed. **No prompt was
sent.** The frames were reduced in memory to these fields.

- `version.txt` is the exact `--version` print.
- `initialize.json` keeps the capabilities as observed. The one
  authentication method keeps its id (`devin-browser`, which the route never
  calls); its labels are replaced. `agentInfo` is kept to show it names
  neither the product nor the version (`affogato`, `0.0.0-dev`), which is why
  the route does not cross-check it against `--version`. The answer's
  `_meta` (a local config path) and the capability `_meta` flags are dropped.
- `new-session.json` is the signed-in answer, from the one session the
  maintainer approved on their own account: the legacy `modes` and the
  `mode` select with Devin's five ids and own names, and the `model` select.
  The session id is replaced; the account's one model is replaced by a
  placeholder. The option-level `_meta` (icon names) is dropped.
- `auth-required.json` is **synthetic**, the standard ACP `-32000` shape the
  other routes replay. It was not observed: signed out (an isolated profile
  with no credential and no `WINDSURF_API_KEY`), 3000.10.48 answers
  `session/new` with a session and an **empty** `model` list, so a missing
  sign-in would surface at the first prompt, which was not sent.

Observed but not kept: `_cognition.ai/mcp/serversChanged` and
`_cognition.ai/output` notifications before and after the `initialize`
answer (the server starts the MCP servers it imports from other tools'
configuration), and `config_option_update`, `current_mode_update` and
`available_commands_update` updates before the `session/new` answer. The
route tests replay a notification of each kind around the fixtures.

Refresh the same way against a newly pinned build, then replay through
`routes.rs`. Never retain raw frames, session ids, account data or paths.
