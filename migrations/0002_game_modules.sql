-- Server-side game logic modules (WebAssembly) for sync rooms, per project, referenced from
-- room props as {"logic": "<name>"}.
CREATE TABLE game_modules (
    project_id UUID NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
    name TEXT NOT NULL,
    hash TEXT NOT NULL,
    wasm BYTEA NOT NULL,
    size INTEGER NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (project_id, name)
);
