CREATE TABLE projects (
    id UUID PRIMARY KEY,
    name TEXT NOT NULL,
    plan TEXT NOT NULL DEFAULT 'self_hosted',
    limits JSONB NOT NULL DEFAULT '{}'::jsonb,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    disabled_at TIMESTAMPTZ
);

CREATE TABLE api_keys (
    id UUID PRIMARY KEY,
    project_id UUID NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
    key_hash TEXT NOT NULL UNIQUE,
    prefix TEXT NOT NULL,
    name TEXT NOT NULL DEFAULT '',
    scopes TEXT[] NOT NULL DEFAULT '{}',
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    last_used_at TIMESTAMPTZ,
    revoked_at TIMESTAMPTZ
);
CREATE INDEX api_keys_project_idx ON api_keys(project_id);

CREATE TABLE regions (
    id TEXT PRIMARY KEY,
    display_name TEXT NOT NULL,
    dns_label TEXT NOT NULL,
    enabled BOOLEAN NOT NULL DEFAULT true
);

CREATE TABLE nodes (
    id UUID PRIMARY KEY,
    region TEXT NOT NULL REFERENCES regions(id),
    public_host TEXT NOT NULL,
    quic_port INT NOT NULL,
    http_url TEXT NOT NULL,
    cert_sha256 TEXT NOT NULL,
    version TEXT NOT NULL,
    rooms INT NOT NULL DEFAULT 0,
    players INT NOT NULL DEFAULT 0,
    load_factor REAL NOT NULL DEFAULT 0,
    draining BOOLEAN NOT NULL DEFAULT false,
    started_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    last_heartbeat TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX nodes_region_idx ON nodes(region);

CREATE TABLE rooms (
    id UUID PRIMARY KEY,
    project_id UUID NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
    node_id UUID REFERENCES nodes(id) ON DELETE SET NULL,
    region TEXT NOT NULL,
    name TEXT NOT NULL,
    mode TEXT NOT NULL,
    state TEXT NOT NULL DEFAULT 'open',
    max_players INT NOT NULL,
    visible BOOLEAN NOT NULL DEFAULT true,
    open BOOLEAN NOT NULL DEFAULT true,
    schema_hash TEXT,
    props JSONB NOT NULL DEFAULT '{}'::jsonb,
    lobby_props TEXT[] NOT NULL DEFAULT '{}',
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    closed_at TIMESTAMPTZ
);
CREATE UNIQUE INDEX rooms_project_name_open_idx ON rooms(project_id, name) WHERE state = 'open';
CREATE INDEX rooms_project_state_idx ON rooms(project_id, state);

CREATE TABLE schemas (
    project_id UUID NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
    hash TEXT NOT NULL,
    version TEXT NOT NULL DEFAULT '',
    idl TEXT NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (project_id, hash)
);

CREATE TABLE webhooks (
    id UUID PRIMARY KEY,
    project_id UUID NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
    url TEXT NOT NULL,
    secret TEXT NOT NULL,
    events TEXT[] NOT NULL DEFAULT '{}',
    enabled BOOLEAN NOT NULL DEFAULT true,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE TABLE usage_hours (
    project_id UUID NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
    hour TIMESTAMPTZ NOT NULL,
    metric TEXT NOT NULL,
    quantity DOUBLE PRECISION NOT NULL DEFAULT 0,
    PRIMARY KEY (project_id, hour, metric)
);

CREATE TABLE audit (
    id BIGSERIAL PRIMARY KEY,
    project_id UUID,
    actor TEXT NOT NULL,
    action TEXT NOT NULL,
    target TEXT NOT NULL DEFAULT '',
    details JSONB NOT NULL DEFAULT '{}'::jsonb,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

INSERT INTO regions (id, display_name, dns_label, enabled) VALUES
    ('local', 'Local', 'local', true),
    ('eu_central', 'Europe (Central)', 'eu-central', true),
    ('eu_west', 'Europe (West)', 'eu-west', false),
    ('us_east', 'US (East)', 'us-east', false),
    ('us_west', 'US (West)', 'us-west', false),
    ('ap_southeast', 'Asia Pacific (Southeast)', 'ap-southeast', false),
    ('ap_northeast', 'Asia Pacific (Northeast)', 'ap-northeast', false),
    ('sa_east', 'South America (East)', 'sa-east', false),
    ('me_central', 'Middle East (Central)', 'me-central', false);
