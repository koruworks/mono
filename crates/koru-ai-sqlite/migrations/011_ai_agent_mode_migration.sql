-- AI schema 10 -> 11: a local agent chat's approval mode and Fast mode
-- (koru-ai D7 and D11, approved 2026-11-02: "AI schema 10 -> 11").

-- The agent's own id for the mode the chat's next message runs in: Claude's
-- `acceptEdits`, OpenCode's `plan`, Koru's `ask` for Codex. An open token,
-- like the working folder, with no CHECK: the agents name their modes, and
-- a mode an agent no longer offers is refused when the message is sent,
-- never here. Null on every chat written before this schema and on every
-- chat that never chose one: the agent's own default mode.
ALTER TABLE chats ADD COLUMN agent_mode TEXT;

-- Whether the chat's next message runs in the agent's Fast mode, for a model
-- that offers it. Null on every earlier chat and every chat that never chose:
-- the model's own default.
ALTER TABLE chats ADD COLUMN agent_fast INTEGER
    CHECK (agent_fast IS NULL OR agent_fast IN (0, 1));
