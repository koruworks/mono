-- AI schema 9 -> 10: where in the reply a local agent's call began (the
-- agent work view, approved 2026-11-02: "Option B is approved, including
-- the schema change").

-- The byte length of the assistant message's Markdown when the row was
-- first written. Text before it was said before the call, text after it
-- after, so a reopened reply can fold the agent's running commentary under
-- "Worked for ..." and keep its final answer in view. Null on every row
-- written before this schema and on Koru's own tool rows: such a reply
-- folds its steps and keeps all of its text as the answer.
ALTER TABLE tool_invocations ADD COLUMN text_offset INTEGER
    CHECK (text_offset IS NULL OR text_offset >= 0);
