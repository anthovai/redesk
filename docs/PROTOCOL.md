# Aegiron protocol (v1)

> The product was called ReDesk before 0.5.0. Signature and key-derivation labels (`redesk-register-v1`, `redesk-server-v1`, `redesk-kx-v1`, `redesk v1 …`), the vault AAD and the LAN discovery magic keep that name on purpose: they are part of the wire format, so agents, servers and stored secrets from before the rename keep working. The server also accepts the old `X-ReDesk` header and `REDESK_*` environment variables.

Everything runs over one HTTPS port (default `21114`): REST API, agent signaling (`/ws/agent`) and the relay (`/ws/relay`).

## 1. Agent registration (`/ws/agent`, text JSON)

The agent pins the server's Ed25519 public key at `init` time.

```
server → agent  {"t":"challenge","nonce":S}
agent  → server {"t":"register","id","pk","nonce":A,"sig","hostname","os","version","enroll_token"?,"macs"?,"lan"?}
                sig = Ed25519(agent_key, "redesk-register-v1\n" + id + "\n" + S + "\n" + A)
server → agent  {"t":"registered","sig":Ed25519(server_key, "redesk-server-v1\n" + id + "\n" + A + "\n" + S)}
             or {"t":"error","code":"id_taken|enroll_required|disabled|bad_signature|...","message"}
```

- The first registration binds `id → pk` (TOFU per device). A different key for the same id is rejected (`id_taken`) and audited as `device.key_mismatch`. Admins can reset the binding.
- With `require_enroll` on, a new or reset device must present a valid enrollment token.
- If the server signature fails verification, the agent aborts: *invalid public key from server*.
- Keepalive: `{"t":"ping"}` / `{"t":"pong"}` every 20 s.
- `macs` (up to 16, `aa:bb:cc:dd:ee:ff`) and `lan` (`[{ip, prefix}]`, up to 16) describe the device's network so the server can wake it later. Admins see them; other users only see `can_wake`.
- Wake-on-LAN: `POST /api/devices/{id}/wake` picks an online agent on the same LAN (same public IP and/or subnet) and sends it `{"t":"wol","macs":[...],"target":"<id>"}`. The helper broadcasts the magic packet (UDP 9 and 7) and reports `wol_sent`. Errors: `online_already`, `no_mac`, `no_helper`, `helper_busy`, `rate_limited`.
- LAN discovery (not through the server): a viewer broadcasts `redesk-discover v1` to UDP 21120; agents on the same subnet with `lan_discovery = true` answer `{id, hostname, os}`, rate-limited.
- Session request: `{"t":"session","sid","token","relay":"/ws/relay","viewer":{"email","name","ip"}}`.
- Agent → server audit events: `{"t":"event","sid","kind","detail"}` with `kind` in `auth_failed | auth_ok | denied | locked | file_upload | file_download | wol_sent`. They are stored as `agent.<kind>` and attributed to the viewer of that session.

## 2. Relay (`/ws/relay?sid=&token=`)

The viewer token goes only to the console user who requested the session, and the relay also requires that user's login cookie. The agent token is sent over signaling. Once both sides connect, the server sends `{"t":"paired"}` to each and then forwards frames **blind**. It counts bytes for the audit log but cannot read the content.

## 3. E2E handshake (text JSON through the relay)

```
viewer → agent  {"t":"hello","v":1,"epk":X25519 public (32B, base64)}
agent  → viewer {"t":"hello","v":1,"epk":...,"sig":Ed25519(device_key, transcript)}

transcript = "redesk-kx-v1" || sid || viewer_epk || agent_epk
```

The viewer verifies `sig` against the device public key fetched from the API. That key was bound at registration, so a relay or server that swaps keys cannot impersonate the device. The UI shows the key's fingerprint, which can be compared with `aegiron-agent info`.

```
shared  = X25519(viewer_esk, agent_epk)
salt    = SHA-256(transcript)
k_v2a   = HKDF-SHA256(shared, salt, "redesk v1 viewer->agent", 32)
k_a2v   = HKDF-SHA256(shared, salt, "redesk v1 agent->viewer", 32)
AEAD    = AES-256-GCM, nonce = 0x00000000 || u64_be(counter), one counter per direction
```

Every later message is binary ciphertext. A dropped, reordered, or replayed message fails decryption because the counter is implicit.

## 4. Messages inside the encrypted channel

Byte 0 is the type: `0x01` JSON, `0x02` video rectangle (`x, y, frame_w, frame_h` as u16 BE, followed by a JPEG), `0x03` file chunk (transfer id as u32 BE, followed by up to 64 KiB of data), `0x04` video frame (codec u8 `1` = H.264, flags u8 with bit 0 = keyframe, `width, height` as u16 BE, followed by one Annex B access unit).

| Direction | Message |
|---|---|
| agent→viewer | `auth_required {password, click}`, `auth_pending`, `auth_result {ok, reason?, attempts_left?, retry_after?}`, `info {hostname, os, displays[], input, clipboard, files, codec, features?}`, `cursor {x,y,visible}`, `clip {s}` |
| viewer→agent | `auth {password, codecs[]}`, `mm {x,y}`, `mb {b,down,x,y}`, `wh {dx,dy}`, `key {code,key,down}`, `text {s}`, `clip {s}`, `cfg {fps,quality,max_width,display}`, `refresh` (alias `keyframe`), `bye` |
| files viewer→agent | `ls {path}` (`""` = home), `put {id,dir,name,size}` followed by `0x03` chunks and then `put_end {id}`, `get {id,path}`, `cancel {id}` |
| files agent→viewer | `ls {path,parent,sep,entries[{name,dir,size,mtime}]}`, `put_ok {id,path}`, `put_done {id,path}`, `get_start {id,name,size}` followed by `0x03` chunks and then `get_end {id}`, `fs_error {op,id?,message}` |

### Video codecs

The viewer lists the codecs it can decode in `auth.codecs`, best first (`["h264","jpeg"]`). The agent picks H.264 when offered, otherwise JPEG, and reports the choice in `info.codec`.

- **H.264** (OpenH264, Constrained Baseline, screen-content mode, rate control from resolution × fps × quality): whole frames as `0x04`, sent only when the screen changed. A keyframe goes out at start, after `cfg` changes, on `refresh`/`keyframe`, and every ~10 s of activity. Browsers decode with WebCodecs (`avc1.42E033`) and the native app with OpenH264. A viewer drops deltas until its first keyframe and sends `keyframe` after a decoder error. If the agent's encoder fails, it falls back to JPEG rectangles in the same session, so every viewer must accept both.
- **JPEG** (fallback): only the changed bounding rectangle as `0x02`.

Screen captures do not include the pointer, so the agent sends `cursor` (normalised to the streamed display, `visible=false` when it is on another display) whenever the pointer moves. Viewers draw it while their own pointer is not over the remote screen.

Uploads keep only the file name (any path the viewer sends is dropped), never overwrite an existing file (`name (1).ext`), must match the announced size, and are deleted if the session ends before `put_end`. Downloads are read on a separate task and share the session's back-pressured outbound queue with video.

### Folders and file management (`info.features` contains `"fs2"`)

Agents from 0.6 list optional capabilities in `info.features` (an array of strings; absent on older agents). Viewers only offer folder transfer, delete, rename, new folder and clipboard files when the matching feature is listed, so old agents and old viewers keep working with the single-file messages above. Everything here still requires the `files` permission; refused requests get `fs_error`.

| Direction | Message |
|---|---|
| viewer→agent | `put {id,dir,name,size,rel}`: like `put`, but `rel` is a path inside `dir` (`/`-separated, e.g. `"Photos/2024/a.jpg"`); missing directories are created |
| viewer→agent | `mkdir {id,dir,name}` (`dir` `""` = `~/Downloads/Aegiron`; `name` may be a relative path; the last component gets ` (1)` if it exists) → `fs_ok {op:"mkdir",id,path}` |
| viewer→agent | `get_dir {id,path}` → `dir_start {id,name,files,size,dirs[]}`, then for each file `dir_file {id,rel,size}` followed by its `0x03` chunks (same transfer id), `dir_skip {id,rel,message}` for unreadable files, and finally `get_end {id}` |
| viewer→agent | `rm {id,path}` (file or whole directory) → `fs_ok {op:"rm",id,path}` |
| viewer→agent | `mv {id,path,name}` (rename inside the same directory; fails if `name` exists) → `fs_ok {op:"mv",id,path}` (new path) |
| viewer→agent | `cancel {id}` now also stops a running `get` / `get_dir` |

Folder upload: the viewer sends `mkdir {dir, name:"<folder>"}` and uses the returned path (so an existing folder is never merged into: `Photos (1)`), then one `put {dir:<path>, rel}` per file (and `mkdir` for empty sub-folders), one after another. Folder download streams files one at a time; the viewer creates `dirs[]` (including empty ones) and writes each `dir_file` until the next `dir_file` or `get_end`. The browser saves a downloaded folder as `<folder>.zip`; the desktop app writes it under `~/Downloads/Aegiron/<folder>` (` (1)` if taken).

Relative paths are untrusted on both sides (agent for `put.rel`/`mkdir.name`, viewer for `dir_file.rel`/`dirs`): split on `/` and `\`; a path that is empty, absolute, has a drive/UNC prefix or contains `.`/`..` components is rejected; characters invalid on Windows (`<>:"|?*`, control characters) become `_`; a component is at most 255 bytes. While creating directories, an existing component that is a symlink or not a directory aborts that file, so a symlink inside the target cannot redirect writes elsewhere. `get_dir` never follows symlinks (they are skipped) and stops at 100,000 entries or depth 64. `rm` refuses the filesystem root and the home directory and removes a symlink itself, never its target. Every upload, download, delete and rename is reported as an agent event (`file_upload`, `file_download`, `file_delete`, `file_rename`).

### Clipboard files (`info.features` contains `"clipfiles"`; desktop app only)

Needs both the `clipboard` and `files` permissions. The viewer opts in with `auth {…, features:["clipfiles"]}`.

| Direction | Message |
|---|---|
| agent→viewer | `clip_files {files[{name,path,dir,size}]}`: files/folders were copied on the agent (macOS `NSPasteboard` file URLs, Windows `CF_HDROP`) |
| viewer→agent | `clip_files {id,paths[]}`: put these agent paths on the agent's clipboard as files → `fs_ok {op:"clip_files",id}` |

- **Viewer → agent (paste):** when ⌘V/Ctrl+V is pressed in the remote window and this machine's clipboard holds files, the app uploads them (folders included) to `~/Downloads/Aegiron` on the agent, puts the uploaded paths on the agent's clipboard with `clip_files`, then presses paste on the remote side, so pasting into Finder/Explorer copies them there. The files also stay in `Downloads/Aegiron` (the app says so), which is the fallback when the focused remote app does not accept files. Pressing paste again without copying anything new only presses paste remotely.
- **Agent → viewer (copy):** small selections (files only, at most 20 and 32 MiB in total) are fetched right away into a temporary folder and put on this machine's clipboard as files, so ⌘V/Ctrl+V in Finder/Explorer pastes them; larger selections and folders wait for the user to click *รับไฟล์* in the remote window, are saved under `~/Downloads/Aegiron` and then put on the clipboard. Files never flow to a viewer that did not opt in, and the agent does not echo back file lists it set itself.

Pointer coordinates are normalised to 0–1 over the displayed frame. Plain typing uses the actual character (Thai and other layouts work). Shortcuts with Ctrl/Cmd/Alt use the physical key.

Authentication: 3 attempts per session; 5 consecutive failures lock the agent for 60 s. The temporary password rotates after every successful login.

## 5. Desktop app sign-in

```
app  → POST /api/agent/login {email, password, totp?, device}      (HTTPS, certificate verified)
     ← {need_totp: true}  |  {user, token, server_key, enroll_token}
```

- `token`: bearer token for the app (`Authorization: Bearer …`), 30 days, revoked by `POST /api/auth/logout`. Stored in the agent's `agent.toml` (owner-only file).
- `server_key`: the server's Ed25519 identity, trusted because it arrived over the server's own TLS. A signed-in device that already pins a different key refuses the sign-in.
- `enroll_token`: single-use, 1 hour, owned by the user. Registering with it sets `devices.owner_id` and adds the device to that user's address book.
- Opening the web viewer from the app: `POST /api/auth/handoff` (bearer) returns a code valid for 60 s and usable once; the browser opens `GET /api/auth/handoff/{code}?next=/connect/<id>`, which sets the console cookie and redirects. `next` must be a same-site path.

### Trusted devices ("remember this device for 30 days")

On the 2FA step a client may ask to be remembered. The trust token replaces only the TOTP code; the password is always checked first, and the brute-force limits apply as before.

```
web  → POST /api/auth/login  {email, password, totp, remember_device: true}
     ← Set-Cookie: aegiron_trust=<token>; Path=/api/auth; HttpOnly; SameSite=Strict; Secure; Max-Age=30 days
web  → POST /api/auth/login  {email, password}            (cookie sent automatically)  → {user}
app  → POST /api/agent/login {email, password, totp, device, remember_device: true}
     ← {user, token, server_key, enroll_token, trust_token}
app  → POST /api/agent/login {email, password, device, trust_token}                      → {user, token, …}
     ← {need_totp: true, trust_rejected: true}   when the token is expired, revoked or another user's
```

- The token is 32 random bytes (base64url). The server stores only its SHA-256 in `trusted_devices`, bound to the user, with a label (browser/OS, or the app's host name), user agent, first and last IP, created / last-used time and a fixed 30-day expiry (not extended by use). At most 20 per user; the oldest go first.
- A token is issued only in the same request as a correct TOTP code, and only if the account has 2FA. It travels in a request body or an HttpOnly cookie, never in a URL. The app keeps it in `agent.toml` (owner-only) per server and account, and keeps it after signing out; it is dropped on `trust_rejected`.
- Revocation: `GET /api/auth/trusted-devices` (the current browser is marked `current`), `DELETE /api/auth/trusted-devices/{id}`, `DELETE /api/auth/trusted-devices` (all). Admins: `GET|DELETE /api/users/{id}/trusted-devices[/{tid}]`. Every remembered device of a user is revoked automatically when the password is changed or reset, 2FA is enabled, disabled or reset, or the account is disabled.
- Audit: `auth.trusted_device_added`, `auth.login` / `auth.agent_login` with `trusted_device_added` or `trusted_device` (2FA skipped) in the detail, `auth.trusted_device_rejected`, `user.trusted_device_revoked {id, label, by}`, `user.trusted_devices_revoked {count, reason}`.
- SSO sign-in is unaffected: it never asks for TOTP (the IdP handles MFA) and never issues trust tokens.

### Organisation SSO (OpenID Connect)

- Browser: `GET /api/oidc/login?next=` → identity provider (Authorization Code + PKCE, `state`, `nonce`, bound to the browser by a cookie) → `GET /api/oidc/callback`. The ID token is verified (signature, issuer, audience, expiry, nonce); `email_verified` and the allowed domains are enforced. TOTP is not asked again: the IdP is responsible for MFA.
- App: `POST /api/oidc/auth {device}` → `{code, url, expires_in}`. The app opens `url` in the browser. That page shows the requesting device and IP before continuing to the IdP. The app polls `GET /api/oidc/auth-query?code=` every 2 s and gets `{pending:true}`, or the same payload as `/api/agent/login`. The poll code never appears in the browser URL.
- Admin: `GET/PUT /api/oidc/settings`. The client secret is sealed with the vault and never returned.

## 6. Agent updates

- Admins upload builds: `POST /api/agent/releases` (multipart, up to 300 MB, stored by SHA-256), then enable or disable them.
- Clients check with `GET /api/agent/update?os=&arch=&version=`, which returns the newest enabled release that is newer by semver.
- Clients download from `GET /api/agent/releases/{id}/download`, served with `X-Checksum-SHA256`.
- The app downloads only after the user asks, from the same server only. It keeps the file only if the size and SHA-256 match, and never installs it by itself.

## 7. Session recording

Sessions are end-to-end encrypted, so the server cannot record them. The **controlled device** records what it streams and uploads the file afterwards.

- Policy (admin, web console → บันทึกเซสชัน): `GET/PUT /api/recordings/settings` `{mode: "off"|"all"|"groups", group_ids[], retention_days}` (0 = keep until deleted). Stored in `settings` under key `recording`.
- When a session must be recorded, the server creates a `pending` row in `session_recordings` and adds `"record": {"token", "upload"}` to the agent's `session` message. The token is single-use (only its SHA-256 is stored) and valid for 7 days. Agents that predate recording ignore the field and stream unrecorded.
- After authentication the agent starts the recorder and reports `{"t":"event","kind":"recording_started"}` (or `recording_failed`; if the recorder cannot start, the session is refused with `auth_result {ok:false, reason:"recording_failed"}` rather than run unrecorded). The approval prompt says the session will be recorded; `info` carries `"recording": true` so viewers show **กำลังบันทึก**, and the connection-manager window on the controlled machine shows the same badge for the whole session.
- Upload, after the session ends (and retried whenever the agent reconnects; files cut off by a crash are truncated to the last complete record and uploaded too):

  ```
  POST /api/agent/recordings/{sid}
  Authorization: Bearer <record.token>
  X-Aegiron: 1
  X-Aegiron-Signature: base64 Ed25519(device key, "aegiron-recording-v1\n" + device_id + "\n" + sid + "\n" + hex(SHA-256(body)))
  body: the .aerec file
  ```

  Only over HTTPS with a verified certificate, or plain HTTP to loopback; `allow_insecure_transport` does not apply. The server checks the token, the signature against the device's registered key, and the file structure (header `sid`/`device_id` must match), then stores it as `<AEGIRON_DATA_DIR>/recordings/<sid>.aerec` (limit `AEGIRON_RECORDING_MAX_MB`, default 4096). 400/401/404/409/413 are final (the agent deletes its copy); anything else is retried.
- Admin only: `GET /api/recordings[?device=]`, `GET /api/recordings/{id}`, `GET /api/recordings/{id}/file` (Range supported), `DELETE /api/recordings/{id}`. Audit: `recording.uploaded`, `recording.rejected`, `recording.viewed`, `recording.deleted`, `recording.purged`, `recording.settings_updated`, plus `agent.recording_started` / `agent.recording_failed`. Retention runs at start-up and hourly.

### Container (`.aerec`)

```
"AEGREC1\n"
u32 BE header length, JSON header {"v":1,"sid","device_id","hostname","os","started_ms"}
records until EOF: u32 BE milliseconds since start, u32 BE payload length (1..32 MiB), payload
```

A payload is a plaintext channel message exactly as streamed: `0x04` H.264 frames, `0x02` JPEG rectangles (a session can switch from H.264 to JPEG), and `0x01` `cursor` messages. Clipboard, file chunks and other control traffic are not recorded. The last record is `{"t":"end"}` so idle time at the end counts. The browser plays it with WebCodecs (H.264) and `createImageBitmap` (JPEG), seeking from the nearest H.264 keyframe or full-screen JPEG rectangle.

## 8. Whiteboard (annotations)

The viewer draws on the remote screen; the controlled machine shows the strokes in a transparent, click-through, always-on-top window over the streamed display. Only the desktop app (`aegiron`) can show it; the headless `aegiron-agent` never offers it.

- Negotiation: `info.whiteboard: true` when the agent allows it (`allow_whiteboard`, default on; app Settings → ความปลอดภัย, or `aegiron-agent set --allow-whiteboard false`) and a desktop UI is attached. Viewers show the pen only then; older agents never set it, older viewers never send strokes.
- viewer→agent (JSON inside the encrypted channel):
  - `wb {op:"line", id, c:"#rrggbb", w, p:[x0,y0,x1,y1,...]}` appends points to stroke `id` (created on first use with color `c` and width `w`, a fraction of the display height). Points are normalised 0–1 over the displayed frame, like pointer input.
  - `wb {op:"clear"}` removes all strokes.
- Limits on the agent: 1024 points per message, 300 strokes, 60 000 points (oldest strokes go first). Strokes stay until cleared; they disappear when the session ends or the viewer switches display. The overlay is part of the screen, so the stream (and a recording) shows it too.

## 9. Direct connections (LAN/TCP and P2P) and proxies

A native viewer can reach the agent without the relay. The bytes on a direct path are the same WebSocket messages as on the relay (`paired`, then the §3 handshake and the §4 encrypted channel, unchanged), so a direct path gains no trust: the agent is still authenticated by its device key, and the viewer by the password or click inside the channel. The browser viewer always uses the relay.

**Negotiation (feature flags, so old peers keep working)**

- The agent adds `"features":["direct1"]` (and its NAT type as `nat`) to `register`. Servers that do not know it ignore the field.
- `registered` may carry `udp_port` (rendezvous / NAT probe). Older agents ignore it.
- The viewer asks with `POST /api/sessions?direct=1`. Older servers ignore the query flag. Without it, nothing below happens.
- The server offers a direct path only if the viewer asked, `AEGIRON_P2P` is on (default) and the agent advertised `direct1`. Then the session request gets `"direct":{"token":T,"punch_key":Ka,"udp_port":P}`, where `T` is a fresh per-session secret and `Ka` is the agent's rendezvous key (`punch_key`/`udp_port` are present only when UDP is enabled).
- The agent answers on signaling with `{"t":"direct_offer","sid","tcp":["ip:port",...],"cert":<hex SHA-256 of its per-session QUIC certificate>,"nat"}`, or with `{"t":"direct_offer","sid","disabled":true}` when its policy (`direct_enabled = false`) forbids direct sessions. The server waits for the offer for at most 2.5 s. It keeps up to 8 valid unicast candidates, adds the agent's public IP as the server sees it (with the listening port), and returns `ticket.direct = {token: T, tcp: [...], quic: {cert, punch_key: Kv, udp_port: P}, nat}` to the viewer. `Kv` is the viewer's own rendezvous key.

**Token proof on every direct path.** The viewer opens a WebSocket (`GET ws://aegiron-direct/`) over the path with the header

```
aegiron-direct: v1 <sid> <viewer_epk_b64> <base64 HMAC-SHA256(T, "aegiron-direct-v1\n" + sid + "\n" + viewer_epk_b64)>
```

The agent checks it during the HTTP upgrade, before any WebSocket frame: the sid must be a session that is waiting for its viewer, the MAC must verify (constant time), and at most 6 connections are accepted per session. Anything else gets `403` and is closed. The agent then sends `{"t":"paired","path":"direct"|"p2p"}` and accepts only a `hello` whose `epk` equals the key in the header, so a sniffed header cannot be reused with another key. The token never crosses the network in clear: it reaches the viewer over the server's HTTPS and the agent over signaling. The direct TCP port is rate-limited to 30 unauthenticated attempts per minute per source address and 16 handshakes at a time. The port opens with the first direct offer and rejects everything unless a session with a direct offer is waiting for its viewer. Each per-session QUIC endpoint accepts for at most 20 s.

**Paths**

- **Direct (TCP)**: the agent listens on `direct_port` (default 21118, a random port if it is taken) and offers its LAN IPv4 addresses. With a port forward, the public address the server adds also works.
- **P2P (QUIC over UDP hole punching)**: each side sends `{"t":"punch","sid","role":"agent"|"viewer","ts","mac":base64 HMAC-SHA256(K, "aegiron-punch-v1\n"+sid+"\n"+role+"\n"+ts)}` from the socket it will use to the server's UDP port `P`, and repeats until the reply `{"t":"punch","addr":<own public address>,"peer":<other side's>}` names the peer. The first authenticated address per role wins, and the rendezvous closes once the session is direct. Both then send a few datagrams to the peer to open their NAT mappings. The agent runs a QUIC server (TLS 1.3, ALPN `aegiron-direct/1`) on that socket with a self-signed certificate made for this session. The viewer pins it by the SHA-256 from the ticket and accepts no other certificate. The WebSocket runs on one bidirectional QUIC stream. QUIC (quinn, ring) gives reliable, ordered, congestion-controlled delivery with keep-alives, which the counter-based AEAD channel requires.
- **NAT probe** (like RustDesk's NAT test port): the server answers `{"t":"nat"}` with `{"t":"nat","addr":"ip:port"}` on UDP `P` and `P+1`. The agent sends from one socket to both: different mapped ports means a *symmetric* NAT (hole punching usually fails), the same means *cone*, and a mapped address equal to a local one means *open*. The result is shown in Settings → Network and sent as `nat`. UDP requests shorter than 96 bytes are ignored and replies are never larger than the request (no amplification), with at most 20 packets per second per source.

**Choosing a path.** The viewer starts every candidate at once (TCP to each address, QUIC). The relay starts after a 2.5 s head start, or at once if every direct attempt has already failed, and direct attempts give up after 9 s. The viewer sends its `hello` on the first path that answers `paired`. The agent joins the relay as before and waits on all paths: the first `hello` wins and the others are dropped. If nothing is offered, or both policies allow it but no direct path works, the session runs on the relay exactly as before. The viewer shows the path (relay / direct / P2P) in its connection info.

**Audit.** When the agent accepts a hello on a direct path it sends `{"t":"direct_started","sid","path":"direct"|"p2p"}`. The server writes `session.start` with `path`, marks the session started, and closes the agent's unused relay slot (a viewer can no longer join it). When the session ends, the agent sends `{"t":"direct_ended","sid","reason","bytes_to_viewer","bytes_to_agent"}` and the server writes `session.end` with `path` and the byte counts. If the agent's signaling connection drops while a direct session runs, the server ends that record with `reason: "agent_offline"`. Relay sessions now also carry `"path":"relay"` in their audit detail. Agent events (`agent.*`) of direct sessions are attributed to the viewer as before.

**Policy.** Server: `AEGIRON_P2P=false` turns off direct and P2P sessions for everyone. `AEGIRON_UDP_PORT` (default `21116`, which also uses `21117`; `0` = off) controls hole punching and the NAT probe; LAN/TCP direct sessions still work without it. Open these UDP ports in the firewall: a TLS reverse proxy such as Caddy does not carry them. Device: `direct_enabled = false` in `agent.toml` (Settings → Network → อนุญาตการเชื่อมต่อโดยตรง, or `aegiron-agent set --direct false`) makes the agent decline every offer and the app's viewer use only the relay.

**Proxy.** `[proxy]` in `agent.toml` (`kind = "socks5" | "http"`, `host`, `port`, optional `username`/`password`, stored like the other secrets in the owner-only file; Settings → Network → พร็อกซี, or `aegiron-agent set --proxy socks5://host:port --proxy-user NAME`, which prompts for the password) sends every connection to the server through a SOCKS5 tunnel (RFC 1928 with username/password, RFC 1929; the proxy resolves the name) or an HTTP `CONNECT` tunnel (`Proxy-Authorization: Basic`). This covers signaling, relay and the HTTPS API (sign-in, address book, sessions, updates). TLS to the server runs inside the tunnel with the same certificate verification as without a proxy. Credentials are refused in proxy addresses and never appear in URLs. Direct and P2P paths do not use the proxy; turn them off where all traffic must go through it.

`aegiron-agent connect <ID> [--only relay|direct|p2p]` runs the app's viewer from a terminal and prints the path it used, which helps diagnose a deployment.

## 10. `aegiron://` links and the single app instance

- Links: `aegiron://connect/<id>` or `aegiron://<id>` (also `redesk://`), where `<id>` is 9 digits (spaces or `%20` allowed). Nothing else is accepted: no query string, no extra path, no password. The app always asks before connecting.
- macOS delivers links as a `kAEGetURL` Apple Event, at launch and while the app runs. The app handles it with `NSAppleEventManager` (installed before the event loop starts and again after launch). Windows passes the link as the first argument.
- One app instance per user and config file. The first instance holds an exclusive lock on `instance.lock` next to `agent.toml` and listens on a Unix socket `instance.sock` there (folder 0700, socket 0600) or, on Windows, on the named pipe `\\.\pipe\aegiron-<first 8 bytes of SHA-256(config path), hex>` (remote clients rejected; other users cannot write to it). A second launch sends one message, `aegiron-show` or `aegiron-open <9-digit id>` (at most 64 bytes, parsed strictly), and exits.
