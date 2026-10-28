-- Device groups: which users may connect to which devices when access_policy = "groups".
CREATE TABLE device_groups (
    id           BIGSERIAL PRIMARY KEY,
    name         TEXT NOT NULL UNIQUE,
    description  TEXT NOT NULL DEFAULT '',
    created_at   TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE TABLE group_members (
    group_id  BIGINT NOT NULL REFERENCES device_groups(id) ON DELETE CASCADE,
    user_id   BIGINT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    PRIMARY KEY (group_id, user_id)
);
CREATE INDEX group_members_user_idx ON group_members(user_id);

ALTER TABLE devices ADD COLUMN group_id BIGINT REFERENCES device_groups(id) ON DELETE SET NULL;
-- Devices enrolled with this token land in this group.
ALTER TABLE enroll_tokens ADD COLUMN group_id BIGINT REFERENCES device_groups(id) ON DELETE SET NULL;
