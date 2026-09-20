CREATE TABLE visits (
    id serial PRIMARY KEY,
    message text NOT NULL,
    created_at timestamptz NOT NULL DEFAULT now()
);

-- Sample historical data to develop against, not just an empty table —
-- large enough that seeding it is genuinely part of what "waiting for db
-- to become healthy" is waiting on, not a health check padded for effect.
INSERT INTO visits (message, created_at)
SELECT
    'seed visit #' || i,
    now() - (random() * interval '365 days')
FROM generate_series(1, 1000000) AS i;

CREATE INDEX idx_visits_created_at ON visits (created_at);
