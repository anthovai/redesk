package api

import (
	"fmt"
	"testing"

	"aegiron/server/internal/auth"
)

func TestSharedAddressBooks(t *testing.T) {
	e := newTestEnv(t)
	adm := e.admin()
	owner := e.createUser(uniqueEmail("owner", "book.test"), "user", false)
	reader := e.createUser(uniqueEmail("reader", "book.test"), "user", false)
	member := e.createUser(uniqueEmail("member", "book.test"), "user", false)
	outsider := e.createUser(uniqueEmail("outsider", "book.test"), "user", false)
	ot, rt, mt, xt := e.bearer(owner), e.bearer(reader), e.bearer(member), e.bearer(outsider)

	st, r := e.post(nil, "POST", "/api/address-books", ot, map[string]any{"name": "Branch PCs", "note": "Chiang Mai"})
	if st != 201 {
		t.Fatalf("create: %d %v", st, r)
	}
	book := fmt.Sprintf("/api/address-books/%d", int64(r["id"].(float64)))
	dev := randomDeviceID()
	if st, _ := e.post(nil, "PUT", book+"/entries/"+dev, ot, map[string]any{"alias": "Front desk", "tags": []string{"cm"}, "note": "call first"}); st != 200 {
		t.Fatalf("owner add: %d", st)
	}
	if st, _ := e.post(nil, "PUT", book+"/entries/not-valid!", ot, map[string]any{"alias": "x"}); st != 400 {
		t.Fatalf("invalid id: %d", st)
	}

	// Not shared: invisible.
	if st, _ := e.post(nil, "GET", book+"/entries", rt, nil); st != 404 {
		t.Fatalf("unshared read: %d", st)
	}
	_, r = e.post(nil, "GET", "/api/address-books", xt, nil)
	if len(r["books"].([]any)) != 0 {
		t.Fatalf("outsider sees books: %v", r)
	}

	// Share: reader read-only, a group (with member in it) read-write.
	st, r = e.post(nil, "POST", "/api/groups", adm, map[string]any{"name": "Branch team " + auth.RandomToken(4), "member_ids": []int64{member.ID}})
	if st != 200 {
		t.Fatalf("group: %d", st)
	}
	gid := int64(r["id"].(float64))
	if st, _ := e.post(nil, "PUT", book+"/shares", rt, map[string]any{"shares": []any{}}); st != 404 {
		t.Fatalf("non-member shares: %d", st)
	}
	if st, r := e.post(nil, "PUT", book+"/shares", ot, map[string]any{"shares": []map[string]any{
		{"user_id": reader.ID, "permission": "read"}, {"group_id": gid, "permission": "rw"},
	}}); st != 200 {
		t.Fatalf("share: %d %v", st, r)
	}
	if st, _ := e.post(nil, "PUT", book+"/shares", ot, map[string]any{"shares": []map[string]any{{"user_id": reader.ID, "permission": "admin"}}}); st != 400 {
		t.Fatalf("bad permission: %d", st)
	}

	st, r = e.post(nil, "GET", book+"/entries", rt, nil)
	if st != 200 || len(r["entries"].([]any)) != 1 || r["book"].(map[string]any)["permission"] != "read" {
		t.Fatalf("reader list: %d %v", st, r)
	}
	if ent := r["entries"].([]any)[0].(map[string]any); ent["alias"] != "Front desk" || ent["note"] != "call first" {
		t.Fatalf("entry: %v", ent)
	}
	if st, _ := e.post(nil, "PUT", book+"/entries/"+dev, rt, map[string]any{"alias": "hijack"}); st != 403 {
		t.Fatalf("reader write: %d", st)
	}
	if st, _ := e.post(nil, "PUT", book+"/entries/"+randomDeviceID(), mt, map[string]any{"alias": "Back office"}); st != 200 {
		t.Fatalf("group rw write: %d", st)
	}
	if st, _ := e.post(nil, "GET", book+"/shares", mt, nil); st != 403 {
		t.Fatalf("rw sees shares: %d", st)
	}
	if st, _ := e.post(nil, "DELETE", book, mt, nil); st != 403 {
		t.Fatalf("rw deletes book: %d", st)
	}
	// The highest permission wins: member also gets full directly.
	e.post(nil, "PUT", book+"/shares", ot, map[string]any{"shares": []map[string]any{
		{"user_id": reader.ID, "permission": "read"}, {"group_id": gid, "permission": "rw"}, {"user_id": member.ID, "permission": "full"},
	}})
	if st, r := e.post(nil, "GET", book+"/shares", mt, nil); st != 200 || len(r["shares"].([]any)) != 3 {
		t.Fatalf("full sees shares: %d %v", st, r)
	}
	// Admins see every book.
	_, r = e.post(nil, "GET", "/api/address-books", adm, nil)
	found := false
	for _, b := range r["books"].([]any) {
		found = found || b.(map[string]any)["name"] == "Branch PCs"
	}
	if !found {
		t.Fatal("admin does not see the book")
	}
	if st, _ := e.post(nil, "DELETE", book+"/entries/"+dev, mt, nil); st != 200 {
		t.Fatalf("remove entry: %d", st)
	}
	if st, _ := e.post(nil, "DELETE", book, mt, nil); st != 200 {
		t.Fatalf("full deletes book: %d", st)
	}
	if e.auditCount("address_book.shared", book[len("/api/address-books/"):]) != 2 {
		t.Fatal("shares not audited")
	}
}
