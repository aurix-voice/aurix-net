-- Optional per-key source allowlist: when non-empty, REST calls with the key are accepted
-- only from these networks (client IP after trusted-proxy resolution).
ALTER TABLE api_keys ADD COLUMN allowed_cidrs TEXT[] NOT NULL DEFAULT '{}';
