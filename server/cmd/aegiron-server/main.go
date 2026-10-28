// Command aegiron-server runs the Aegiron rendezvous, relay and API server on a single port.
package main

import (
	"context"
	"errors"
	"log/slog"
	"net/http"
	"os"
	"os/signal"
	"syscall"
	"time"

	"aegiron/server/internal/api"
	"aegiron/server/internal/auth"
	"aegiron/server/internal/config"
	"aegiron/server/internal/hub"
	"aegiron/server/internal/store"
	"aegiron/server/internal/vault"
)

func main() {
	log := slog.New(slog.NewTextHandler(os.Stderr, nil))
	if err := run(log); err != nil {
		log.Error("fatal", "err", err)
		os.Exit(1)
	}
}

func run(log *slog.Logger) error {
	cfg := config.Load()
	ctx, stop := signal.NotifyContext(context.Background(), os.Interrupt, syscall.SIGTERM)
	defer stop()

	st, err := store.Open(ctx, cfg.DatabaseURL)
	if err != nil {
		return err
	}
	defer st.Close()
	if err := st.Migrate(ctx); err != nil {
		return err
	}
	if err := st.CloseDanglingSessions(ctx); err != nil {
		return err
	}
	if err := bootstrapAdmin(ctx, cfg, st, log); err != nil {
		return err
	}

	key, err := auth.LoadOrCreateServerKey(cfg.DataDir)
	if err != nil {
		return err
	}
	log.Info("server identity", "public_key", auth.PublicKeyB64(key))

	v, err := vault.Load(cfg.VaultKey, cfg.DataDir)
	if err != nil {
		return err
	}
	if cfg.GuacdAddr != "" {
		log.Info("RDP/VNC/SSH gateway enabled", "guacd", cfg.GuacdAddr, "recording", cfg.GatewayRecordingPath != "")
	}

	h := hub.New(st, key, log, cfg.AllowedOrigins)
	a := api.New(cfg, st, h, key, v, log)

	srv := &http.Server{
		Addr:              cfg.Listen,
		Handler:           a.Handler(),
		ReadHeaderTimeout: 10 * time.Second,
	}

	go func() {
		t := time.NewTicker(time.Hour)
		defer t.Stop()
		for {
			select {
			case <-ctx.Done():
				return
			case <-t.C:
				_ = st.PurgeExpiredSessions(ctx)
			}
		}
	}()
	go func() {
		t := time.NewTicker(time.Duration(cfg.SyncInterval) * time.Minute)
		defer t.Stop()
		for {
			select {
			case <-ctx.Done():
				return
			case <-t.C:
				a.SyncAll(ctx)
			}
		}
	}()

	errc := make(chan error, 1)
	go func() {
		log.Info("listening", "addr", cfg.Listen, "tls", cfg.TLSCert != "", "web", cfg.WebDir)
		if cfg.TLSCert != "" {
			errc <- srv.ListenAndServeTLS(cfg.TLSCert, cfg.TLSKey)
		} else {
			errc <- srv.ListenAndServe()
		}
	}()

	select {
	case err := <-errc:
		if !errors.Is(err, http.ErrServerClosed) {
			return err
		}
	case <-ctx.Done():
		log.Info("shutting down")
		sctx, cancel := context.WithTimeout(context.Background(), 10*time.Second)
		defer cancel()
		_ = srv.Shutdown(sctx)
	}
	return nil
}

// bootstrapAdmin creates the first admin account when the database has no users.
func bootstrapAdmin(ctx context.Context, cfg config.Config, st *store.Store, log *slog.Logger) error {
	n, err := st.CountUsers(ctx)
	if err != nil || n > 0 {
		return err
	}
	pw := cfg.AdminPassword
	generated := pw == ""
	if generated {
		pw = auth.RandomToken(12)
	}
	hash, err := auth.HashPassword(pw)
	if err != nil {
		return err
	}
	if _, err := st.CreateUser(ctx, cfg.AdminEmail, "Administrator", "admin", hash); err != nil {
		return err
	}
	if generated {
		log.Warn("created initial admin account; change this password after first login", "email", cfg.AdminEmail, "password", pw)
	} else {
		log.Info("created initial admin account", "email", cfg.AdminEmail)
	}
	return nil
}
