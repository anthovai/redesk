# ReDesk protocol (v1)

Everything runs over one HTTPS port (default `21114`): REST API, agent signaling (`/ws/agent`) and the relay (`/ws/relay`).

## 1. Agent registration (`/ws/agent`, text JSON)

The agent pins the server's Ed25519 public key at `init` time.

```
server → agent  {"t":"challenge","nonce":S}
agent  → server {"t":"register","id","pk","nonce":A,"sig","hostname","os","version","enroll_token"?}
                sig = Ed25519(agent_key, "redesk-register-v1\n" + id + "\n" + S + "\n" + A)
server → agent  {"t":"registered","sig":Ed25519(server_key, "redesk-server-v1\n" + id + "\n" + A + "\n" + S)}
             or {"t":"error","code":"id_taken|enroll_required|disabled|bad_signature|...","message"}
```

- The first registration binds `id → pk` (TOFU per device). A different key for the same id is rejected (`id_taken`) and audited as `device.key_mismatch`. Admins can reset the binding.
- With `require_enroll` on, a new or reset device must present a valid enrollment token.
- If the server signature fails verification, the agent aborts: *invalid public key from server*.
- Keepalive: `{"t":"ping"}` / `{"t":"pong"}` every 20 s.
- Session request: `{"t":"session","sid","token","relay":"/ws/relay","viewer":{"email","name","ip"}}`.
- Agent → server audit events: `{"t":"event","sid","kind","detail"}` with `kind` in `auth_failed | auth_ok | denied | locked | file_upload | file_download`. They are stored as `agent.<kind>` and attributed to the viewer of that session.

## 2. Relay (`/ws/relay?sid=&token=`)

The viewer token goes only to the console user who requested the session, and the relay also requires that user's login cookie. The agent token is sent over signaling. Once both sides connect, the server sends `{"t":"paired"}` to each and then forwards frames **blind**. It counts bytes for the audit log but cannot read the content.

## 3. E2E handshake (text JSON through the relay)

```
viewer → agent  {"t":"hello","v":1,"epk":X25519 public (32B, base64)}
agent  → viewer {"t":"hello","v":1,"epk":...,"sig":Ed25519(device_key, transcript)}

transcript = "redesk-kx-v1" || sid || viewer_epk || agent_epk
```

The viewer verifies `sig` against the device public key fetched from the API. That key was bound at registration, so a relay or server that swaps keys cannot impersonate the device. The UI shows the key's fingerprint, which can be compared with `redesk-agent info`.

```
shared  = X25519(viewer_esk, agent_epk)
salt    = SHA-256(transcript)
k_v2a   = HKDF-SHA256(shared, salt, "redesk v1 viewer->agent", 32)
k_a2v   = HKDF-SHA256(shared, salt, "redesk v1 agent->viewer", 32)
AEAD    = AES-256-GCM, nonce = 0x00000000 || u64_be(counter), one counter per direction
```

Every later message is binary ciphertext. A dropped, reordered, or replayed message fails decryption because the counter is implicit.

## 4. Messages inside the encrypted channel

Byte 0 is the type: `0x01` JSON, `0x02` video rectangle (`x, y, frame_w, frame_h` as u16 BE, followed by a JPEG), `0x03` file chunk (transfer id as u32 BE, followed by up to 64 KiB of data).

| Direction | Message |
|---|---|
| agent→viewer | `auth_required {password, click}`, `auth_pending`, `auth_result {ok, reason?, attempts_left?, retry_after?}`, `info {hostname, os, displays[], input, clipboard, files}`, `clip {s}` |
| viewer→agent | `auth {password}`, `mm {x,y}`, `mb {b,down,x,y}`, `wh {dx,dy}`, `key {code,key,down}`, `text {s}`, `clip {s}`, `cfg {fps,quality,max_width,display}`, `refresh`, `bye` |
| files viewer→agent | `ls {path}` (`""` = home), `put {id,dir,name,size}` followed by `0x03` chunks and then `put_end {id}`, `get {id,path}`, `cancel {id}` |
| files agent→viewer | `ls {path,parent,sep,entries[{name,dir,size,mtime}]}`, `put_ok {id,path}`, `put_done {id,path}`, `get_start {id,name,size}` followed by `0x03` chunks and then `get_end {id}`, `fs_error {op,id?,message}` |

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
