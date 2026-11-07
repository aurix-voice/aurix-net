-- Webhook secret rotation: the previous secret keeps signing (second header) until it expires.
ALTER TABLE webhooks ADD COLUMN previous_secret TEXT;
ALTER TABLE webhooks ADD COLUMN previous_secret_expires_at TIMESTAMPTZ;
