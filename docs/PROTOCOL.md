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
| agent→viewer | `auth_required {password, click}`, `auth_pending`, `auth_result {ok, reason?, attempts_left?, retry_after?}`, `info {hostname, os, displays[], input, clipboard, files, codec}`, `cursor {x,y,visible}`, `clip {s}` |
| viewer→agent | `auth {password, codecs[]}`, `mm {x,y}`, `mb {b,down,x,y}`, `wh {dx,dy}`, `key {code,key,down}`, `text {s}`, `clip {s}`, `cfg {fps,quality,max_width,display}`, `refresh` (alias `keyframe`), `bye` |
| files viewer→agent | `ls {path}` (`""` = home), `put {id,dir,name,size}` followed by `0x03` chunks and then `put_end {id}`, `get {id,path}`, `cancel {id}` |
| files agent→viewer | `ls {path,parent,sep,entries[{name,dir,size,mtime}]}`, `put_ok {id,path}`, `put_done {id,path}`, `get_start {id,name,size}` followed by `0x03` chunks and then `get_end {id}`, `fs_error {op,id?,message}` |

### Video codecs

The viewer lists the codecs it can decode in `auth.codecs`, best first (`["h264","jpeg"]`). The agent picks H.264 when offered, otherwise JPEG, and reports the choice in `info.codec`.

- **H.264** (OpenH264, Constrained Baseline, screen-content mode, rate control from resolution × fps × quality): whole frames as `0x04`, sent only when the screen changed. A keyframe goes out at start, after `cfg` changes, on `refresh`/`keyframe`, and every ~10 s of activity. Browsers decode with WebCodecs (`avc1.42E033`) and the native app with OpenH264. A viewer drops deltas until its first keyframe and sends `keyframe` after a decoder error. If the agent's encoder fails, it falls back to JPEG rectangles in the same session, so every viewer must accept both.
- **JPEG** (fallback): only the changed bounding rectangle as `0x02`.

Screen captures do not include the pointer, so the agent sends `cursor` (normalised to the streamed display, `visible=false` when it is on another display) whenever the pointer moves. Viewers draw it while their own pointer is not over the remote screen.

Uploads keep only the file name (any path the viewer sends is dropped), never overwrite an existing file (`name (1).ext`), must match the announced size, and are deleted if the session ends before `put_end`. Downloads are read on a separate task and share the session's back-pressured outbound queue with video.

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
