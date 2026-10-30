-- Strategies: admin-defined bundles of agent settings (approval mode, permissions, direct
-- connections, recording, ...). A strategy is assigned to a device group or to one device;
-- the device's own assignment wins over its group's. Agents receive the effective strategy when
-- they register and whenever it changes, and lock those settings locally. Devices without a
-- strategy keep their local settings.
CREATE TABLE strategies (
    id           BIGSERIAL PRIMARY KEY,
    name         TEXT NOT NULL UNIQUE,
    description  TEXT NOT NULL DEFAULT '',
    settings     JSONB NOT NULL DEFAULT '{}',
    created_at   TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at   TIMESTAMPTZ NOT NULL DEFAULT now()
);

ALTER TABLE device_groups ADD COLUMN strategy_id BIGINT REFERENCES strategies(id) ON DELETE SET NULL;
ALTER TABLE devices ADD COLUMN strategy_id BIGINT REFERENCES strategies(id) ON DELETE SET NULL;
