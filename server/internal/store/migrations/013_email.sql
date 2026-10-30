-- Outgoing mail (login verification codes and admin alerts). The password is vault-sealed.
CREATE TABLE smtp_settings (
    id                SMALLINT PRIMARY KEY DEFAULT 1 CHECK (id = 1),
    host              TEXT NOT NULL DEFAULT '',
    port              INT NOT NULL DEFAULT 587,
    security          TEXT NOT NULL DEFAULT 'starttls' CHECK (security IN ('starttls', 'tls', 'none')),
    ca_cert           TEXT NOT NULL DEFAULT '',
    username          TEXT NOT NULL DEFAULT '',
    password          BYTEA NOT NULL DEFAULT ''::bytea,
    from_addr         TEXT NOT NULL DEFAULT '',
    alert_recipients  TEXT[] NOT NULL DEFAULT '{}',     -- empty = every enabled admin
    alert_new_device  BOOLEAN NOT NULL DEFAULT FALSE,
    alert_lockout     BOOLEAN NOT NULL DEFAULT FALSE,
    enabled           BOOLEAN NOT NULL DEFAULT FALSE,
    updated_at        TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- Second factor by email: a 6-digit code mailed after the password was accepted
-- (used when the account has no authenticator app).
ALTER TABLE users ADD COLUMN email_otp BOOLEAN NOT NULL DEFAULT FALSE;

-- One pending code per user; only its SHA-256 is stored.
CREATE TABLE email_codes (
    user_id     BIGINT PRIMARY KEY REFERENCES users(id) ON DELETE CASCADE,
    code_hash   BYTEA NOT NULL,
    attempts    INT NOT NULL DEFAULT 0,
    sent_at     TIMESTAMPTZ NOT NULL DEFAULT now(),
    expires_at  TIMESTAMPTZ NOT NULL
);
