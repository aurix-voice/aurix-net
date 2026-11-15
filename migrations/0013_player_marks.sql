-- Last seen network identity per player (IP / device bans, data export) and bans by IP or device.
CREATE TABLE player_seen (
    project_id UUID NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
    player_id  TEXT NOT NULL,
    ip         TEXT NOT NULL DEFAULT '',
    device     TEXT NOT NULL DEFAULT '',
    name       TEXT NOT NULL DEFAULT '',
    joins      BIGINT NOT NULL DEFAULT 0,
    first_seen TIMESTAMPTZ NOT NULL DEFAULT now(),
    last_seen  TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (project_id, player_id)
);
CREATE TABLE ban_marks (
    project_id UUID NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
    kind       TEXT NOT NULL CHECK (kind IN ('ip', 'device')),
    value      TEXT NOT NULL,
    player_id  TEXT NOT NULL DEFAULT '',
    reason     TEXT NOT NULL DEFAULT '',
    created_by TEXT NOT NULL,
    expires_at TIMESTAMPTZ,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (project_id, kind, value)
);
CREATE INDEX ban_marks_player_idx ON ban_marks (project_id, player_id);
