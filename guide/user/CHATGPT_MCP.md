# Connect ChatGPT or Codex to Koru

The ChatGPT desktop/Codex host can launch a local stdio MCP server. ChatGPT on
the web cannot launch a process on your computer; use OpenAI's secure MCP tunnel
for that case.

## Prepare Koru

Build Koru and leave Store running on the same isolated root the MCP command
will use:

```bat
koru.bat run-all --mcp
```

## ChatGPT desktop

In ChatGPT desktop, open **Settings → MCP servers → Add server**, choose
**STDIO**, and configure:

```text
Command: C:\absolute\path\to\repo\target\debug\koru-mcp.exe
Arguments: serve --stdio --root C:\absolute\path\to\repo\.dev-data\history-smoke
```

Restart ChatGPT after saving. The desktop host owns the bridge process.

These commands serve every Koru tool. To serve fewer, append
`--toolsets docs,tasks` (any of `docs`, `files`, `versions`, `tasks`, `chat`, `bots`)
to the arguments; see
[Serve fewer tools](CLAUDE_MCP.md#serve-fewer-tools).

The equivalent shared Codex configuration in `~/.codex/config.toml` is:

```toml
[mcp_servers.koru]
command = "C:\\absolute\\path\\to\\repo\\target\\debug\\koru-mcp.exe"
args = ["serve", "--stdio", "--root", "C:\\absolute\\path\\to\\repo\\.dev-data\\history-smoke"]
```

Or add it from Codex CLI:

```powershell
codex mcp add koru -- C:\absolute\path\to\repo\target\debug\koru-mcp.exe serve --stdio --root C:\absolute\path\to\repo\.dev-data\history-smoke
```

## ChatGPT web through a secure tunnel

Create an OpenAI MCP tunnel and initialize its local stdio command:

```powershell
$env:CONTROL_PLANE_API_KEY = "sk-..."
tunnel-client init --sample sample_mcp_stdio_local --profile koru-local --tunnel-id tunnel_... --mcp-command "C:\absolute\path\to\repo\target\debug\koru-mcp.exe serve --stdio --root C:\absolute\path\to\repo\.dev-data\history-smoke"
tunnel-client doctor --profile koru-local --explain
tunnel-client run --profile koru-local
```

In ChatGPT web Developer Mode, create an app using that tunnel. Keep the tunnel
client and Koru Store running while the web app is in use. Do not expose the
local Store or stdio executable directly to the internet.

## Ask a Bot to work

Serve `--toolsets docs,bots` to let the connected assistant discover Bots,
submit work, read their answers, and stop an accepted task. For example:

> Find my CFO Bot and ask it for a summary of potential VCs. Keep the request
> receipt, check its progress, and show me its answer and any saved pages.

The request appears in the Bot's main conversation in Koru. You can follow it
and use the normal Stop control there. The task continues if the assistant
disconnects. An assistant retrying an uncertain submission must preserve the
original request UUID and input. Paused Bots refuse new tasks; a main
conversation in Trash must be restored before it can receive a task.

The tools return the Bot's actual answer, including answers without a saved
page. Longer answers and saved-page lists have continuation cursors. See the
[Bot task contract](../../crates/koru-mcp-server/MCP.md#bot-task-tools) for scopes,
retry rules, and result limits. This is a trusted local connection under your
OS user account; it does not isolate individual assistant applications.

## Smoke test

> Use Koru to find the History smoke document. Read it, create a new document
> called "ChatGPT linked page", and insert a /document reference to that new
> document at the end of the first paragraph. Read the source again and confirm
> the returned attribute routes to the new document ID.

If Store reports `store_unreachable`, no Store is listening on that channel and
root: check that Store and the MCP command use the same absolute root. The
bridge keeps trying, so starting Store is enough — there is no need to restart
the host. (`storage_failed` is a different answer: Store is there and its
storage failed.) If an old schema appears, fully restart the desktop host so
its child bridge exits. Revision conflicts require a fresh read and retry.

References: [OpenAI MCP setup](https://learn.chatgpt.com/docs/extend/mcp), [OpenAI secure MCP tunnels](https://developers.openai.com/api/docs/guides/secure-mcp-tunnels), [ChatGPT Developer Mode](https://developers.openai.com/api/docs/guides/developer-mode)
