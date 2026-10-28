-- Recordings of Aegiron remote-control sessions. The server cannot see session content (it is
-- end-to-end encrypted), so the controlled device records what it streams and uploads the file
-- afterwards. A row is created as 'pending' when a recorded session starts; token_hash is the
-- one-time upload credential handed to the device over its signaling connection.
CREATE TABLE session_recordings (
    id           BIGSERIAL PRIMARY KEY,
    sid          TEXT NOT NULL UNIQUE,
    device_id    TEXT NOT NULL,
    user_id      BIGINT REFERENCES users(id) ON DELETE SET NULL,
    viewer_ip    TEXT NOT NULL DEFAULT '',
    token_hash   BYTEA,
    status       TEXT NOT NULL DEFAULT 'pending' CHECK (status IN ('pending', 'ready')),
    -- The device reported that it started recording (sessions that never got past
    -- authentication stay false and are purged without being listed).
    recording_started BOOLEAN NOT NULL DEFAULT FALSE,
    created_at   TIMESTAMPTZ NOT NULL DEFAULT now(),
    uploaded_at  TIMESTAMPTZ,
    duration_ms  BIGINT NOT NULL DEFAULT 0,
    size_bytes   BIGINT NOT NULL DEFAULT 0,
    sha256       TEXT NOT NULL DEFAULT '',
    codec        TEXT NOT NULL DEFAULT '',
    frames       INTEGER NOT NULL DEFAULT 0
);
CREATE INDEX session_recordings_created_idx ON session_recordings(created_at DESC);
CREATE INDEX session_recordings_device_idx ON session_recordings(device_id);
