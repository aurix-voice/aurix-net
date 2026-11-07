-- Project-level player deny-list: refused at grant issue and at join. Optional expiry.
CREATE TABLE bans (
    project_id UUID NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
    player_id TEXT NOT NULL,
    reason TEXT NOT NULL DEFAULT '',
    created_by TEXT NOT NULL,
    expires_at TIMESTAMPTZ,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (project_id, player_id)
);
