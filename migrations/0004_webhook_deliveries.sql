-- One row per webhook emission (after retries): what was sent, how it went, how long it took.
CREATE TABLE webhook_deliveries (
    id UUID PRIMARY KEY,
    webhook_id UUID NOT NULL REFERENCES webhooks(id) ON DELETE CASCADE,
    project_id UUID NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
    event TEXT NOT NULL,
    ok BOOLEAN NOT NULL,
    status SMALLINT,
    attempts SMALLINT NOT NULL,
    latency_ms INTEGER NOT NULL,
    error TEXT,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX webhook_deliveries_hook_idx ON webhook_deliveries (webhook_id, created_at DESC);
CREATE INDEX webhook_deliveries_created_idx ON webhook_deliveries (created_at);
