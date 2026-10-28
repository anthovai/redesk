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

### Organisation SSO (OpenID Connect)

- Browser: `GET /api/oidc/login?next=` → identity provider (Authorization Code + PKCE, `state`, `nonce`, bound to the browser by a cookie) → `GET /api/oidc/callback`. The ID token is verified (signature, issuer, audience, expiry, nonce); `email_verified` and the allowed domains are enforced. TOTP is not asked again: the IdP is responsible for MFA.
- App: `POST /api/oidc/auth {device}` → `{code, url, expires_in}`. The app opens `url` in the browser. That page shows the requesting device and IP before continuing to the IdP. The app polls `GET /api/oidc/auth-query?code=` every 2 s and gets `{pending:true}`, or the same payload as `/api/agent/login`. The poll code never appears in the browser URL.
- Admin: `GET/PUT /api/oidc/settings`. The client secret is sealed with the vault and never returned.

## 6. Agent updates

- Admins upload builds: `POST /api/agent/releases` (multipart, up to 300 MB, stored by SHA-256), then enable or disable them.
- Clients check with `GET /api/agent/update?os=&arch=&version=`, which returns the newest enabled release that is newer by semver.
- Clients download from `GET /api/agent/releases/{id}/download`, served with `X-Checksum-SHA256`.
- The app downloads only after the user asks, from the same server only. It keeps the file only if the size and SHA-256 match, and never installs it by itself.
