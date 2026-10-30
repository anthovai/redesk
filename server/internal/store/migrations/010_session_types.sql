-- Tunnel and terminal sessions (docs/PROTOCOL.md §11): the session type, and for tunnels the
-- target host:port as seen from the device. Existing rows are screen sessions.
ALTER TABLE remote_sessions ADD COLUMN kind TEXT NOT NULL DEFAULT 'screen';
ALTER TABLE remote_sessions ADD COLUMN target TEXT NOT NULL DEFAULT '';
