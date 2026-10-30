-- Sign-in against an LDAP directory or Active Directory (one per server). The service
-- account password is vault-sealed. Connections are always TLS (ldaps:// or StartTLS).
CREATE TABLE ldap_settings (
    id             SMALLINT PRIMARY KEY DEFAULT 1 CHECK (id = 1),
    url            TEXT NOT NULL DEFAULT '',
    ca_cert        TEXT NOT NULL DEFAULT '',            -- extra trusted CA (PEM), e.g. an internal AD CA
    bind_dn        TEXT NOT NULL DEFAULT '',
    bind_password  BYTEA NOT NULL DEFAULT ''::bytea,   -- vault-sealed
    base_dn        TEXT NOT NULL DEFAULT '',
    user_filter    TEXT NOT NULL DEFAULT '',            -- {username} is replaced (escaped)
    email_attr     TEXT NOT NULL DEFAULT 'mail',
    name_attr      TEXT NOT NULL DEFAULT 'displayName',
    group_filter   TEXT NOT NULL DEFAULT '',            -- optional group search, {dn} is replaced (escaped)
    admin_groups   TEXT[] NOT NULL DEFAULT '{}',        -- members become admins
    user_groups    TEXT[] NOT NULL DEFAULT '{}',        -- if set, only members may sign in
    auto_create    BOOLEAN NOT NULL DEFAULT TRUE,
    enabled        BOOLEAN NOT NULL DEFAULT FALSE,
    updated_at     TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- Where a user's password is checked. 'ldap' accounts never use the local password hash.
ALTER TABLE users ADD COLUMN auth_source TEXT NOT NULL DEFAULT 'local' CHECK (auth_source IN ('local', 'ldap'));
ALTER TABLE users ADD COLUMN external_dn TEXT NOT NULL DEFAULT '';
