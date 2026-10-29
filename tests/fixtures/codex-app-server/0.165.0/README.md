# Codex 0.165.0 evidence

Admitted on Windows on 2026-10-28 with the normally installed `codex`, on the
maintainers' instruction: the first **Update** press on Koru's update card
(ADR-469) moved the installation from 0.163.0, and **Codex (Embedded)** read
*Needs a supported version* again. The generated v2 schema SHA-256 is
`38027ec8a6fffc2422b674cab13dc1a05c26e13a48349c46a274609d52090e35`
(generated twice, same digest). Reproduce it with
`scripts/dev/probe-codex-app-server.ps1`.

**What was checked.** The schema carries every method, notification, approval
request and input shape the adapter depends on (the `required_*` lists in
`schema-evidence.json`), and `model/rerouted` is still there. Its server
requests are the 0.163.0 set without `openai/form`, plus the legacy
`applyPatchApproval` and `execCommandApproval`; the unhandled ones reach a
person as a generic approval. The opt-in no-prompt factory handshake
(`live_codex_no_prompt_handshake_lists_models_when_opted_in`, under
`KORU_LIVE_PROVIDER_SMOKE`) completed `initialize` and paged `model/list`
against the installed CLI: 8 models in 3.3 s, no thread, no prompt.
`codex features list` still names `multi_agent_v2` (stable, `false` by
default), so delegated launches set both child-agent switches off as before;
`multi_agent_v2_dynamic_tools` is still under development and off.

**What was not.** There are no replay fixtures for this version, and no model
turn, approval, interruption or image turn has been recorded under 0.165.0.
The unhandled server requests are still unhandled, and the many additive
notifications are ignored without having been observed in a turn.
