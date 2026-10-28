-- Single sign-on through an OpenID Connect provider (one per server).
CREATE TABLE oidc_settings (
    id               SMALLINT PRIMARY KEY DEFAULT 1 CHECK (id = 1),
    name             TEXT NOT NULL DEFAULT '',
    issuer           TEXT NOT NULL DEFAULT '',
    client_id        TEXT NOT NULL DEFAULT '',
    client_secret    BYTEA NOT NULL DEFAULT ''::bytea, -- vault-sealed
    allowed_domains  TEXT[] NOT NULL DEFAULT '{}',
    auto_create      BOOLEAN NOT NULL DEFAULT FALSE,
    enabled          BOOLEAN NOT NULL DEFAULT FALSE,
    updated_at       TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- Vetted agent builds offered to desktop apps (never installed silently).
CREATE TABLE agent_releases (
    id          BIGSERIAL PRIMARY KEY,
    version     TEXT NOT NULL,
    os          TEXT NOT NULL CHECK (os IN ('windows', 'macos', 'linux')),
    arch        TEXT NOT NULL CHECK (arch IN ('x86_64', 'aarch64', 'universal')),
    filename    TEXT NOT NULL,
    size        BIGINT NOT NULL CHECK (size > 0),
    sha256      TEXT NOT NULL CHECK (sha256 ~ '^[0-9a-f]{64}$'),
    notes       TEXT NOT NULL DEFAULT '',
    enabled     BOOLEAN NOT NULL DEFAULT FALSE,
    created_by  BIGINT REFERENCES users(id) ON DELETE SET NULL,
    created_at  TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (version, os, arch)
);
CREATE INDEX agent_releases_enabled_idx ON agent_releases(os) WHERE enabled;
