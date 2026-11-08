-- Each node records its own heartbeat interval so peers judge liveness by it (3 missed beats),
-- not by their own interval: a node configured with a fast heartbeat used to reap healthy
-- slower peers.
ALTER TABLE nodes ADD COLUMN heartbeat_secs INTEGER NOT NULL DEFAULT 30;
