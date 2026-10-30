// Package ldapauth signs users in against an LDAP directory or Active Directory.
//
// Flow: connect over TLS (ldaps://, or ldap:// followed by StartTLS — there is no plaintext
// mode and no fallback), bind as the service account, find exactly one user entry with the
// configured filter, then bind as that entry with the user's password. Group membership comes
// from the entry's memberOf attribute and, optionally, a group search.
package ldapauth

import (
	"crypto/tls"
	"crypto/x509"
	"errors"
	"fmt"
	"net"
	"net/url"
	"strings"
	"time"

	"github.com/go-ldap/ldap/v3"
)

// DefaultUserFilter matches OpenLDAP (uid, mail) and Active Directory (sAMAccountName, UPN).
const DefaultUserFilter = "(|(uid={username})(mail={username})(sAMAccountName={username})(userPrincipalName={username}))"

const timeout = 15 * time.Second

var (
	// ErrInvalidCredentials: unknown user, ambiguous user or wrong password.
	ErrInvalidCredentials = errors.New("invalid credentials")
	// ErrUnavailable wraps connection, TLS and service-account failures.
	ErrUnavailable = errors.New("directory unavailable")
)

type Config struct {
	URL          string // ldaps://host[:636] or ldap://host[:389] (StartTLS)
	CACert       string // extra trusted CA (PEM)
	BindDN       string
	BindPassword string
	BaseDN       string
	UserFilter   string // {username} is replaced, escaped
	EmailAttr    string
	NameAttr     string
	GroupFilter  string // optional; {dn} is replaced, escaped
}

// User is a directory entry that authenticated.
type User struct {
	DN     string   `json:"dn"`
	Email  string   `json:"email"`
	Name   string   `json:"name"`
	Groups []string `json:"groups"` // group DNs
}

// Validate checks the static parts of a configuration.
func (c *Config) Validate() error {
	u, err := url.Parse(c.URL)
	if err != nil || u.Host == "" || u.User != nil || (u.Path != "" && u.Path != "/") || u.RawQuery != "" {
		return errors.New("invalid LDAP URL (ldaps://host or ldap://host)")
	}
	if u.Scheme != "ldaps" && u.Scheme != "ldap" {
		return errors.New("LDAP URL must start with ldaps:// or ldap:// (StartTLS)")
	}
	if c.BaseDN == "" {
		return errors.New("base DN is required")
	}
	if _, err := ldap.CompileFilter(c.userFilter("x")); err != nil {
		return fmt.Errorf("invalid user filter: %v", err)
	}
	if !strings.Contains(c.filterTemplate(), "{username}") {
		return errors.New("user filter must contain {username}")
	}
	if c.GroupFilter != "" {
		if !strings.Contains(c.GroupFilter, "{dn}") {
			return errors.New("group filter must contain {dn}")
		}
		if _, err := ldap.CompileFilter(strings.ReplaceAll(c.GroupFilter, "{dn}", "x")); err != nil {
			return fmt.Errorf("invalid group filter: %v", err)
		}
	}
	if c.CACert != "" {
		if _, err := certPool(c.CACert); err != nil {
			return err
		}
	}
	return nil
}

func (c *Config) filterTemplate() string {
	if c.UserFilter == "" {
		return DefaultUserFilter
	}
	return c.UserFilter
}

func (c *Config) userFilter(username string) string {
	return strings.ReplaceAll(c.filterTemplate(), "{username}", ldap.EscapeFilter(username))
}

func certPool(pemCA string) (*x509.CertPool, error) {
	pool, err := x509.SystemCertPool()
	if err != nil || pool == nil {
		pool = x509.NewCertPool()
	}
	if pemCA != "" && !pool.AppendCertsFromPEM([]byte(pemCA)) {
		return nil, errors.New("CA certificate is not valid PEM")
	}
	return pool, nil
}

// dial connects over TLS and binds the service account.
func (c *Config) dial() (*ldap.Conn, error) {
	if err := c.Validate(); err != nil {
		return nil, err
	}
	u, _ := url.Parse(c.URL)
	pool, err := certPool(c.CACert)
	if err != nil {
		return nil, err
	}
	tlsCfg := &tls.Config{ServerName: u.Hostname(), RootCAs: pool, MinVersion: tls.VersionTLS12}
	dialer := &net.Dialer{Timeout: timeout}
	var conn *ldap.Conn
	if u.Scheme == "ldaps" {
		conn, err = ldap.DialURL(c.URL, ldap.DialWithDialer(dialer), ldap.DialWithTLSConfig(tlsCfg))
	} else {
		conn, err = ldap.DialURL(c.URL, ldap.DialWithDialer(dialer))
		if err == nil {
			if err = conn.StartTLS(tlsCfg); err != nil {
				conn.Close()
				err = fmt.Errorf("StartTLS: %w", err)
			}
		}
	}
	if err != nil {
		return nil, fmt.Errorf("%w: %v", ErrUnavailable, err)
	}
	conn.SetTimeout(timeout)
	if err := c.bindService(conn); err != nil {
		conn.Close()
		return nil, err
	}
	return conn, nil
}

func (c *Config) bindService(conn *ldap.Conn) error {
	var err error
	if c.BindDN == "" {
		err = conn.UnauthenticatedBind("")
	} else {
		err = conn.Bind(c.BindDN, c.BindPassword)
	}
	if err != nil {
		return fmt.Errorf("%w: service account bind: %v", ErrUnavailable, err)
	}
	return nil
}

// find returns the single entry matching username (nil when there is none or several).
func (c *Config) find(conn *ldap.Conn, username string) (*ldap.Entry, error) {
	attrs := []string{"memberOf"}
	for _, a := range []string{c.EmailAttr, c.NameAttr} {
		if a != "" {
			attrs = append(attrs, a)
		}
	}
	res, err := conn.Search(ldap.NewSearchRequest(c.BaseDN, ldap.ScopeWholeSubtree, ldap.NeverDerefAliases, 2, int(timeout/time.Second), false,
		c.userFilter(username), attrs, nil))
	if err != nil && !ldap.IsErrorWithCode(err, ldap.LDAPResultSizeLimitExceeded) {
		return nil, fmt.Errorf("%w: search: %v", ErrUnavailable, err)
	}
	if res == nil || len(res.Entries) != 1 {
		return nil, nil
	}
	return res.Entries[0], nil
}

func (c *Config) user(conn *ldap.Conn, e *ldap.Entry) (*User, error) {
	u := &User{DN: e.DN, Groups: e.GetAttributeValues("memberOf")}
	if c.EmailAttr != "" {
		u.Email = strings.TrimSpace(e.GetAttributeValue(c.EmailAttr))
	}
	if c.NameAttr != "" {
		u.Name = strings.TrimSpace(e.GetAttributeValue(c.NameAttr))
	}
	if c.GroupFilter != "" {
		res, err := conn.Search(ldap.NewSearchRequest(c.BaseDN, ldap.ScopeWholeSubtree, ldap.NeverDerefAliases, 500, int(timeout/time.Second), false,
			strings.ReplaceAll(c.GroupFilter, "{dn}", ldap.EscapeFilter(e.DN)), []string{"dn"}, nil))
		if err != nil && !ldap.IsErrorWithCode(err, ldap.LDAPResultSizeLimitExceeded) {
			return nil, fmt.Errorf("%w: group search: %v", ErrUnavailable, err)
		}
		if res != nil {
			for _, g := range res.Entries {
				u.Groups = append(u.Groups, g.DN)
			}
		}
	}
	return u, nil
}

// Authenticate checks username and password against the directory.
func Authenticate(c Config, username, password string) (*User, error) {
	username = strings.TrimSpace(username)
	// Never bind with an empty password: many servers treat it as an anonymous bind that
	// "succeeds" (RFC 4513 unauthenticated bind).
	if username == "" || password == "" || len(username) > 256 || len(password) > 1024 {
		return nil, ErrInvalidCredentials
	}
	conn, err := c.dial()
	if err != nil {
		return nil, err
	}
	defer conn.Close()
	e, err := c.find(conn, username)
	if err != nil {
		return nil, err
	}
	if e == nil {
		return nil, ErrInvalidCredentials
	}
	if err := conn.Bind(e.DN, password); err != nil {
		if ldap.IsErrorWithCode(err, ldap.LDAPResultInvalidCredentials) {
			return nil, ErrInvalidCredentials
		}
		return nil, fmt.Errorf("%w: user bind: %v", ErrUnavailable, err)
	}
	// Group searches run as the service account again.
	if c.GroupFilter != "" {
		if err := c.bindService(conn); err != nil {
			return nil, err
		}
	}
	return c.user(conn, e)
}

// CheckPassword binds as a known entry (re-authentication of a signed-in user).
func CheckPassword(c Config, dn, password string) error {
	if dn == "" || password == "" || len(password) > 1024 {
		return ErrInvalidCredentials
	}
	conn, err := c.dial()
	if err != nil {
		return err
	}
	defer conn.Close()
	if err := conn.Bind(dn, password); err != nil {
		if ldap.IsErrorWithCode(err, ldap.LDAPResultInvalidCredentials) {
			return ErrInvalidCredentials
		}
		return fmt.Errorf("%w: user bind: %v", ErrUnavailable, err)
	}
	return nil
}

// Lookup finds a user without a password (connection test in the console).
func Lookup(c Config, username string) (*User, error) {
	conn, err := c.dial()
	if err != nil {
		return nil, err
	}
	defer conn.Close()
	if username == "" {
		return nil, nil
	}
	e, err := c.find(conn, username)
	if err != nil || e == nil {
		return nil, err
	}
	return c.user(conn, e)
}

// InGroups reports whether any of the user's group DNs matches one of the configured names.
// A configured name matches a group by full DN or by its first RDN value (e.g. "aegiron-admins"
// matches "cn=aegiron-admins,ou=groups,dc=example,dc=com"), case-insensitively.
func InGroups(groups, wanted []string) bool {
	for _, g := range groups {
		cn := g
		if dn, err := ldap.ParseDN(g); err == nil && len(dn.RDNs) > 0 && len(dn.RDNs[0].Attributes) > 0 {
			cn = dn.RDNs[0].Attributes[0].Value
		}
		for _, w := range wanted {
			if strings.EqualFold(w, g) || strings.EqualFold(w, cn) {
				return true
			}
		}
	}
	return false
}
