# ReDesk

ระบบ remote desktop แบบโฮสต์เอง ออกแบบตามผลวิเคราะห์ใน [การวิเคราะห์ RustDesk.app (macOS).md](การวิเคราะห์%20RustDesk.app%20(macOS).md) และแก้จุดเสี่ยงที่พบในเอกสารนั้น

| ส่วน | Stack | หน้าที่ (เทียบ RustDesk) |
|---|---|---|
| `server/` | Go 1.27, PostgreSQL 16, `coder/websocket`, `pgx` | hbbs (rendezvous) + hbbr (relay) + API server (Pro) รวมในไบนารีเดียว พอร์ตเดียว `21114` |
| `agent/` | Rust (tokio, xcap, enigo, x25519/ed25519-dalek, aes-gcm) | ฝั่งเครื่องที่ถูกควบคุม: จับภาพจอ, รับ input, clipboard, เข้ารหัส E2E |
| `web/` | Bun, SvelteKit 3 (Svelte 5), Tailwind 4, guacamole-common-js | เว็บคอนโซล (สมุดที่อยู่, อุปกรณ์, ผู้ใช้, audit, ตั้งค่า) + **ตัวควบคุมผ่านเบราว์เซอร์** |
| guacd | Apache Guacamole (Docker) | engine โปรโตคอล RDP / VNC / SSH สำหรับ gateway ในเบราว์เซอร์ |

**รวมผู้ให้บริการอื่นไว้ในคอนโซลเดียว** (หน้า "การเชื่อมต่อ"): AnyDesk, TeamViewer, RustDesk (เปิดแอปของเจ้านั้น), RDP / VNC / SSH (เปิดในเบราว์เซอร์ผ่าน gateway) พร้อม vault รหัสผ่านที่เข้ารหัส, สิทธิ์ตามกลุ่ม, audit, ซิงก์อุปกรณ์และประวัติการเชื่อมต่อจาก TeamViewer API และนำเข้า CSV — รายละเอียด: [docs/INTEGRATIONS.md](docs/INTEGRATIONS.md)

```
 Browser (viewer) ──HTTPS/WSS──┐                ┌──WSS── redesk-agent (Rust)
   WebCrypto E2E               │  redesk-server │          xcap · enigo · arboard
                               └──  (Go) ───────┘
                                     │  signaling · blind relay · REST API
                                  PostgreSQL  (users, devices, address book, audit, sessions)
```

## สิ่งที่แก้จากผลวิเคราะห์ RustDesk

| ประเด็นในเอกสาร | ReDesk |
|---|---|
| ใช้เซิร์ฟเวอร์สาธารณะ `rs-ny.rustdesk.com` | ไม่มีค่า default ภายนอก ต้อง `init --server` กับเซิร์ฟเวอร์ของบริษัทเอง |
| fallback "danger accept invalid cert" | ไม่มี fallback ใดๆ agent ใช้ rustls ตรวจ cert เต็มรูปแบบ และปฏิเสธ `ws://` ไปยัง host ที่ไม่ใช่ localhost เว้นแต่สั่ง `--insecure` เอง |
| ตรึง public key ของเซิร์ฟเวอร์ | เซิร์ฟเวอร์เซ็น challenge ทุกครั้งที่ลงทะเบียน; key ไม่ตรง → agent ตัดทันที |
| relay อ่านได้? | E2E: X25519 + Ed25519 (ผูกกับเครื่องตอนลงทะเบียน) + AES-256-GCM; relay เห็นแค่ ciphertext |
| 2FA ผ่าน Telegram | TOTP มาตรฐาน ไม่มีการส่งข้อมูลออกไปบริการภายนอก |
| approve-mode / รหัสชั่วคราว | `--verification temporary/permanent/both`, `--approve-mode password/click/both`; รหัสชั่วคราวใช้ครั้งเดียวแล้วเปลี่ยนทันที; ผิด 5 ครั้งล็อก 60 วิ |
| daemon ระดับ root | agent รันเป็นผู้ใช้ปกติ ไม่ติดตั้ง LaunchDaemon เอง |
| ควบคุมเครื่องใหม่ | Enrollment token (ตั้งให้บังคับได้), ปิดใช้งาน/รีเซ็ตกุญแจอุปกรณ์ได้จากคอนโซล |
| ตรวจสอบย้อนหลัง | Audit log ทุก login, เซสชัน (ระยะเวลา, bytes), การเปลี่ยนแปลงอุปกรณ์/ผู้ใช้/ตั้งค่า และเหตุการณ์ที่ agent รายงาน (ใส่รหัสผิด, ถูกปฏิเสธ, ล็อก, ส่ง/รับไฟล์) |
| ใครเข้าเครื่องไหนได้ | นโยบาย "ตามกลุ่ม": ผู้ใช้ทั่วไปเชื่อมต่อได้เฉพาะเครื่องในกลุ่มอุปกรณ์ที่ตนเป็นสมาชิก, enrollment token ผูกกลุ่มให้เครื่องใหม่อัตโนมัติ |
| ติดตั้งแบบ unattended | `redesk-agent install` ใช้ LaunchAgent / systemd **ระดับผู้ใช้** (ไม่ใช่ root daemon); การกดยอมรับขึ้นเป็นกล่องโต้ตอบบนหน้าจอ |
| Mac ชิป Apple รันผ่าน Rosetta | build native arm64 (`cargo build --release`) |

รายละเอียดโปรโตคอล: [docs/PROTOCOL.md](docs/PROTOCOL.md) · ผู้ให้บริการอื่นและ gateway: [docs/INTEGRATIONS.md](docs/INTEGRATIONS.md)

## เริ่มใช้งาน (เครื่องพัฒนา)

ต้องมี Go 1.27+, Rust 1.85+, Bun 1.2+, PostgreSQL 16

```sh
make dev-db                      # Postgres + role/db "redesk"
cd web && bun install && bun run build && cd ..
make dev-guacd                   # (ไม่บังคับ) gateway RDP/VNC/SSH — ต้องมี Docker
REDESK_ADMIN_PASSWORD='change-me-now-123' make server
#   log จะแสดง server public_key — ใช้ตอน init agent
```

เปิด http://localhost:21114 แล้วล็อกอิน `admin@redesk.local` (ถ้าไม่ตั้ง `REDESK_ADMIN_PASSWORD` รหัสสุ่มจะพิมพ์ใน log)

**โปรแกรม ReDesk (หน้าต่าง + tray)** — หน้าตาแบบ RustDesk: แท็บด้านบน, แผง "เครื่องของคุณ" (ID/รหัสผ่านใช้ครั้งเดียว), การ์ด "ควบคุมเครื่องอื่น", รายชื่อเครื่องแบบการ์ดสีตาม ID หรือแบบรายการ, หน้าต่างการเชื่อมต่อเข้า (อนุญาต/ปฏิเสธ/ตัดการเชื่อมต่อ แยกแท็บต่อผู้ควบคุม) และหน้าตั้งค่าแบบเมนูซ้าย:

| หน้าหลัก | คำขอควบคุม | ตั้งค่า | เข้าสู่ระบบ |
|---|---|---|---|
| ![](docs/images/agent-home.png) | ![](docs/images/agent-approval.png) | ![](docs/images/agent-settings.png) | ![](docs/images/agent-login.png) |

**ควบคุมเครื่องในหน้าต่างโปรแกรม (เหมือน RustDesk):** กดเชื่อมต่อจากสมุดที่อยู่หรือใส่ ID แล้วจะเปิดหน้าต่างแยกต่อเครื่อง — ภาพหน้าจอ, เมาส์/คีย์บอร์ด (พิมพ์ไทยได้, Cmd บน Mac = Ctrl บน Windows), คลิปบอร์ดสองทาง, หลายจอ, ปรับคุณภาพ, Ctrl+Alt+Del, เต็มจอ และ**ลากไฟล์มาวางเพื่อส่ง** เข้ารหัส E2E แบบเดียวกับเว็บ (ตรวจลายเซ็นเครื่องปลายทาง)

![](docs/images/agent-viewer.png)

หน้าต่างควบคุมใช้แถบเครื่องมือลอยกลางจอ (ปักหมุด/ซ่อนเองได้), เมนูจอ, คำสั่ง, คุณภาพภาพ, คีย์บอร์ด, ขนาดจริง 1:1 และหน้าต่างข้อมูลคุณภาพการเชื่อมต่อ — เว็บคอนโซลใช้รูปแบบเดียวกัน

![](docs/images/web-devices.png)

**วิดีโอ H.264 (0.4.0):** agent เข้ารหัสด้วย OpenH264 แบบปรับสำหรับภาพหน้าจอ ส่งเฉพาะเมื่อจอเปลี่ยน ทั้งแอปและเว็บ (WebCodecs) ถอดรหัสได้ ถ้าฝั่งใดไม่รองรับจะกลับไปใช้ JPEG อัตโนมัติ และแสดงเคอร์เซอร์ของเครื่องปลายทางให้ด้วย — รายละเอียดใน [PROTOCOL.md](docs/PROTOCOL.md#video-codecs)

![](docs/images/web-viewer-h264.png)

**หน้าแรกแบบ RustDesk:** แท็บ ล่าสุด / รายการโปรด / สมุดที่อยู่ / ในเครือข่าย (ค้นหาเครื่อง ReDesk ในวงแลนเดียวกัน), ค้นหาด้วยชื่อ ID หรือแท็ก, ลิงก์ `redesk://connect/<id>` (ถามยืนยันก่อนเชื่อมต่อ), ล็อกหน้าตั้งค่าด้วย PIN, จำขนาดหน้าต่าง

![](docs/images/agent-lan.png)

**ปลุกเครื่อง (Wake-on-LAN):** ปุ่ม "ปลุกเครื่อง" ในคอนโซล ให้ agent ที่ออนไลน์ในวงแลนเดียวกันส่ง magic packet ให้

**SSO ขององค์กร (OIDC):** ตั้งค่าในคอนโซล (Google Workspace, Entra ID, Keycloak ฯลฯ) ใช้ได้ทั้งเว็บและปุ่ม "เข้าสู่ระบบด้วย SSO ขององค์กร" ในแอป

**อัปเดต agent:** แอดมินอัปโหลดไฟล์ติดตั้งในคอนโซล แอปตรวจหาอัปเดตในหน้าตั้งค่า ดาวน์โหลดเมื่อผู้ใช้กด และตรวจ SHA-256 ก่อนเก็บไฟล์ (ไม่ติดตั้งเอง)

![](docs/images/agent-update.png)

**macOS:** `ReDesk.app` + `.dmg` (มีการ์ดขอสิทธิ์ Screen Recording / Accessibility ในหน้าตั้งค่า) และกันเครื่องหลับระหว่างมีผู้ควบคุม

**เข้าสู่ระบบในโปรแกรม (เหมือน RustDesk):** เปิดโปรแกรมครั้งแรกแล้วเข้าสู่ระบบด้วยบัญชีคอนโซล เครื่องจะลงทะเบียนเป็นของผู้ใช้นั้นเอง (ดึงกุญแจเซิร์ฟเวอร์ผ่าน HTTPS) แสดงสมุดที่อยู่พร้อมสถานะออนไลน์ ปุ่ม "เชื่อมต่อ" เปิดหน้าต่างควบคุมในโปรแกรม และปุ่ม "เปิดคอนโซล" ในตั้งค่าเปิดเว็บคอนโซล — CLI: `redesk-agent login --server https://… --email …`

**Windows:** ตัวติดตั้ง `ReDesk-Agent-Setup-*.exe` (ไม่ต้องใช้สิทธิ์ admin, ติดตั้งแบบเงียบได้) — ดู [deploy/windows/README.md](deploy/windows/README.md)

ติดตั้ง agent บนเครื่องที่จะถูกควบคุม (คำสั่งพร้อม key อยู่ในหน้า **ตั้งค่าเซิร์ฟเวอร์**):

```sh
cd agent && cargo build --release
./target/release/redesk-agent init --server https://redesk.example.com --key <SERVER_PUBLIC_KEY>
./target/release/redesk-agent run          # แสดง ID 9 หลัก + รหัสผ่านชั่วคราว
./target/release/redesk-agent set-password # (ถ้าต้องการ unattended access)
./target/release/redesk-agent set --approve-mode both   # ต้องมีคนกดยอมรับที่เครื่องด้วย
./target/release/redesk-agent install       # เริ่มอัตโนมัติเมื่อล็อกอิน (uninstall เพื่อลบ)
./target/release/redesk-agent show-password # ดูรหัสชั่วคราวของ agent ที่รันเป็น service
./target/release/redesk-agent set --allow-file-transfer false   # ปิดการโอนไฟล์
```

เมื่อใช้ approve-mode `click`/`both` agent จะเปิดกล่องโต้ตอบ "อนุญาต/ปฏิเสธ" บนหน้าจอ (macOS: osascript, Linux: zenity) และถามใน terminal ด้วยถ้ามี ถ้าไม่มีใครตอบภายใน 30 วินาทีจะถือว่าปฏิเสธ

### ฟีเจอร์ในหน้าควบคุม

- ภาพหน้าจอ + เมาส์/คีย์บอร์ด (พิมพ์ภาษาไทยได้), เลือกจอ, ปรับคุณภาพ, เต็มจอ, Ctrl+Alt+Del
- คลิปบอร์ดสองทาง และ "พิมพ์จากคลิปบอร์ด"
- **โอนไฟล์**: ปุ่ม "ไฟล์" เปิดตัวเรียกดูไฟล์ของเครื่องปลายทาง (ดาวน์โหลด/อัปโหลด) หรือลากไฟล์มาวางบนหน้าจอเพื่อส่ง (ไปที่โฟลเดอร์ที่เปิดอยู่ หรือ `~/Downloads/ReDesk`)

### กลุ่มอุปกรณ์และสิทธิ์

1. **กลุ่มอุปกรณ์** → สร้างกลุ่มและเลือกสมาชิก
2. กำหนดกลุ่มให้เครื่องที่หน้า **อุปกรณ์ทั้งหมด** หรือสร้าง enrollment token ที่ผูกกลุ่มไว้ (`init --enroll-token ...`) เพื่อให้เครื่องใหม่เข้ากลุ่มเอง
3. **ตั้งค่าเซิร์ฟเวอร์** → การเข้าถึง: "ตามกลุ่ม" แล้วผู้ใช้ทั่วไปจะเห็นเครื่องของกลุ่มตนที่หน้าแรก และเชื่อมต่อเครื่องอื่นไม่ได้ (ถูกบันทึกเป็น `session.denied`)

macOS: อนุญาต **Screen Recording** และ **Accessibility** ให้ terminal/ไบนารีที่รัน agent ใน System Settings → Privacy & Security

พัฒนาเว็บแบบ hot reload: `make web` (http://localhost:5173 proxy ไปที่ server)

## Production

**Mac server (แนะนำ):** `deploy/macos/build-kit.sh` สร้างชุดติดตั้งใน `dist/` แล้วรัน `sudo ./install.sh --domain <โดเมน> --email <อีเมล>` บนเครื่อง Mac server — ดู [deploy/macos/README.md](deploy/macos/README.md)

**Linux / Docker:**

```sh
echo "POSTGRES_PASSWORD=$(openssl rand -hex 16)" > .env
echo "REDESK_ADMIN_PASSWORD=..." >> .env
docker compose up -d --build
```

วาง reverse proxy ที่มี TLS (เช่น Caddy: `reverse_proxy localhost:21114`) ไว้ข้างหน้า หรือกำหนด `REDESK_TLS_CERT`/`REDESK_TLS_KEY` **สำรองไฟล์ `/data/server_ed25519.key` และ `/data/vault.key`** — ถ้า server key หาย agent ทุกเครื่องต้อง init ใหม่ ถ้า vault key หาย รหัสผ่านที่เก็บไว้ทั้งหมดใช้ไม่ได้ (เก็บ vault key แยกจาก backup ฐานข้อมูล)

| ตัวแปร | ค่าเริ่มต้น | |
|---|---|---|
| `REDESK_LISTEN` | `:21114` | |
| `REDESK_DATABASE_URL` | `postgres://redesk:redesk@localhost:5432/redesk?sslmode=disable` | |
| `REDESK_DATA_DIR` | `./data` | เก็บ server identity key |
| `REDESK_WEB_DIR` | `../web/build` | |
| `REDESK_TLS_CERT` / `REDESK_TLS_KEY` | — | เปิด HTTPS ในตัว |
| `REDESK_TRUST_PROXY` | `false` | อ่าน IP จาก `X-Forwarded-For` |
| `REDESK_COOKIE_SECURE` | true เมื่อเปิด TLS | |
| `REDESK_ALLOWED_ORIGINS` | — | origin เพิ่มเติมสำหรับ WebSocket (dev: `localhost:5173`) |
| `REDESK_ADMIN_EMAIL` / `REDESK_ADMIN_PASSWORD` | — | สร้าง admin คนแรกเมื่อ DB ว่าง |
| `REDESK_VAULT_KEY` | `<data>/vault.key` | กุญแจ AES-256 (base64) สำหรับรหัสผ่านใน vault — เก็บแยกจาก backup DB |
| `REDESK_GUACD_ADDR` | — (ปิด gateway) | `host:4822` ของ guacd |
| `REDESK_GATEWAY_RECORDING_PATH` | — (ไม่บันทึก) | โฟลเดอร์ใน container ของ guacd สำหรับบันทึกเซสชัน RDP/VNC/SSH |
| `REDESK_TIMEZONE` | `Asia/Bangkok` | timezone ที่ส่งให้เซสชัน gateway |
| `REDESK_SYNC_MINUTES` | `15` | รอบซิงก์ TeamViewer API |

## ทดสอบ

```sh
make test                                   # go vet, cargo test (crypto + dirty-rect), svelte-check
cd web && REDESK_PASSWORD=... bun scripts/e2e-viewer.ts <device-id> <device-password>
```

`e2e-viewer.ts` ใช้โค้ด `src/lib/crypto.ts` ชุดเดียวกับเบราว์เซอร์ คุยกับ agent จริงผ่าน relay: handshake → ใส่รหัสผิด → ใส่รหัสถูก → รับเฟรมภาพ

## ข้อจำกัดของเวอร์ชันนี้ (roadmap)

- H.264 เข้ารหัสด้วย CPU (ยังไม่ใช้ hardware encoder เช่น VideoToolbox/NVENC) และยังไม่มี VP9/AV1
- วิ่งผ่าน relay เสมอ ยังไม่มี P2P/UDP hole punching
- ยังไม่มี: เสียง, ไวท์บอร์ด, ควบคุมที่หน้าจอล็อกอินของ macOS (ต้องใช้ root daemon ซึ่งตั้งใจไม่ทำ)
- ผู้ให้บริการอื่น: ยังไม่ได้ทดสอบกับบัญชี TeamViewer จริง (ทดสอบกับ mock ตามรูปแบบในเอกสาร), AnyDesk ไม่มี API สาธารณะสำหรับอุปกรณ์/เซสชัน, ยังไม่มีหน้าเล่นซ้ำ session recording, RDP ยังไม่ได้ทดสอบกับ Windows จริง (SSH และ VNC ทดสอบแล้ว)
- ลิงก์ `redesk://` บน macOS ใช้ได้เฉพาะตอนส่งเป็นอาร์กิวเมนต์ (eframe ยังไม่รับ event เปิด URL ของ macOS) — บน Windows ใช้ได้เต็มที่ผ่านตัวติดตั้ง
- Wake-on-LAN ต้องมี agent อีกเครื่องออนไลน์ในวงแลนเดียวกัน และเครื่องเป้าหมายต้องเปิด WoL ใน BIOS/การ์ดแลน
- service บน Windows ยังไม่รองรับ (ใช้ Task Scheduler รัน `redesk-agent run` ตอน logon)
- โอนไฟล์ทีละไฟล์ (ยังไม่รองรับทั้งโฟลเดอร์) และไฟล์ที่ดาวน์โหลดจะถูกเก็บในหน่วยความจำเบราว์เซอร์ก่อนบันทึก
