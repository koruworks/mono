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
- `display-message.json` is **synthetic**: the shape of the status line
  the maintainer saw read as reply text (koru-ai D69, 2026-11-02) — an
  `agent_message_chunk` whose `_meta` carries `cognition.ai/displayMessage`,
  sent as the session starts — with the hook's path placeholdered. No frame
  of it was kept; the adapter also reads the mark on the chunk's content.
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

**The floor is now 3000.11.1** (2026-11-02), so `version.txt` is refused,
as a below-floor print should be. 3000.11.1's handshake is not recorded:
until it is, the route tests replay these frames under the floor's version
(`tests/devin.rs`, `tests/routes.rs`). The signed-out answer the tests
replay is `new-session.json` with its `model` options emptied, the shape
observed above; no fixture of it was kept.

Refresh the same way against a 3000.11.x build after `devin update`, into
a `devin-3000.11.x` folder, then replay through `routes.rs`. Never retain
raw frames, session ids, account data or paths.
