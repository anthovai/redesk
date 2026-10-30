# Testing

## Unit tests

```sh
make test     # go vet + go test, cargo test, svelte-check and bun test (web/src/**/*.test.ts)
```

Windows code is at least compiled from a Mac: `cd agent && cargo check --target x86_64-pc-windows-gnu --all-targets`.

Database-backed server tests run when `AEGIRON_TEST_DATABASE_URL` points at a scratch database (e.g. `postgres:///aegiron_test?host=/tmp`); without it they are skipped. Mail tests use an in-process SMTP server with STARTTLS (`server/internal/mail/mailtest`).

### LDAP / Active Directory

`deploy/test/ldap-test.sh` starts a directory (glauth, container `aegiron-admintest-ldap`, `127.0.0.1:13893` StartTLS and `13894` LDAPS) with a throwaway CA and the users in `deploy/test/ldap/glauth.cfg`, runs the LDAP tests and removes it. They cover both transports, wrong passwords, unknown and wildcard user names, groups through `memberOf` and a group search, an unknown CA, a server that refuses StartTLS (no plaintext fallback) and, with `AEGIRON_TEST_DATABASE_URL` set, web and desktop-app sign-in, role mapping, allowed groups, demotion and local-account protection. Not tested against a real Active Directory.

## End to end on one machine (`make e2e`)

`scripts/e2e.sh` builds everything and runs the real programs against each other on this machine, isolated from anything already installed:

- a server on `127.0.0.1:21200` with its own database (`aegiron_e2e`, dropped and recreated), data folder and a random admin password
- an agent with a scratch config (`--config` in a temporary folder): audio allowed, remote input, clipboard, LAN discovery and direct connections off, so tests never move the mouse or touch the network beyond the server
- Google Chrome, headless, driven by `puppeteer-core` (a web dev dependency; `CHROME_PATH` if Chrome is not in `/Applications`)

Everything is stopped, the database dropped and the folder removed at the end. Requirements: PostgreSQL 16 running locally (`make dev-db`; `PG_BIN` for the folder with `psql`/`createdb`/`dropdb` if they are not on `PATH`), Go, Rust, Bun, Chrome, and **Screen Recording permission** for the terminal (the agent captures the real screen). Options: `E2E_NO_BUILD=1`, `E2E_KEEP=1` (leave it running), `E2E_PORT`, `E2E_DB`, `E2E_REAL_AUDIO=1`, `E2E_STEPS` (e.g. `remote auth`: only those steps of `protocol native media auth remote`). The agent allows tunnels (to itself only) and terminals.

The server limits sign-ins to 20 per address per 10 minutes, and one run uses most of that; `make e2e` starts a fresh server each time, so this only matters when running the scripts below by hand against one server.

| Step | Script | What it checks |
|---|---|---|
| Protocol | `web/scripts/e2e-viewer.ts <id> <password> [dir]` | Relay, handshake with the browser's crypto, wrong then right password; a viewer offering `av1,h264,jpeg` gets `h264` from an agent without an AV1 encoder and `info.codecs` without `av1`; `cfg {codec: av1}` is ignored, `cfg {codec: jpeg}` and back switch with `encoder` messages; Opus `0x05` every 20 ms without sequence gaps; `audio {on}` stops and resumes it; optional file round trip |
| Native viewer | `agent/examples/media_probe.rs` | The desktop app's viewer code (`AEGIRON_CODEC=av1`) gets H.264, decodes frames and audio (`AEGIRON_AUDIO_OUT=0` decodes without playing) |
| Browser media | `web/scripts/e2e-media.ts <id> <password>` | Live viewer with AV1 remembered: decoder configured for H.264, quality monitor shows H264 and Opus, the menu shows AV1 disabled ("ปลายทางไม่รองรับ") with H.264 marked, `AudioDecoder` output, mute/unmute; recording policy "all": the session is uploaded with `codec h264`, `audio opus` and plays with sound; synthetic AV1 + Opus recordings (below): what is drawn and heard at each moment, audio within 60 ms of video, seeking forward, back, across a muted stretch and while paused, AV1 → H.264 switch, 2× with sound, 4× silent |
| Tunnels and terminals | `web/scripts/e2e-remote.ts <id>` | `aegiron-agent tunnel` to a local HTTP server (one page, 8 parallel 3 MiB downloads) and to a server that answers only after the client half-closed; a private-network target refused before the password is used; `aegiron-agent terminal --command` with Thai output and the remote exit status; the terminal protocol with the browser's crypto (start size, resize, Ctrl-C interrupts `sleep`, exit status); the console's xterm.js page in Chrome; recording policy "all": tunnels refused (422), a terminal session recorded and played back; audit entries with type and target |
| Sign-in | `web/scripts/e2e-auth.ts` | 2FA user in Chrome: TOTP with "จดจำอุปกรณ์นี้ 30 วัน", cookie attributes and lifetime, account page, password-only sign-in, wrong password still fails, admin revokes from the Users page, TOTP required again and the cookie cleared; `aegiron-agent login --remember-device`, second login without a code, admin revoke, code required and the token forgotten; every audit entry |

Each script prints `PASS`/`FAIL` lines and exits non-zero on a failure. They take `AEGIRON_URL` and `AEGIRON_PASSWORD` (admin) and can run against any test server; see the header of each file for its other variables.

### Test audio

Live audio is captured with ScreenCaptureKit on macOS and needs the Screen Recording permission. For repeatable tests the agent can send a generated tone instead: `AEGIRON_TEST_AUDIO=tone` (440 Hz, 0.5 s on / 0.5 s off). It is read from the environment only; the app, the installers and the login service never set it. `make e2e` uses it unless `E2E_REAL_AUDIO=1`. Real capture can be checked by hand: run the agent without it, play a sound and run `media_probe`, which prints the decoded level every second.

### Test display resolution

`AEGIRON_TEST_RESOLUTION=fake` on the agent replaces the CoreGraphics / `ChangeDisplaySettingsEx` backend with an in-memory list of modes (1920×1080, 1600×900, 1280×720, 1024×768), so `res_list` / `res_set` / `res_restore` can be exercised without touching the real screen. Like the test audio it is read from the environment only. The unit tests (`cargo test resolution`) use the same fake and check that the original mode comes back when a session ends.

### Synthetic recordings

A Mac has no AV1 encoder for live sessions, so `agent/examples/make_recording.rs` writes recordings with rav1e (a dev-dependency) and OpenH264: 320×240 at 10 fps, each frame showing its number in base 4 as the gray levels of its quadrants; Opus with a beep in the first half of every second, a muted second (8–9 s) without a sequence gap, and record times that jitter like network sends. `--codec av1|h264|switch`. With `--upload <server> --config <agent.toml> --token <t>` it uploads the file signed with that agent's key, exactly like an agent. The upload needs a pending recording for that token, which the server only creates for a real session and hands to the agent; `e2e-media.ts` inserts the row directly into the test database (`AEGIRON_E2E_DB`).

### Desktop app and `aegiron://` links

Run the app with a separate config so it does not touch the installed one; with `--config` its log is written next to that config:

```sh
aegiron-agent --config /tmp/v/agent.toml login --server http://127.0.0.1:21200 --email admin@aegiron.local
AEGIRON_UI_CONNECT=<id> AEGIRON_UI_PASSWORD=<password> AEGIRON_AUDIO_OUT=0 AEGIRON_UI_VIEWER_STATE=quality,menu=display \
  agent/target/release/aegiron --config /tmp/v/agent.toml
agent/target/release/aegiron --config /tmp/v/agent.toml aegiron://connect/<id>   # second launch: hands the link over and exits
```

The running app logs `request from another launch: Open("<id>")` and asks before connecting; an invalid link (`aegiron://connect/<id>?password=x`) is logged as ignored and only brings the window forward. Links opened by macOS itself (`open aegiron://…`, a click in the browser) arrive as Apple Events and need the installed `.app` registered with LaunchServices, so they are tested on an installed build, not here.

### Upgrading an existing database

To check that new migrations apply to a database in use: create a database, apply the migrations up to the previous release with `psql -f` (recording each name in `schema_migrations` as `migrations/<file>`), add some rows, then start the new server against it and check `schema_migrations` and the new columns.

## Not covered on a Mac

- AV1 from a live session (needs a Windows machine with a hardware AV1 encoder; rav1d decoding in the app is covered by `cargo test`)
- Windows audio capture (WASAPI loopback), Media Foundation encoders
