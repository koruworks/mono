# Named ACP route fixtures

Gemini fixtures are archived evidence of the retired route. They are no longer
replayed or probed by the active registry; see ADR-391.

These minimal source-shaped responses use the exact reviewed CLI releases:
Gemini0.59.0 (fb0d535af931b27c51e87e5e6ade72905b1e8390),
OpenCode1.18.27 (4b7e19e315cca414121ba1d61523fef74bb3ae8b),
Kimi Code0.42.0 (6954d2c8bf94a5c7fc29cc6ae35b15d042cc4dcb),
and installed Grok1.0.25 (f7e67d6988e2 stable).

Antigravity's ACP server 1.2.1 prints no version, so its folder holds
`digest.txt` (the executable's SHA-256) instead of `version.txt`; unlike the
others its fixtures were reduced from observed frames, including the
signed-out refusal. See `antigravity-1.2.1/README.md`.

Devin CLI 3000.10.48 (`devin 3000.10.48 (fcf7ba39)`, the copy in Devin's
desktop app) was reduced from observed frames too, including a signed-in
`session/new`; its `auth-required.json` is the synthetic shape, because a
signed-out 3000.10.48 starts a session and refuses only at the prompt. See
`devin-3000.10.48/README.md`.

The version.txt files preserve only public exact --version output. JSON is
minimal sanitized protocol evidence, not raw transcript retention. Omitted
optional fields make no capability claim. HTTP/SSE were observed at initialize;
auth-required is a standard synthetic/source-shaped ACP-32000 negative case
exercised through each route, not a captured signed-out refusal from every CLI.
Grok's native cached_token is a private routing constant; account data is absent.
The Grok installed build revision is not available from its public repository;
current public source is supplementary evidence, never an exact-build claim.

Provider notes link release sources and separate these offline shapes from
live authentication, tool registration, child enforcement and cancellation.
Refresh by running only the pinned executable under an isolated root, reducing
in memory to the fields used here, replacing paths/ids/labels/messages, then
replaying through the ordinary adapter tests. Never retain raw frames or keys.

The separate orchestrated-qualification.json records actual Windows G9
registered-route success/cancellation assertions for Claude and Grok.
Their tool-discovery.sanitized.json files contain fixed observation counts and
source-restoration hashes; the measurement tests intentionally failed the old
tool-count bound and do not themselves claim cancellation. Provider notes
describe the canonical matcher, the later normal-build pass, and platform/auth
limits. These evidence files contain no native IDs, account data or tool input.
