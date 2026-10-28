// Package vault seals stored credentials (endpoint passwords, API tokens) with AES-256-GCM.
//
// The key comes from AEGIRON_VAULT_KEY (base64, 32 bytes) or <data dir>/vault.key, created
// on first start. Database dumps alone therefore do not reveal secrets; keep the key file
// out of database backups and back it up separately.
package vault

import (
	"crypto/aes"
	"crypto/cipher"
	"crypto/rand"
	"encoding/base64"
	"errors"
	"fmt"
	"os"
	"path/filepath"
	"strings"
)

var aad = []byte("redesk-vault-v1")

type Vault struct{ aead cipher.AEAD }

func Load(envKey, dataDir string) (*Vault, error) {
	key, err := loadKey(envKey, dataDir)
	if err != nil {
		return nil, err
	}
	block, err := aes.NewCipher(key)
	if err != nil {
		return nil, err
	}
	aead, err := cipher.NewGCM(block)
	if err != nil {
		return nil, err
	}
	return &Vault{aead: aead}, nil
}

func loadKey(envKey, dataDir string) ([]byte, error) {
	if envKey != "" {
		k, err := base64.StdEncoding.DecodeString(envKey)
		if err != nil || len(k) != 32 {
			return nil, errors.New("AEGIRON_VAULT_KEY must be 32 bytes, base64")
		}
		return k, nil
	}
	path := filepath.Join(dataDir, "vault.key")
	if b, err := os.ReadFile(path); err == nil {
		k, err := base64.StdEncoding.DecodeString(strings.TrimSpace(string(b)))
		if err != nil || len(k) != 32 {
			return nil, fmt.Errorf("invalid vault key file %s", path)
		}
		return k, nil
	} else if !errors.Is(err, os.ErrNotExist) {
		return nil, err
	}
	if err := os.MkdirAll(dataDir, 0o700); err != nil {
		return nil, err
	}
	k := make([]byte, 32)
	if _, err := rand.Read(k); err != nil {
		return nil, err
	}
	if err := os.WriteFile(path, []byte(base64.StdEncoding.EncodeToString(k)+"\n"), 0o600); err != nil {
		return nil, err
	}
	return k, nil
}

// Seal returns nonce || ciphertext. An empty secret seals to nil (nothing stored).
func (v *Vault) Seal(plain string) ([]byte, error) {
	if plain == "" {
		return nil, nil
	}
	nonce := make([]byte, v.aead.NonceSize())
	if _, err := rand.Read(nonce); err != nil {
		return nil, err
	}
	return v.aead.Seal(nonce, nonce, []byte(plain), aad), nil
}

func (v *Vault) Open(sealed []byte) (string, error) {
	if len(sealed) == 0 {
		return "", nil
	}
	n := v.aead.NonceSize()
	if len(sealed) < n {
		return "", errors.New("vault: ciphertext too short")
	}
	p, err := v.aead.Open(nil, sealed[:n], sealed[n:], aad)
	if err != nil {
		return "", errors.New("vault: cannot decrypt (wrong vault key?)")
	}
	return string(p), nil
}
