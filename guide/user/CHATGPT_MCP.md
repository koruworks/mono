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
`--toolsets docs,tasks` (any of `docs`, `files`, `versions`, `tasks`, `chat`)
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
