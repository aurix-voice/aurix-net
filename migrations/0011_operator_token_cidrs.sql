-- Operator tokens: optional source allowlist and the address they were last used from.
ALTER TABLE operator_tokens ADD COLUMN allowed_cidrs TEXT[] NOT NULL DEFAULT '{}';
ALTER TABLE operator_tokens ADD COLUMN last_used_ip TEXT;
