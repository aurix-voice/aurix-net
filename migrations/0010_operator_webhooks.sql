-- Fleet-level alert webhooks for operators (node stale/draining, project at a usage cap,
-- failing project webhooks, suspensions). Same signing and delivery log as project webhooks.
CREATE TABLE operator_webhooks (
    id UUID PRIMARY KEY,
    url TEXT NOT NULL,
    secret TEXT NOT NULL,
    events TEXT[] NOT NULL DEFAULT '{}',
    created_by TEXT NOT NULL DEFAULT '',
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE TABLE operator_webhook_deliveries (
    id UUID PRIMARY KEY,
    webhook_id UUID NOT NULL REFERENCES operator_webhooks(id) ON DELETE CASCADE,
    event TEXT NOT NULL,
    ok BOOLEAN NOT NULL,
    status SMALLINT,
    attempts SMALLINT NOT NULL,
    latency_ms INTEGER NOT NULL,
    error TEXT,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX operator_webhook_deliveries_hook_idx ON operator_webhook_deliveries (webhook_id, created_at DESC);
CREATE INDEX operator_webhook_deliveries_created_idx ON operator_webhook_deliveries (created_at);
