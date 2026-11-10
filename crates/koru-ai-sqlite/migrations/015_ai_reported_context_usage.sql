-- AI schema 14 -> 15: provider-reported active context for each attempt.
-- NULL means the agent has not reported a usable pair; response usage stays separate.
ALTER TABLE inference_attempts ADD COLUMN context_used_tokens INTEGER
    CHECK (context_used_tokens IS NULL OR context_used_tokens >= 0);
ALTER TABLE inference_attempts ADD COLUMN context_limit_tokens INTEGER
    CHECK (context_limit_tokens IS NULL OR context_limit_tokens > 0);
