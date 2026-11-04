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

`turn/steer`, which a steer from Koru AI's queue rides (a message sent into
the running turn), was added to `required_client_methods` on 2026-11-02 after
regenerating the schema from the installed 0.166.0: the same digest, and
`turn/steer` with `TurnSteerParams` is in it.

**What was not.** There are no replay fixtures for this version, and no model
turn, approval, interruption or image turn has been recorded under 0.166.0.
The unhandled server requests are still unhandled, and the many additive
notifications are ignored without having been observed in a turn.

**An agent's questions** (koru-ai D108, session protocol v21, 2026-11-03).
`request-user-input.server.json` is an `item/tool/requestUserInput` server
request and `request-user-input.client.json` the
`ToolRequestUserInputResponse` the adapter answers it with, both in the shapes
of the schema regenerated from the installed 0.166.0 into a scratch folder
(`ToolRequestUserInputParams.json` and `ToolRequestUserInputResponse.json`,
identical to the copies the session lead generated): three questions — one
with options and *Other*, one with no options, one secret — and an answer
each. They are written from the schema, not recorded from a turn: no model
turn was started, and how a live Codex reads an *Other* answer or a skip
(`{"answers": {}}`) is not established.

**The root cause, corrected (2026-11-03).** The reply that read *Three
questions are ready* and showed none was not this server request answered
`-32601`. A live run with `gpt-6.1-sol` never called the synchronous tool: it
called `request_user_input_async`, which answers its model at once and adds an
`agentMessage` thread item with `delivery: "async"` and `questions` — in the
stable 0.166.0 schema as `AgentMessageThreadItem.delivery` and
`AsyncUserInputQuestion {title, options?}` — that streams no delta. The adapter
took a message's words only from deltas and dropped the completed item, so
the questions never reached the chat. `request-user-input-async.server.json`
is that item's `item/completed` notification, written in the exact 0.166.0
shape (`ItemCompletedNotification`: `threadId`, `turnId`, `completedAtMs`,
`item`) with neutral content — not recorded from the run — and
`request-user-input-async.client.json` is the input item the adapter sends with
the answers as the person's next message: Codex's own reply envelope
(`<send_user_message_question_reply>` around a compact JSON array of
`{answer, question, questionItemId}`, from `codex-rs/context-fragments/src/
answered_question.rs` at `rust-v0.166.0`). `schema-evidence.json` now lists
`item/tool/requestUserInput` as handled for a session that draws questions,
and the async item as a question item.
