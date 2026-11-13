# User guides

What the app does, from the outside. These describe behaviour a person can see,
not the code that produces it.

Start with [profiles and workspaces](USERGUIDE.md#profiles-and-workspaces),
[documents](USERGUIDE.md#documents), and [writing](USERGUIDE.md#writing).
Some experimental features depend on the build and platform; the guide records
their current limitations. [Product flags](../contributing/FEATURE-FLAGS.md)
explain which features a build shows. Source builds and optional service configuration are
covered in the [repository README](../../README.md) and
[CONTRIBUTING.md](../../CONTRIBUTING.md).

| | |
| --- | --- |
| Everything the app does, and every gesture | [USERGUIDE.md](USERGUIDE.md) |
| Experimental task collection beside a document | [Tasks and document](TASK-DOCUMENT.md) |
| Every chord, per platform — **generated, never hand-edited** | [SHORTCUTS.md](../../reference/SHORTCUTS.md) |
| Connect Claude Desktop | [CLAUDE_MCP.md](CLAUDE_MCP.md) |
| Connect ChatGPT or Codex | [CHATGPT_MCP.md](CHATGPT_MCP.md) |
| Connect a local Ollama model | [OLLAMA_LOCAL_MCP.md](OLLAMA_LOCAL_MCP.md) |
| Configure an X account for Socials | [X setup](socials/X-OAUTH.md) |
| Configure a LinkedIn profile or Page | [LinkedIn setup](socials/LINKEDIN.md) |
| Configure a Reddit account | [Reddit setup](socials/REDDIT.md) |

`SHORTCUTS.md` is generated from `koru-keys`' `REGISTRY`; editing it by hand is
undone by the next `cargo test -p koru-keys`, which fails when the two disagree.
Change the registry instead, then regenerate with `KORU_UPDATE_SHORTCUTS=1`.

The MCP guides describe connecting your own client. The
[MCP model and tool reference](../../crates/koru-mcp-server/MCP.md) describes
the document operations available through that connection.
