package store

import (
	"context"
	"time"

	"github.com/jackc/pgx/v5"
)

// Address book permissions, lowest to highest.
const (
	BookRead = "read" // see entries
	BookRW   = "rw"   // also add, edit and remove entries
	BookFull = "full" // also rename, delete and share the book
)

// BookPermRank orders permissions; 0 = no access.
func BookPermRank(p string) int {
	switch p {
	case BookRead:
		return 1
	case BookRW:
		return 2
	case BookFull:
		return 3
	}
	return 0
}

// AddressBook is a shared address book as seen by one user.
type AddressBook struct {
	ID         int64     `json:"id"`
	Name       string    `json:"name"`
	Note       string    `json:"note"`
	OwnerID    *int64    `json:"owner_id"`
	OwnerEmail *string   `json:"owner_email"`
	CreatedAt  time.Time `json:"created_at"`
	EntryCount int       `json:"entry_count"`
	// Permission of the requesting user: read | rw | full.
	Permission string `json:"permission"`
}

// BookShare grants a user or a group (its members) access to a book.
type BookShare struct {
	UserID     *int64 `json:"user_id"`
	GroupID    *int64 `json:"group_id"`
	Permission string `json:"permission"`
}

// BookEntry is a device in a shared address book.
type BookEntry struct {
	DeviceID  string    `json:"device_id"`
	Alias     string    `json:"alias"`
	Tags      []string  `json:"tags"`
	Note      string    `json:"note"`
	CreatedAt time.Time `json:"created_at"`
	Device    *Device   `json:"device"` // nil when the id has never registered
}

// bookPermSQL computes the permission of user $1 (admin flag $2) on book b.
const bookPermSQL = `CASE WHEN $2 OR b.owner_id=$1 THEN 'full' ELSE COALESCE((
	SELECT sh.permission FROM address_book_shares sh
	WHERE sh.book_id=b.id AND (sh.user_id=$1 OR sh.group_id IN (SELECT group_id FROM group_members WHERE user_id=$1))
	ORDER BY CASE sh.permission WHEN 'full' THEN 3 WHEN 'rw' THEN 2 ELSE 1 END DESC LIMIT 1), '') END`

// ListAddressBooks returns the shared books userID can see (admins see all).
func (s *Store) ListAddressBooks(ctx context.Context, userID int64, admin bool) ([]*AddressBook, error) {
	rows, err := s.DB.Query(ctx, `SELECT * FROM (SELECT b.id, b.name, b.note, b.owner_id, u.email, b.created_at,
			(SELECT count(*) FROM address_book_entries e WHERE e.book_id=b.id), `+bookPermSQL+` AS perm
		FROM address_books b LEFT JOIN users u ON u.id=b.owner_id) x WHERE perm <> '' ORDER BY name, id`, userID, admin)
	if err != nil {
		return nil, err
	}
	defer rows.Close()
	out := []*AddressBook{}
	for rows.Next() {
		var b AddressBook
		if err := rows.Scan(&b.ID, &b.Name, &b.Note, &b.OwnerID, &b.OwnerEmail, &b.CreatedAt, &b.EntryCount, &b.Permission); err != nil {
			return nil, err
		}
		out = append(out, &b)
	}
	return out, rows.Err()
}

// GetAddressBook returns one book with the user's permission ("" = no access).
func (s *Store) GetAddressBook(ctx context.Context, id, userID int64, admin bool) (*AddressBook, error) {
	var b AddressBook
	err := s.DB.QueryRow(ctx, `SELECT b.id, b.name, b.note, b.owner_id, u.email, b.created_at,
			(SELECT count(*) FROM address_book_entries e WHERE e.book_id=b.id), `+bookPermSQL+`
		FROM address_books b LEFT JOIN users u ON u.id=b.owner_id WHERE b.id=$3`, userID, admin, id).
		Scan(&b.ID, &b.Name, &b.Note, &b.OwnerID, &b.OwnerEmail, &b.CreatedAt, &b.EntryCount, &b.Permission)
	if err != nil {
		return nil, notFound(err)
	}
	return &b, nil
}

func (s *Store) CreateAddressBook(ctx context.Context, name, note string, ownerID int64) (int64, error) {
	var id int64
	err := s.DB.QueryRow(ctx, `INSERT INTO address_books(name, note, owner_id) VALUES ($1,$2,$3) RETURNING id`, name, note, ownerID).Scan(&id)
	return id, err
}

func (s *Store) UpdateAddressBook(ctx context.Context, id int64, name, note string) error {
	tag, err := s.DB.Exec(ctx, `UPDATE address_books SET name=$2, note=$3 WHERE id=$1`, id, name, note)
	if err == nil && tag.RowsAffected() == 0 {
		return ErrNotFound
	}
	return err
}

func (s *Store) DeleteAddressBook(ctx context.Context, id int64) error {
	tag, err := s.DB.Exec(ctx, `DELETE FROM address_books WHERE id=$1`, id)
	if err == nil && tag.RowsAffected() == 0 {
		return ErrNotFound
	}
	return err
}

func (s *Store) ListBookShares(ctx context.Context, bookID int64) ([]BookShare, error) {
	rows, err := s.DB.Query(ctx, `SELECT user_id, group_id, permission FROM address_book_shares WHERE book_id=$1
		ORDER BY group_id NULLS FIRST, user_id`, bookID)
	if err != nil {
		return nil, err
	}
	defer rows.Close()
	out := []BookShare{}
	for rows.Next() {
		var sh BookShare
		if err := rows.Scan(&sh.UserID, &sh.GroupID, &sh.Permission); err != nil {
			return nil, err
		}
		out = append(out, sh)
	}
	return out, rows.Err()
}

// SetBookShares replaces the share list of a book.
func (s *Store) SetBookShares(ctx context.Context, bookID int64, shares []BookShare) error {
	return pgx.BeginFunc(ctx, s.DB, func(tx pgx.Tx) error {
		if _, err := tx.Exec(ctx, `DELETE FROM address_book_shares WHERE book_id=$1`, bookID); err != nil {
			return err
		}
		for _, sh := range shares {
			if _, err := tx.Exec(ctx, `INSERT INTO address_book_shares(book_id, user_id, group_id, permission) VALUES ($1,$2,$3,$4)`,
				bookID, sh.UserID, sh.GroupID, sh.Permission); err != nil {
				return err
			}
		}
		return nil
	})
}

func (s *Store) ListBookEntries(ctx context.Context, bookID int64) ([]*BookEntry, error) {
	rows, err := s.DB.Query(ctx, `SELECT device_id, alias, tags, note, created_at FROM address_book_entries
		WHERE book_id=$1 ORDER BY alias, device_id`, bookID)
	if err != nil {
		return nil, err
	}
	out := []*BookEntry{}
	ids := []string{}
	for rows.Next() {
		var e BookEntry
		if err := rows.Scan(&e.DeviceID, &e.Alias, &e.Tags, &e.Note, &e.CreatedAt); err != nil {
			rows.Close()
			return nil, err
		}
		out = append(out, &e)
		ids = append(ids, e.DeviceID)
	}
	rows.Close()
	if err := rows.Err(); err != nil {
		return nil, err
	}
	devs, err := s.DevicesByIDs(ctx, ids)
	if err != nil {
		return nil, err
	}
	byID := make(map[string]*Device, len(devs))
	for _, d := range devs {
		byID[d.ID] = d
	}
	for _, e := range out {
		e.Device = byID[e.DeviceID]
	}
	return out, nil
}

func (s *Store) UpsertBookEntry(ctx context.Context, bookID int64, deviceID, alias string, tags []string, note string) error {
	_, err := s.DB.Exec(ctx, `INSERT INTO address_book_entries(book_id, device_id, alias, tags, note) VALUES ($1,$2,$3,$4,$5)
		ON CONFLICT (book_id, device_id) DO UPDATE SET alias=EXCLUDED.alias, tags=EXCLUDED.tags, note=EXCLUDED.note`,
		bookID, deviceID, alias, nonNil(tags), note)
	return err
}

func (s *Store) DeleteBookEntry(ctx context.Context, bookID int64, deviceID string) error {
	tag, err := s.DB.Exec(ctx, `DELETE FROM address_book_entries WHERE book_id=$1 AND device_id=$2`, bookID, deviceID)
	if err == nil && tag.RowsAffected() == 0 {
		return ErrNotFound
	}
	return err
}
