-- Recordings now include the session's Opus audio when it was streamed. 'opus' when the
-- uploaded file contains audio packets, '' for silent sessions and older recordings.
ALTER TABLE session_recordings ADD COLUMN audio TEXT NOT NULL DEFAULT '';
