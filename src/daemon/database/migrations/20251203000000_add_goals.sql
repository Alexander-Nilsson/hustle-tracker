CREATE TABLE IF NOT EXISTS daily_goals (
    id SERIAL PRIMARY KEY,
    target_minutes BIGINT NOT NULL DEFAULT 120,
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
INSERT INTO daily_goals (target_minutes) SELECT 120 WHERE NOT EXISTS (SELECT 1 FROM daily_goals);
