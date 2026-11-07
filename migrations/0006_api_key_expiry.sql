-- Optional expiry for API keys: expired keys fail authentication like revoked ones.
ALTER TABLE api_keys ADD COLUMN expires_at TIMESTAMPTZ;
