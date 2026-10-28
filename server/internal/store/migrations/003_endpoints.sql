-- Connection targets served by other products (AnyDesk, TeamViewer, RustDesk) or open
-- protocols (RDP, VNC, SSH). Credentials are sealed with the server vault key.
CREATE TABLE endpoints (
    id           BIGSERIAL PRIMARY KEY,
    name         TEXT NOT NULL,
    provider     TEXT NOT NULL CHECK (provider IN ('anydesk', 'teamviewer', 'rustdesk', 'rdp', 'vnc', 'ssh')),
    address      TEXT NOT NULL,              -- vendor id, or host for rdp/vnc/ssh
    port         INT,
    username     TEXT NOT NULL DEFAULT '',
    secret       BYTEA,                      -- vault-sealed password
    options      JSONB NOT NULL DEFAULT '{}',
    group_id     BIGINT REFERENCES device_groups(id) ON DELETE SET NULL,
    tags         TEXT[] NOT NULL DEFAULT '{}',
    note         TEXT NOT NULL DEFAULT '',
    source       TEXT NOT NULL DEFAULT 'manual',   -- manual | import | teamviewer_api
    external_id  TEXT,
    online       BOOLEAN,                    -- as reported by a vendor API
    synced_at    TIMESTAMPTZ,
    created_by   BIGINT REFERENCES users(id) ON DELETE SET NULL,
    created_at   TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at   TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (source, external_id)
);
CREATE INDEX endpoints_group_idx ON endpoints(group_id);

-- Vendor API connections (config is vault-sealed JSON, e.g. {"token": "..."}).
CREATE TABLE integrations (
    provider      TEXT PRIMARY KEY,
    config        BYTEA NOT NULL,
    enabled       BOOLEAN NOT NULL DEFAULT TRUE,
    group_id      BIGINT REFERENCES device_groups(id) ON DELETE SET NULL,
    last_sync_at  TIMESTAMPTZ,
    last_error    TEXT NOT NULL DEFAULT '',
    cursor        TEXT NOT NULL DEFAULT ''   -- provider-specific sync position
);

-- Sessions that happened in other products, imported from their APIs.
CREATE TABLE external_sessions (
    provider     TEXT NOT NULL,
    external_id  TEXT NOT NULL,
    user_name    TEXT NOT NULL DEFAULT '',
    target_name  TEXT NOT NULL DEFAULT '',
    target_id    TEXT NOT NULL DEFAULT '',
    started_at   TIMESTAMPTZ,
    ended_at     TIMESTAMPTZ,
    raw          JSONB NOT NULL DEFAULT '{}',
    imported_at  TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (provider, external_id)
);
CREATE INDEX external_sessions_started_idx ON external_sessions(started_at DESC);

-- Sessions opened through the RDP/VNC/SSH gateway.
ALTER TABLE remote_sessions ADD COLUMN endpoint_id BIGINT REFERENCES endpoints(id) ON DELETE SET NULL;
ALTER TABLE remote_sessions ALTER COLUMN device_id DROP NOT NULL;
