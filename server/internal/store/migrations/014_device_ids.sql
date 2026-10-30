-- Custom device IDs: when an admin changes a device's ID, the old one keeps pointing at the
-- device until its agent has picked up the new ID (so an offline agent learns it on its next
-- registration), and nobody else can register with it meanwhile.
CREATE TABLE device_id_changes (
    old_id      TEXT PRIMARY KEY,
    device_id   TEXT NOT NULL REFERENCES devices(id) ON UPDATE CASCADE ON DELETE CASCADE,
    changed_by  BIGINT REFERENCES users(id) ON DELETE SET NULL,
    changed_at  TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX device_id_changes_device_idx ON device_id_changes(device_id);
