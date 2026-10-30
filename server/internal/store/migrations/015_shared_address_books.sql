-- Shared address books (besides each user's personal one in address_book). The owner and
-- admins have full control; others get read, read-write or full access through a share to
-- them or to a group they belong to (the highest permission wins).
CREATE TABLE address_books (
    id          BIGSERIAL PRIMARY KEY,
    name        TEXT NOT NULL,
    note        TEXT NOT NULL DEFAULT '',
    owner_id    BIGINT REFERENCES users(id) ON DELETE SET NULL,
    created_at  TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE TABLE address_book_entries (
    book_id     BIGINT NOT NULL REFERENCES address_books(id) ON DELETE CASCADE,
    device_id   TEXT NOT NULL,
    alias       TEXT NOT NULL DEFAULT '',
    tags        TEXT[] NOT NULL DEFAULT '{}',
    note        TEXT NOT NULL DEFAULT '',
    created_at  TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (book_id, device_id)
);

CREATE TABLE address_book_shares (
    book_id     BIGINT NOT NULL REFERENCES address_books(id) ON DELETE CASCADE,
    user_id     BIGINT REFERENCES users(id) ON DELETE CASCADE,
    group_id    BIGINT REFERENCES device_groups(id) ON DELETE CASCADE,
    permission  TEXT NOT NULL CHECK (permission IN ('read', 'rw', 'full')),
    CHECK ((user_id IS NULL) <> (group_id IS NULL))
);
CREATE UNIQUE INDEX address_book_shares_user_idx ON address_book_shares(book_id, user_id) WHERE user_id IS NOT NULL;
CREATE UNIQUE INDEX address_book_shares_group_idx ON address_book_shares(book_id, group_id) WHERE group_id IS NOT NULL;
