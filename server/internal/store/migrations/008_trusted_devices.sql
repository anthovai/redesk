-- "Remember this device for 30 days" on the 2FA step. Only a SHA-256 of the random token
-- is stored; a valid token lets that user skip TOTP (never the password) until it expires
-- or is revoked.
CREATE TABLE trusted_devices (
    id            BIGSERIAL PRIMARY KEY,
    token_hash    BYTEA NOT NULL UNIQUE,
    user_id       BIGINT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    label         TEXT NOT NULL DEFAULT '',
    user_agent    TEXT NOT NULL DEFAULT '',
    ip            TEXT NOT NULL DEFAULT '',
    last_ip       TEXT NOT NULL DEFAULT '',
    created_at    TIMESTAMPTZ NOT NULL DEFAULT now(),
    last_used_at  TIMESTAMPTZ,
    expires_at    TIMESTAMPTZ NOT NULL
);
CREATE INDEX trusted_devices_user_idx ON trusted_devices(user_id);
