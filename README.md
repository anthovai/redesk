<p align="center"><img src="agent/assets/aegiron.png" width="104" alt="Aegiron"></p>

<h1 align="center">Aegiron</h1>

<p align="center">
  <b>รีโมตเดสก์ท็อปที่องค์กรติดตั้งบนเซิร์ฟเวอร์ของตัวเอง</b><br>
  ใช้งานแบบ RustDesk · เข้ารหัสต้นทาง-ปลายทาง · มีคอนโซลจัดการเครื่อง ผู้ใช้ และ audit ในที่เดียว
</p>

<p align="center">
  <a href="#ดาวน์โหลด">ดาวน์โหลด</a> ·
  <a href="#เริ่มต้นใช้งาน">เริ่มต้นใช้งาน</a> ·
  <a href="#ความปลอดภัย">ความปลอดภัย</a> ·
  <a href="#พัฒนาต่อ">พัฒนาต่อ</a> ·
  <a href="#อัปเกรดจาก-redesk">อัปเกรดจาก ReDesk</a>
</p>

<p align="center"><img src="docs/images/agent-home.png" width="760" alt="หน้าหลักของแอป Aegiron"></p>

> Aegiron เดิมชื่อ **ReDesk** (ถึงเวอร์ชัน 0.4.0) เครื่องและเซิร์ฟเวอร์ที่ติดตั้งไว้แล้วอัปเกรดได้โดยไม่ต้องตั้งค่าใหม่ — ดู [อัปเกรดจาก ReDesk](#อัปเกรดจาก-redesk)

## ทำอะไรได้บ้าง

**ควบคุมเครื่องจากระยะไกล**
- แอปบน macOS และ Windows หน้าตาแบบ RustDesk: ID 9 หลักกับรหัสผ่านใช้ครั้งเดียว, ใส่ ID แล้วกดเชื่อมต่อ, รายชื่อเครื่องเป็นการ์ดหรือรายการ
- หน้าต่างควบคุมแยกต่อเครื่อง พร้อมแถบเครื่องมือลอย: เลือกจอ, ปรับคุณภาพ, ขนาดจริง 1:1, Ctrl+Alt+Del, เต็มจอ, ข้อมูลคุณภาพการเชื่อมต่อ
- วิดีโอ H.264 ส่งเฉพาะตอนจอเปลี่ยน ใช้ hardware encoder เมื่อมี (สำรองเป็น OpenH264 และ JPEG อัตโนมัติ), แสดงเคอร์เซอร์ของเครื่องปลายทาง
- เสียงจากเครื่องปลายทาง (Opus) ทั้งในแอปและในเบราว์เซอร์ ปิดเสียงได้จากแถบเครื่องมือ
- เมาส์/คีย์บอร์ด (พิมพ์ไทยได้, ⌘ บน Mac แปลงเป็น Ctrl บน Windows), คลิปบอร์ดสองทาง, ส่ง/รับไฟล์ (ลากวางได้)
- ควบคุมผ่านเบราว์เซอร์ได้ด้วย ไม่ต้องติดตั้งอะไรที่ฝั่งผู้ควบคุม

**ฝั่งเครื่องที่ถูกควบคุม**
- หน้าต่างการเชื่อมต่อเข้าแยกแท็บต่อผู้ควบคุม: อนุญาต/ปฏิเสธ (ปฏิเสธอัตโนมัติใน 30 วินาที), ตัดการเชื่อมต่อได้ทุกเมื่อ
- เลือกได้ว่าจะใช้รหัสผ่าน, ให้คนกดอนุญาต หรือทั้งสองอย่าง; กำหนดสิทธิ์เมาส์/คีย์บอร์ด, คลิปบอร์ด, ไฟล์
- ล็อกหน้าตั้งค่าด้วย PIN, กันเครื่องหลับระหว่างมีผู้ควบคุม, เริ่มอัตโนมัติตอนเข้าเครื่อง

**จัดการทั้งองค์กร (เว็บคอนโซล)**
- สมุดที่อยู่, อุปกรณ์ทั้งหมด, กลุ่มอุปกรณ์และสิทธิ์ตามกลุ่ม, ผู้ใช้และ 2FA (TOTP)
- เข้าสู่ระบบด้วย SSO ขององค์กร (OpenID Connect: Google Workspace, Microsoft Entra ID, Keycloak ฯลฯ) ทั้งในเว็บและในแอป
- จดจำเครื่องที่ผ่าน 2FA แล้ว ผู้ใช้และแอดมินเพิกถอนได้จากคอนโซล
- Audit log ทุกการเข้าสู่ระบบ, เซสชัน, การโอนไฟล์ และการเปลี่ยนแปลงตั้งค่า
- ปลุกเครื่อง (Wake-on-LAN), ค้นหาเครื่องในวงแลนเดียวกัน, ลิงก์ `aegiron://connect/<id>`
- แจกจ่ายเวอร์ชันใหม่ของแอปผ่านเซิร์ฟเวอร์ตัวเอง (ตรวจ SHA-256 ก่อนเก็บไฟล์, ไม่ติดตั้งเองโดยไม่ถาม)

**รวมเครื่องมืออื่นไว้ในคอนโซลเดียว** — AnyDesk, TeamViewer, RustDesk (เปิดแอปของเจ้านั้น) และ RDP / VNC / SSH (เปิดในเบราว์เซอร์ผ่าน gateway) พร้อม vault รหัสผ่านที่เข้ารหัส, ซิงก์จาก TeamViewer API และนำเข้า CSV — ดู [docs/INTEGRATIONS.md](docs/INTEGRATIONS.md)

## ภาพหน้าจอ

| ควบคุมเครื่อง | คำขอเชื่อมต่อเข้า |
|---|---|
| ![](docs/images/agent-viewer.png) | ![](docs/images/agent-approval.png) |
| **ตั้งค่า** | **เครื่องในวงแลน** |
| ![](docs/images/agent-settings.png) | ![](docs/images/agent-lan.png) |
| **เว็บคอนโซล: อุปกรณ์ทั้งหมด** | **ควบคุมผ่านเบราว์เซอร์** |
| ![](docs/images/web-devices.png) | ![](docs/images/web-viewer-h264.png) |

## ดาวน์โหลด

ไฟล์ติดตั้งอยู่ที่หน้า [Releases](https://github.com/anthovai/redesk/releases)

| ไฟล์ | ใช้กับ |
|---|---|
| `Aegiron-<version>.dmg` | แอปสำหรับ macOS 11 ขึ้นไป (Apple Silicon และ Intel) |
| `Aegiron-Agent-Setup-<version>.exe` | ตัวติดตั้งสำหรับ Windows 10/11 (ไม่ต้องใช้สิทธิ์ admin, ติดตั้งแบบเงียบได้) |
| `aegiron-server-macos-<version>.tar.gz` | ชุดติดตั้งเซิร์ฟเวอร์บน Mac |
| `aegiron-agent-macos-<version>.tar.gz` | agent แบบ command line สำหรับ macOS |
| `SHA256SUMS` | ค่า checksum ของทุกไฟล์ |

ไฟล์ยังไม่ได้เซ็นด้วยใบรับรองขององค์กร: macOS ให้คลิกขวาแล้วเลือก "เปิด" ในครั้งแรก, Windows จะมีคำเตือนจาก SmartScreen

## เริ่มต้นใช้งาน

### 1. ติดตั้งเซิร์ฟเวอร์

**Mac Server (แนะนำ)** — ติดตั้งครบชุด: Aegiron server, HTTPS อัตโนมัติ (Caddy + Let's Encrypt), PostgreSQL, gateway RDP/VNC/SSH และสำรองข้อมูลทุกวัน

```sh
tar xzf aegiron-server-macos-<version>.tar.gz && cd aegiron-server-macos-<version>
sudo ./install.sh --domain aegiron.example.com --email it@example.com
```

เมื่อเสร็จจะแสดงรหัสผ่านแอดมินครั้งแรกและกุญแจเซิร์ฟเวอร์ — รายละเอียดใน [deploy/macos/README.md](deploy/macos/README.md)

**Linux / Docker**

```sh
echo "POSTGRES_PASSWORD=$(openssl rand -hex 16)" > .env
echo "AEGIRON_ADMIN_PASSWORD=<รหัสผ่านแอดมินครั้งแรก>" >> .env
docker compose up -d --build
```

แล้ววาง reverse proxy ที่มี TLS ไว้ข้างหน้า (เช่น Caddy: `reverse_proxy localhost:21114`) หรือตั้ง `AEGIRON_TLS_CERT` / `AEGIRON_TLS_KEY`

> **สำรองไฟล์ `server_ed25519.key` และ `vault.key` ในโฟลเดอร์ data แยกจาก backup ฐานข้อมูล** — ถ้ากุญแจเซิร์ฟเวอร์หาย ทุกเครื่องต้องลงทะเบียนใหม่ ถ้า vault key หาย รหัสผ่านที่เก็บไว้ใช้ไม่ได้ทั้งหมด

### 2. ติดตั้งแอปบนเครื่อง

- **macOS:** เปิด `.dmg` ลาก Aegiron ไปที่ Applications แล้วอนุญาต Screen Recording และ Accessibility ตามที่แอปแนะนำในหน้าตั้งค่า
- **Windows:** รัน `Aegiron-Agent-Setup-<version>.exe` — ติดตั้งให้ทั้งองค์กรแบบเงียบได้ด้วย `/S /SERVER=https://… /KEY=…` ดู [deploy/windows/README.md](deploy/windows/README.md)

เปิดแอปแล้ว **เข้าสู่ระบบด้วยบัญชีของคอนโซล** (หรือ SSO) เครื่องจะลงทะเบียนเป็นของผู้ใช้นั้นและเห็นสมุดที่อยู่ทันที

### 3. เชื่อมต่อ

ใส่ ID ของเครื่องปลายทางแล้วกด **เชื่อมต่อ** หรือดับเบิลคลิกการ์ดเครื่องในรายการ แล้วใส่รหัสผ่านที่แสดงบนเครื่องนั้น (หรือรอให้ปลายทางกดอนุญาต)

### ใช้แบบ command line

สำหรับเครื่องที่ไม่มีคนใช้ (เซิร์ฟเวอร์, kiosk) หรือการติดตั้งด้วยสคริปต์ — คำสั่งพร้อมกุญแจอยู่ในคอนโซลหน้า **ตั้งค่าเซิร์ฟเวอร์**

```sh
aegiron-agent init --server https://aegiron.example.com --key <SERVER_PUBLIC_KEY>
aegiron-agent login --email it@example.com      # หรือเข้าสู่ระบบแทนการใช้ --key
aegiron-agent set-password                      # รหัสผ่านถาวร สำหรับเครื่องที่ไม่มีคนเฝ้า
aegiron-agent set --approve-mode both           # ต้องใส่รหัสผ่านและมีคนกดอนุญาต
aegiron-agent install                           # เริ่มอัตโนมัติตอนเข้าเครื่อง (ระดับผู้ใช้ ไม่ใช่ root)
aegiron-agent show-password                     # ดูรหัสผ่านใช้ครั้งเดียวปัจจุบัน
aegiron-agent info                              # ID และลายนิ้วมือกุญแจของเครื่อง
```

## ความปลอดภัย

Aegiron ออกแบบจาก [ผลวิเคราะห์ RustDesk.app](การวิเคราะห์%20RustDesk.app%20(macOS).md) และแก้จุดเสี่ยงที่พบ:

| ประเด็นใน RustDesk | Aegiron |
|---|---|
| ค่าเริ่มต้นชี้ไปเซิร์ฟเวอร์สาธารณะ | ไม่มีเซิร์ฟเวอร์ภายนอกเลย ต้องตั้งค่าเซิร์ฟเวอร์ขององค์กรเอง |
| มี fallback ยอมรับใบรับรองที่ไม่ถูกต้อง | ตรวจใบรับรองเต็มรูปแบบเสมอ ไม่มี fallback; ไม่ยอมใช้ `ws://` ที่ไม่เข้ารหัสกับเครื่องอื่นนอกจาก localhost เว้นแต่สั่ง `--insecure` เอง |
| ตรึง public key ของเซิร์ฟเวอร์ | เซิร์ฟเวอร์เซ็น challenge ทุกครั้งที่ลงทะเบียน กุญแจไม่ตรงจะตัดทันที |
| relay อ่านข้อมูลได้หรือไม่ | เข้ารหัสต้นทาง-ปลายทาง X25519 + AES-256-GCM และตรวจลายเซ็น Ed25519 ของเครื่องปลายทาง relay เห็นแค่ข้อมูลที่เข้ารหัสแล้ว |
| 2FA ส่งผ่าน Telegram | TOTP มาตรฐาน ไม่ส่งข้อมูลออกไปบริการภายนอก |
| service ระดับ root | agent รันเป็นผู้ใช้ปกติ ไม่ติดตั้ง daemon ระดับ root |
| ใครเข้าเครื่องไหนก็ได้ที่รู้รหัส | สิทธิ์ตามกลุ่ม, enrollment token, ปิดใช้งาน/รีเซ็ตกุญแจเครื่องได้จากคอนโซล, audit log ครบ |
| รหัสผ่านชั่วคราว | ใช้ได้ครั้งเดียวแล้วเปลี่ยนทันที, ผิด 3 ครั้งต่อเซสชัน, ผิด 5 ครั้งติดกันล็อก 60 วินาที |
| บิลด์ x86_64 รันผ่าน Rosetta | บิลด์ native ทั้ง Apple Silicon และ Intel |

รายละเอียดโปรโตคอลและการเข้ารหัส: [docs/PROTOCOL.md](docs/PROTOCOL.md)

## สถาปัตยกรรม

```
 แอป Aegiron / เบราว์เซอร์ ──HTTPS/WSS──┐                  ┌──WSS── aegiron-agent
   (ผู้ควบคุม, E2E)                     │  aegiron-server  │        (เครื่องที่ถูกควบคุม)
                                        └──     (Go)    ───┘
                                            │ signaling · relay แบบมองไม่เห็นข้อมูล · REST API
                                         PostgreSQL (ผู้ใช้, อุปกรณ์, สมุดที่อยู่, audit, เซสชัน)
```

| ส่วน | เทคโนโลยี | หน้าที่ (เทียบ RustDesk) |
|---|---|---|
| `server/` | Go, PostgreSQL 16 | rendezvous + relay + API (เทียบ hbbs + hbbr + Pro) รวมในไบนารีเดียว พอร์ตเดียว `21114` |
| `agent/` | Rust, egui, OpenH264 | แอปบนเครื่อง (`aegiron`) และ CLI (`aegiron-agent`): จับภาพจอ, input, คลิปบอร์ด, ไฟล์, หน้าต่างควบคุม |
| `web/` | Bun, SvelteKit (Svelte 5), Tailwind | เว็บคอนโซลและตัวควบคุมผ่านเบราว์เซอร์ (WebCrypto, WebCodecs) |
| guacd | Apache Guacamole | engine ของ gateway RDP / VNC / SSH ในเบราว์เซอร์ (ไม่บังคับ) |

### ตั้งค่าเซิร์ฟเวอร์ (environment variables)

| ตัวแปร | ค่าเริ่มต้น | ความหมาย |
|---|---|---|
| `AEGIRON_LISTEN` | `:21114` | พอร์ตของเซิร์ฟเวอร์ |
| `AEGIRON_DATABASE_URL` | `postgres://aegiron:aegiron@localhost:5432/aegiron?sslmode=disable` | ฐานข้อมูล |
| `AEGIRON_DATA_DIR` | `./data` | กุญแจเซิร์ฟเวอร์และ vault key |
| `AEGIRON_WEB_DIR` | `../web/build` | ไฟล์เว็บคอนโซลที่ build แล้ว |
| `AEGIRON_PUBLIC_URL` | — | URL ภายนอก (ใช้กับ SSO) |
| `AEGIRON_TLS_CERT` / `AEGIRON_TLS_KEY` | — | เปิด HTTPS ในตัว |
| `AEGIRON_TRUST_PROXY` | `false` | อ่าน IP ผู้ใช้จาก `X-Forwarded-For` (เปิดเมื่ออยู่หลัง reverse proxy) |
| `AEGIRON_COOKIE_SECURE` | `true` เมื่อเปิด TLS | |
| `AEGIRON_ALLOWED_ORIGINS` | — | origin เพิ่มเติมสำหรับ WebSocket |
| `AEGIRON_ADMIN_EMAIL` / `AEGIRON_ADMIN_PASSWORD` | `admin@aegiron.local` / สุ่ม | แอดมินคนแรก (เฉพาะตอนฐานข้อมูลว่าง) |
| `AEGIRON_VAULT_KEY` | `<data>/vault.key` | กุญแจ AES-256 (base64) ของ vault |
| `AEGIRON_GUACD_ADDR` | — (ปิด gateway) | `host:4822` ของ guacd |
| `AEGIRON_GATEWAY_RECORDING_PATH` | — (ไม่บันทึก) | โฟลเดอร์บันทึกเซสชัน RDP/VNC/SSH ใน container ของ guacd |
| `AEGIRON_TIMEZONE` | `Asia/Bangkok` | timezone ของเซสชัน gateway |
| `AEGIRON_SYNC_MINUTES` | `15` | รอบซิงก์ TeamViewer API |

ชื่อเดิม `REDESK_*` ยังใช้ได้ถ้าไม่ได้ตั้ง `AEGIRON_*`

## พัฒนาต่อ

ต้องมี Go 1.27+, Rust 1.85+, Bun 1.2+ และ PostgreSQL 16

```sh
make dev-db                                       # PostgreSQL + role/database "aegiron"
cd web && bun install && bun run build && cd ..
make dev-guacd                                    # (ไม่บังคับ) gateway RDP/VNC/SSH ต้องมี Docker
AEGIRON_ADMIN_PASSWORD='change-me-now-123' make server
```

เปิด http://localhost:21114 แล้วเข้าสู่ระบบด้วย `admin@aegiron.local` — log จะแสดงกุญแจเซิร์ฟเวอร์สำหรับ `aegiron-agent init`

| คำสั่ง | |
|---|---|
| `make web` | เว็บแบบ hot reload ที่ http://localhost:5173 |
| `make agent` | รัน agent (หลัง `aegiron-agent init`) |
| `cd agent && cargo run --bin aegiron` | รันแอปบนเครื่อง |
| `make test` | go vet + go test, cargo test, svelte-check |
| `deploy/macos/build-kit.sh` | สร้างชุดติดตั้งเซิร์ฟเวอร์, agent และ `.dmg` ใน `dist/` |
| `deploy/windows/build-installer.sh` | สร้างตัวติดตั้ง Windows (cross-compile จาก Mac/Linux) |

ทดสอบการเชื่อมต่อจริงทั้งเส้นทาง (handshake → รหัสผิด → รหัสถูก → รับภาพ) ด้วยโค้ดเข้ารหัสชุดเดียวกับเบราว์เซอร์:

```sh
cd web && AEGIRON_PASSWORD=... bun scripts/e2e-viewer.ts <device-id> <device-password>
```

## ข้อจำกัดและแผนต่อไป

- ยังไม่มี VP9/AV1 (H.264 ใช้ hardware encoder ของ VideoToolbox / Media Foundation และสำรองเป็น OpenH264); hardware encoder และเสียงบน Windows ยังไม่ได้ทดสอบกับเครื่องจริง
- เสียงจากเครื่องปลายทางต้องใช้ macOS 13 ขึ้นไป หรือ Windows; ไม่มีเสียงในไฟล์บันทึกเซสชัน
- ยังไม่มีการควบคุมที่หน้าจอล็อกอินของ macOS (ต้องใช้ daemon ระดับ root ซึ่งตั้งใจไม่ทำ)
- Wake-on-LAN ต้องมี agent อีกเครื่องออนไลน์ในวงแลนเดียวกัน และเครื่องเป้าหมายต้องเปิด WoL ไว้
- ผู้ให้บริการอื่น: TeamViewer ทดสอบกับ mock เท่านั้น, AnyDesk ไม่มี API สาธารณะ, RDP ยังไม่ได้ทดสอบกับ Windows จริง

## อัปเกรดจาก ReDesk

เวอร์ชัน 0.5.0 เปลี่ยนชื่อจาก ReDesk เป็น Aegiron ข้อมูล กุญแจ และฐานข้อมูลเดิมใช้ต่อได้ทั้งหมด

- **เซิร์ฟเวอร์ Mac:** รัน `sudo ./install.sh` จากชุดติดตั้งใหม่ตามปกติ สคริปต์จะย้าย `/usr/local/{etc,var,var/log}/redesk` ไปที่ `…/aegiron`, เปลี่ยน `REDESK_*` ในไฟล์ตั้งค่าเป็น `AEGIRON_*`, เปลี่ยน daemon `co.redesk.*` เป็น `co.aegiron.*` และบัญชีบริการ `_redesk` เป็น `_aegiron` ฐานข้อมูลเดิมใช้ต่อตามเดิม **แนะนำให้สำรองข้อมูลก่อน**
- **Docker:** เซิร์ฟเวอร์ยังอ่าน `REDESK_*` ได้ แต่ `docker-compose.yml` ใหม่ใช้ฐานข้อมูลชื่อ `aegiron` ถ้าใช้ volume เดิมให้ตั้ง `AEGIRON_DATABASE_URL` ชี้ไปที่ฐานข้อมูล `redesk`
- **แอปและ agent 0.4.x** ต่อกับเซิร์ฟเวอร์ใหม่ได้ เพราะโปรโตคอลไม่เปลี่ยน
- **ติดตั้งแอปใหม่:** ตัวติดตั้ง Windows จะถอด ReDesk เดิมออกและย้ายการตั้งค่า (ID, กุญแจ, บัญชี, ประวัติ) มาให้ บน macOS ลาก Aegiron.app ไปที่ Applications แล้วลบ ReDesk.app แอปจะย้ายการตั้งค่าและการเปิดอัตโนมัติให้เองตอนเปิดครั้งแรก ลิงก์ `redesk://` เดิมยังเปิดได้
- **macOS ต้องอนุญาตสิทธิ์ใหม่** (Screen Recording / Accessibility) เพราะชื่อแอปเปลี่ยน
- **ผู้ใช้เว็บคอนโซลต้องเข้าสู่ระบบใหม่หนึ่งครั้ง**

## เอกสารเพิ่มเติม

- [docs/PROTOCOL.md](docs/PROTOCOL.md) — โปรโตคอล, การเข้ารหัส, codec วิดีโอ, SSO, การอัปเดต
- [docs/INTEGRATIONS.md](docs/INTEGRATIONS.md) — AnyDesk, TeamViewer, RustDesk, RDP / VNC / SSH gateway
- [deploy/macos/README.md](deploy/macos/README.md) — ติดตั้งและดูแลเซิร์ฟเวอร์บน Mac, ติดตั้งแอปบน Mac, MDM
- [deploy/windows/README.md](deploy/windows/README.md) — ตัวติดตั้ง Windows และการติดตั้งแบบเงียบ
- [การวิเคราะห์ RustDesk.app (macOS).md](การวิเคราะห์%20RustDesk.app%20(macOS).md) — ผลวิเคราะห์ที่เป็นที่มาของการออกแบบ
