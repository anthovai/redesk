-- Devices enrolled by signing in from the desktop app belong to that user
-- (and land in the user's address book).
ALTER TABLE enroll_tokens ADD COLUMN owner_id BIGINT REFERENCES users(id) ON DELETE CASCADE;
ALTER TABLE devices ADD COLUMN owner_id BIGINT REFERENCES users(id) ON DELETE SET NULL;
