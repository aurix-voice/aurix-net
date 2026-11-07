-- Named operator tokens: revocable, optionally expiring, and named in the audit log.
-- The bootstrap `auth.admin_token` from the config keeps working as actor "operator".
CREATE TABLE operator_tokens (
    id UUID PRIMARY KEY,
    name TEXT NOT NULL,
    token_hash TEXT NOT NULL UNIQUE,
    prefix TEXT NOT NULL,
    created_by TEXT NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    expires_at TIMESTAMPTZ,
    revoked_at TIMESTAMPTZ,
    last_used_at TIMESTAMPTZ
);
