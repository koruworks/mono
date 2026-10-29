# Codex 0.163.0 evidence

Admitted on Windows on 2026-10-27 with the normally installed `codex`, on the
maintainers' instruction (UPDATEAGENTS: Koru now offers to update the agent
CLIs it finds, so the pins were widened to what is installed). The installation
had moved on from 0.158.0 and **Codex (Embedded)** read *Needs a supported
version* again. The generated v2 schema SHA-256 is
`d204b96d8560d56600127423b89df9db471b3e22338e8809bb91d5a952998978`
(generated twice, same digest). Reproduce it with
`scripts/dev/probe-codex-app-server.ps1`.

**What was checked.** The schema carries every method, notification, approval
request and input shape the adapter depends on (the `required_*` lists in
`schema-evidence.json`), and `model/rerouted` is still there. Its server
requests are the 0.158.0 set plus `openai/form`, which — like the five
0.156.1 added — is unhandled and would reach a person as a generic approval.
The opt-in no-prompt factory handshake
(`live_codex_no_prompt_handshake_lists_models_when_opted_in`, under
`KORU_LIVE_PROVIDER_SMOKE`) completed `initialize` and paged `model/list`
against the installed CLI: 8 models in 2.0 s, no thread, no prompt.
`codex features list` still names `multi_agent_v2` (stable, `false` by
default) and accepts `-c features.multi_agent_v2=false`, so delegated launches
on 0.163.0 set both child-agent switches off, as on 0.153.4 through 0.158.0.
A new `multi_agent_v2_dynamic_tools` switch is under development and off.

**What was not.** There are no replay fixtures for this version: the 0.154.0
set remains the adapter's boundary fixture, and no model turn, approval,
interruption or image turn has been recorded under 0.163.0. The six unhandled
server requests above are still unhandled. 0.163.0 adds many notifications the
adapter ignores (auto-approval review, realtime threads, goals, moderation
metadata; listed in `schema-evidence.json`); none was observed in a turn,
because no turn was run. The first real turn is the acceptance's.
