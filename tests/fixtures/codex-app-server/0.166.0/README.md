# Codex 0.166.0 evidence

Admitted on Windows on 2026-11-02 with the normally installed `codex`, when
Codex came back to Koru AI's agent picker: the installation had moved on to
0.166.0, which the allowlist did not hold, so the picker's Codex would have
read *Needs a supported version* again. Koru AI's approval modes (D7) and Fast
mode (D11) were already worked out against this version. The generated v2
schema SHA-256 is
`e025e54fb11354eb82767ea040b40efa17b34dfc706c9433a6115b5f4771c465`
(generated twice, same digest). Reproduce it with
`scripts/dev/probe-codex-app-server.ps1`.

**What was checked.** The schema carries every method, notification, approval
request and input shape the adapter depends on (the `required_*` lists in
`schema-evidence.json`), and `model/rerouted` is still there. Its server
requests are exactly the 0.165.0 set; every additive notification listed for
0.165.0 is still there and still ignored. A model still lists
`supportedReasoningEfforts` and `serviceTiers`; `thread/start` and
`turn/start` still take `approvalsReviewer` and `serviceTier`, and
`turn/start` takes the `effort` a chosen level now rides on. The opt-in
no-prompt factory handshake
(`live_codex_no_prompt_handshake_lists_models_when_opted_in`, under
`KORU_LIVE_PROVIDER_SMOKE`) completed `initialize` and paged `model/list`
against the installed CLI: 8 models in 1.0 s, no thread, no prompt.
`codex features list` still names `multi_agent_v2` (stable, `false` by
default), so delegated launches set both child-agent switches off as before;
`multi_agent_v2_dynamic_tools` is still under development and off.

**What was not.** There are no replay fixtures for this version, and no model
turn, approval, interruption or image turn has been recorded under 0.166.0.
The unhandled server requests are still unhandled, and the many additive
notifications are ignored without having been observed in a turn.
