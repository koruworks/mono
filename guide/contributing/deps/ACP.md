# ACP schema and transport traps

Koru uses agent-client-protocol-schema =1.7.0 with default-features=false,
under its v1 namespace. It does not link the full ACP SDK or create another
async runtime. [The pinned fixture](../../../tests/fixtures/acp/1.7.0/README.md)
records the upstream commit and regeneration command.

- The crate release number is not the negotiated protocolVersion integer.
  The current adapter checks integer 1 separately.
- Optional schema fields may default when their JSON value is malformed.
  Successful InitializeResponse deserialization is not evidence of HTTP or
  image support: check the actual boolean at the capability path.
- session/prompt responds when the turn ends. submit must return when its
  request was sent, leaving next_event to process approval requests and stop
  reasons. ACP carries no Koru turn id; keep the caller's identity privately.
- A permission response has two outcome levels:
  {"outcome":{"outcome":"cancelled"}} or
  {"outcome":{"outcome":"selected","optionId":"<private-option>"}}.
  Never map a legacy Approve to allow_always, or pick one of two allow_once
  choices on the person's behalf.
- PermissionOptionKind is a closed enum in the pinned schema. A structurally
  valid future kind must not become a malformed-frame failure before Koru can
  cancel an unrepresentable offer. Validate option identifiers, labels and kind
  strings first, omit unknown kinds from the typed validation copy, then let the
  pinned schema validate the request envelope and tool call. Retain only known
  choices; an empty result replies cancelled, expires and stops. Malformed or
  duplicate native identifiers still fail protocol validation.
- Unknown notifications may be counted. Unknown requests need a JSON-RPC
  method-not-found response; silently ignoring them strands the caller.
- A cancellation deadline must bound the transport receive itself, even when
  the worker requested Duration::MAX, and be tested on every pass. A continual
  stream of late updates is not evidence that cancellation worked.
- A full content queue must not erase TurnStarted or prevent ApprovalExpired
  and the terminal outcome from being delivered. The adapter reserves approval
  expiration space and a forced terminal slot, then signals an error so the
  existing worker shuts down the process tree.
- HTTP MCP servers are optional. Never send the endpoint or its bearer unless
  mcpCapabilities.http was actually true; the bearer belongs in headers.
- The standard ACP MCP server description has no enabled-tools allowlist.
  Koru's authenticated run endpoint remains the authority for which tools
  the launched session can invoke.
- ToolKind search does not distinguish a filesystem search from a web search.
  It maps to Other unless provider-specific evidence establishes more.
- Authentication is provider-specific. Kimi checks at session/new; Claude's
  wrapper can create a session while signed out. Its additional structured
  auth status is a qualification requirement, not something to infer from an
  advertised list of login methods.

The following provider observations explain the authentication checks.

The pinned Claude wrapper's authStatus capability is an empty advertisement,
not a signed-in value. Its private _auth/status_update may arrive before or
after session/new. account/api_key identify credentials; none is signed out;
external can mean only a configured cloud backend. Unknown or absent identity
must remain Failed under a finite deadline. Its SDK unions session metadata
disallowedTools with wrapper exclusions; Koru preserves caller restrictions
while adding both native child names Agent and Task.

Windows qualification found that passing canonicalize's verbatim namespace
path as a Node ESM entrypoint can exit before initialize. The opt-in fixture
uses its already-absolute manifest ancestors without canonicalizing them.
This does not change production wrapper resolution, which already uses the
profile resolver's Windows-compatible launch path.

Windows npm shims must use the profile renderer for both version probes and
ACP startup; the wrapper route rejects authored shell arguments, and its only
additional probe token is the fixed --version. The stdio process owner still
assigns the cmd/node descendants to its bounded process tree. Cargo can inject
enough DLL search directories to exceed cmd.exe's 8191-character PATH limit;
the shim then reports Node missing even though direct executable resolution
succeeds. The opt-in factory fixture supplies a child-only Node/System32 PATH.
Production never truncates the user's PATH; a cargo-run environment with the
same oversized PATH can hit this Windows launcher limitation.

Named routes now use the same Windows renderer for Gemini --acp, OpenCode/Kimi
acp and Grok agent stdio. Only the closed table's exact tokens may be appended
to a batch command; arbitrary authored arguments remain refused. Grok's
version carrier includes build revision and channel, so taking the last
whitespace token is not a version parser.

Grok's accepted top-level suppression flags are not forwarded to ACP. The
mandatory GROK_SUBAGENTS=0 process environment was tested against conflicting
persisted settings and model-injected native aliases. Its complete inventory
is inside update._meta.tools on available_commands_update, not params._meta
or MCP server status. Koru binds it to the created session and closes on a
later policy contradiction; native tool names are never logged.

## An agent's child needs the effective PATH, not Koru's

Finding the program is half of launching it. A Mac app started from Finder,
the Dock or a login item gets `PATH=/usr/bin:/bin:/usr/sbin:/sbin` from
launchd: no Homebrew, no `~/.local/bin`. `claude-agent-acp` resolved through a
known location still failed its `--version` probe, because it is a
`#!/usr/bin/env node` script and its child's `PATH` had no `node`; every other
agent read "Not installed". A login shell (`zsh -l -c`) restores Homebrew but
not what installers append to `~/.zshrc` (`~/.local/bin`, `~/.opencode/bin`),
and an interactive one (`-i`) can hang on prompts and tty reads. Windows has the
same class of bug in another shape: a long-lived Agent's `PATH` predates every
later install, so a `.cmd` npm shim cannot find a `node` installed after it.

`koru_session_profiles::effective_path` is the one answer: the inherited `PATH`,
then the registry's current `Path` on Windows, or on macOS and Linux the login
shell's `PATH` (captured once per process between markers, three-second
deadline) and the agent installers' directories. `resolve_executable` searches
it, and every child gets it: `with_effective_path` on a `std::process::Command`
(probes, ACP and Codex adapters, `mcp add`, CLI updates), and
`SpawnRequest::environment` for every PTY (terminal, raw agent and *Install*
tabs). Apply it before the launch's own environment, so a `PATH` a route or
fixture sets still wins. A new spawn site that skips it works from a terminal
and fails only in a Finder-launched build.

## OpenCode model selection

OpenCode 1.18.27 returns the model selector in session/new.configOptions, with
id=model, type=select, options and currentValue. Applying
session/set_config_option must acknowledge the selected currentValue before a
prompt. Successful session creation alone does not prove the requested model
was selected. Koru checks the option even when typed optional-field decoding
would otherwise default malformed content away.

Its `models` CLI command emits provider/model IDs on separate lines. --verbose
adds JSON after each ID rather than returning one JSON array. IDs may contain
additional slashes after the provider prefix; preserve the whole value. The
probe uses the real working directory and provider-owned configuration. A
preview list is revalidated against the actual ACP session. See
ADR-393.


### OpenCode resolved providers include implicit connections

In 1.18.27, `models` and the `/provider` connected set include custom autoload
providers. Bedrock can activate merely because AWS_PROFILE exists; free OpenCode
models activate without login. A resolved catalog is not a list of accounts a
person explicitly connected. The Koru chooser intersects with bounded explicit
account metadata (ADR-395). The `providers list` CLI prints display names and
is unsuitable as a stable-ID protocol. OpenCode's `debug paths` resolves its
data directory; OPENCODE_AUTH_CONTENT takes precedence over auth.json. Credential
values are discarded while reading the minimum connection shape.
