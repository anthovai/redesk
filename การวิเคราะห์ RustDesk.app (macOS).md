# การวิเคราะห์ RustDesk.app (macOS)

Oct 27, 2026 · @AI-Dev

## สรุป

RustDesk 1.5.0 บนเครื่องนี้เป็นแอป remote desktop ที่มี UI เขียนด้วย Flutter และลอจิกทั้งหมดอยู่ใน Rust core ขนาด 38 MB ตัวแอปเซ็นถูกต้องและผ่าน notarization แต่เป็นบิลด์ x86\_64 ที่รันผ่าน Rosetta และตอนนี้เชื่อมกับเซิร์ฟเวอร์สาธารณะ `rs-ny.rustdesk.com`

- **สถาปัตยกรรม:** Flutter (Dart AOT) ↔ `flutter_rust_bridge` 1.80.1 ↔ `liblibrustdesk.dylib`; มีไฟล์ `service` แยกสำหรับรันเป็น daemon
- **เครือข่าย:** ลงทะเบียนที่ hbbs (21116) → พยายาม P2P → ถ้าไม่ได้จึงใช้ relay hbbr (21117); เข้ารหัสด้วย libsodium/Curve25519/Ed25519
- **สิทธิ์:** ต้องใช้ Screen Recording, Accessibility, Input Monitoring และ Microphone; ไม่อยู่ใน sandbox
- **สถานะตอนตรวจ:** ยังไม่ได้ติดตั้ง LaunchDaemon/LaunchAgent และไม่มี process กำลังทำงาน
- **สิ่งที่ควรทำก่อนใช้ในองค์กร:** ตั้งเซิร์ฟเวอร์เอง, คุมการติดตั้ง service, ตรวจ fallback "accept invalid cert" และเปลี่ยนเป็นบิลด์ arm64

## ข้อมูลแอปและลายเซ็น

RustDesk 1.5.0 (build 68) ในเครื่องนี้เป็นไบนารี **x86\_64 อย่างเดียว** จึงรันบน Mac ชิป Apple Silicon (arm64) ผ่าน Rosetta 2 แอปเซ็นด้วย Developer ID และผ่าน notarization ของ Apple

| รายการ | ค่าที่ตรวจพบ |
| --- | --- |
| Bundle ID | `com.carriez.rustdesk` |
| เวอร์ชัน | 1.5.0 (CFBundleVersion 68) |
| ผู้เผยแพร่ (copyright) | Purslane Tech Pte. Ltd. |
| สถาปัตยกรรม CPU | x86\_64 (thin) ทุกไฟล์หลัก; เครื่องนี้เป็น arm64 |
| macOS ขั้นต่ำ | 10.14 (Mojave) |
| Build | Xcode 16.4 (16F6), macOS SDK 15.5 |
| ผู้เซ็นโค้ด | Developer ID Application: zhou huabing (Team ID `HZF9JMC8YN`) |
| Hardened Runtime | เปิด (flags `0x10000(runtime)`) ทั้ง `RustDesk` และ `service` |
| Notarization | `spctl`: accepted, source = Notarized Developer ID; ไม่มี ticket stapled (Gatekeeper ต้องเช็กออนไลน์) |
| `codesign --verify --deep --strict` | ผ่าน |
| URL scheme | `rustdesk://` (ใช้เปิดการเชื่อมต่อจากลิงก์) |
| `LSUIElement` | 1 — ไม่แสดงไอคอนใน Dock ตามค่าเริ่มต้น ทำงานแบบเมนูบาร์ |

**Entitlements ของแอปหลัก**

| Entitlement | ค่า | ความหมาย |
| --- | --- | --- |
| `com.apple.security.app-sandbox` | false | ไม่อยู่ใน App Sandbox เข้าถึงไฟล์และระบบได้เท่าผู้ใช้ |
| `com.apple.security.cs.allow-jit` | true | อนุญาต JIT ภายใต้ Hardened Runtime |
| `com.apple.security.device.audio-input` | true | ใช้ไมโครโฟนได้ (คู่กับ `NSMicrophoneUsageDescription`) |
| `com.apple.security.network.client` | true | เชื่อมต่อเครือข่ายขาออก |

ไฟล์ `service` ไม่มี entitlements เพิ่มเติม สิทธิ์ Screen Recording และ Accessibility ไม่ได้มาจาก entitlements แต่ผู้ใช้ต้องอนุญาตเองใน System Settings (ดูหัวข้อสิทธิ์ macOS)

## โครงสร้าง bundle

แอปมีขนาดรวม 89 MB โดย 78 MB อยู่ใน `Frameworks/` ลอจิกหลักทั้งหมดอยู่ในไฟล์ Rust `liblibrustdesk.dylib` (38 MB) ส่วนไฟล์รัน `RustDesk` มีขนาดเพียง 121 KB เพราะทำหน้าที่เป็น launcher ของ Flutter เท่านั้น

| ไฟล์ | ขนาด | หน้าที่ |
| --- | --- | --- |
| `Frameworks/liblibrustdesk.dylib` | 38.2 MB | Rust core: เครือข่าย, codec, จับภาพหน้าจอ, input, clipboard, การเข้ารหัส |
| `Frameworks/FlutterMacOS.framework` | 13.8 MB | Flutter engine |
| `Frameworks/App.framework` | 13.6 MB | โค้ด UI ภาษา Dart ที่คอมไพล์แบบ AOT (แพ็กเกจ `flutter_hbb`) |
| `MacOS/service` | 12.0 MB | ไฟล์รัน Rust แยกสำหรับฝั่งถูกควบคุม (มี `rendezvous_mediator`, `server/connection`, `audio_service`) ใช้เมื่อติดตั้งเป็น service |
| `MacOS/RustDesk` | 121 KB | ตัวเปิด Flutter ที่ weak-link ไปยัง `liblibrustdesk.dylib` |
| `libswift*.dylib` (19 ไฟล์) | — | Swift runtime ที่ฝังมาเพื่อรองรับ macOS 10.14 |

**Flutter plugin ที่ฝังมา (16 ตัว + FMDB):** `desktop_multi_window` (เปิดหน้าต่างแยกต่อการเชื่อมต่อ), `texture_rgba_renderer` (วาดภาพหน้าจอปลายทาง), `flutter_custom_cursor`, `window_manager`, `window_size`, `screen_retriever`, `desktop_drop`, `file_selector_macos`, `uni_links_desktop` (รับ `rustdesk://`), `url_launcher_macos`, `sqflite` + `FMDB`, `video_player_avfoundation`, `wakelock_plus`, `device_info_plus`, `package_info_plus`, `path_provider_foundation`

**ไฟล์ตั้งค่าของผู้ใช้** อยู่ที่ `~/Library/Preferences/com.carriez.RustDesk/` (สิทธิ์ `rw-------`)

| ไฟล์ | เก็บอะไร (ดูเฉพาะชื่อ key ไม่ได้เปิดดูค่าลับ) |
| --- | --- |
| `RustDesk.toml` | `enc_id`, `password`, `salt`, `key_pair`, `key_confirmed` — ID เครื่อง, รหัสผ่านถาวร, คู่กุญแจ Ed25519 |
| `RustDesk2.toml` | `rendezvous_server`, `nat_type`, `serial`, `unlock_pin`, `trusted_devices`, `[options]` |
| `RustDesk_local.toml` | ประวัติ ID ที่เชื่อมต่อ, รายการโปรด, ขนาด/ตำแหน่งหน้าต่าง |
| `RustDesk_hwcodec.toml` | ผลทดสอบ hardware codec |
| `peers/` | ข้อมูลเครื่องปลายทางที่เคยเชื่อมต่อ (ตอนตรวจไม่มีไฟล์) |

## สถาปัตยกรรมภายใน

&#91;embedded content: สถาปัตยกรรม RustDesk.app · 3 ชั้น + เซิร์ฟเวอร์ภายนอก\]

Flutter ทำหน้าที่แค่ UI แล้วเรียก Rust core ผ่าน `flutter_rust_bridge` (FFI) งานจับภาพจอ ส่ง input เข้ารหัส และเครือข่ายทั้งหมดอยู่ใน `liblibrustdesk.dylib` ไฟล์ `service` เป็นโค้ด Rust ชุดเดียวกันที่แยกมารันเป็น daemon

**โมดูล Rust ที่พบ (จาก path ตอนคอมไพล์):** `libs/scrap` (จับภาพ + codec: vpx, aom, hwcodec), `libs/enigo` (จำลอง input), `libs/clipboard` (clipboard + โอนไฟล์ผ่าน pasteboard), `libs/hbb_common` (config, socket, TLS, WebSocket, WebRTC, รหัสผ่าน), `src/client.rs`, `src/server/*`, `src/rendezvous_mediator.rs`, `src/hbbs_http/*` (API, OIDC, downloader), `src/updater.rs`, `src/whiteboard/macos.rs`, `src/lan.rs`, `src/auth_2fa.rs`

**Crate สำคัญ:** `tokio` 1.44.2, `rustls` 0.23.28, `ring` 0.17.14, `sodiumoxide` 0.2.7, `protobuf` 3.7.2, `reqwest` 0.12.24, `webrtc` (git fork), `kcp-sys`, `totp-rs` 5.5.1 (2FA), `flutter_rust_bridge` 1.80.1; คอมไพล์ด้วย rustc commit `eeb90cda1969` (อนุมานจาก path)

## เครือข่าย

เครื่องนี้ใช้ **เซิร์ฟเวอร์สาธารณะของ RustDesk** (`rs-ny.rustdesk.com:21116`) ไม่ได้ตั้งเซิร์ฟเวอร์เอง แอปลงทะเบียน ID กับ rendezvous server (hbbs) ก่อน จากนั้นพยายามเชื่อมต่อตรงแบบ P2P (UDP hole punching) ถ้าไม่สำเร็จจะวิ่งผ่าน relay server (hbbr)

| ส่วน | พอร์ต / ปลายทาง | หลักฐานในไบนารี |
| --- | --- | --- |
| Rendezvous (hbbs) | TCP + UDP 21116 | สตริง `TCP 21116`, `UDP 21116`, ค่า default `rs-ny.rustdesk.com` |
| NAT type test | 21115 | พบในสตริง 3 จุด; config บันทึก `nat_type = 1` |
| Relay (hbbr) | TCP 21117 | ตัวอย่าง `...:21117?key=...` ในไบนารี |
| API server (Pro) | HTTP(S) 21114 | `allow-https-21114`, `/api/login`, `/api/ab`, `/api/audit` |
| เชื่อมต่อตรง (ปิดตามค่าเริ่มต้น) | `direct-access-port` | ออปชัน `direct-server` |
| LAN discovery / Wake-on-LAN | UDP บน LAN | `src/lan.rs`, crate `wol-rs` |
| ทางเลือกขนส่ง | WebSocket, WebRTC, KCP, SOCKS5 proxy | `websocket.rs`, `webrtc.rs`, `kcp-sys`, `tokio-socks` |
| แจ้งเตือน 2FA | `api.telegram.org` | ส่งรหัส 2FA / แจ้งการเชื่อมต่อผ่าน Telegram bot เมื่อตั้งค่าไว้ |
| ตรวจอัปเดต | GitHub releases | `check_software_update`, `src/updater.rs`, ลิงก์ `github.com/rustdesk/rustdesk/releases/tag/` |

**การเข้ารหัส**

- ใช้ `sodiumoxide` (libsodium), `curve25519-dalek` และ `ed25519` สำหรับ key exchange และลายเซ็น และใช้ `rustls` / `ring` กับ HTTPS
- ฝัง public key ของเซิร์ฟเวอร์สาธารณะไว้ในไบนารี (`OeVuKk5nlHiXp+APNn0Y3pC1Iwpwn44JGqrQCsWqmBw=`) ถ้าเซิร์ฟเวอร์ตอบด้วย key ที่ไม่ตรง แอปจะขึ้น `Handshake failed: invalid public key from rendezvous server`
- สตริง `secure_connection ok` และ `punch` บ่งว่ามีการเข้ารหัสหลัง hole punching ทั้งทาง P2P และ relay

**เส้นทาง API ฝั่ง Flutter** (ใช้เมื่อเชื่อม RustDesk Server Pro): `/api/login`, `/api/login-options`, `/api/logout`, `/api/currentUser`, `/api/users`, `/api/peers`, `/api/device-group/accessible`, `/api/audit`, `/api/ab/*` (สมุดที่อยู่และแท็ก) ฝั่ง Rust มี `/api/oidc/auth`, `/api/oidc/auth-query` (ล็อกอิน OIDC) และ `/api/switch-grant`

## สิทธิ์ macOS และกลไกระดับระบบ

การควบคุมจากระยะไกลต้องใช้สิทธิ์ TCC 3 ตัวที่ผู้ใช้ต้องกดอนุญาตเองใน System Settings → Privacy & Security แอปตรวจและขอสิทธิ์ผ่าน API ต่อไปนี้ (พบใน import ของ `liblibrustdesk.dylib`)

| สิทธิ์ | ใช้ทำอะไร | API ที่เรียก |
| --- | --- | --- |
| Screen Recording | จับภาพหน้าจอส่งให้ฝั่งควบคุม | `CGPreflightScreenCaptureAccess`, `CGRequestScreenCaptureAccess`, `CGDisplayStreamCreateWithDispatchQueue` |
| Accessibility | ส่งเมาส์/คีย์บอร์ดจำลอง | `AXIsProcessTrustedWithOptions`, `CGEventPost` |
| Input Monitoring | ดักจับคีย์บอร์ดฝั่งผู้ควบคุม | `IOHIDCheckAccess`, `IOHIDRequestAccess`, `CGEventTapCreate` |
| Microphone | ส่งเสียง | entitlement `audio-input` + `NSMicrophoneUsageDescription` |
| กันเครื่องหลับ | ระหว่างเซสชัน | `IOPMAssertionCreateWithName` |

macOS ผูกสิทธิ์เหล่านี้กับลายเซ็นโค้ด (Team ID `HZF9JMC8YN`) ถ้าแทนที่แอปด้วยบิลด์ที่เซ็นต่างคน ต้องขอสิทธิ์ใหม่

**การติดตั้งเป็น service (เมนู Install)** — Rust core ฝัง AppleScript ที่รันผ่าน `osascript` พร้อมขอรหัสผ่านผู้ดูแล (ข้อความ `Installation and administrative privileges required!`) แล้วทำตามลำดับ

1. เขียน `/Library/LaunchDaemons/com.carriez.RustDesk_service.plist` (เจ้าของ `root:wheel`) — รัน `service` ในฐานะ root
2. เขียน `/Library/LaunchAgents/com.carriez.RustDesk_server.plist` — รันส่วน server ใน session ของผู้ใช้ทุกคนที่ล็อกอิน (รวมหน้าจอล็อกอิน)
3. คัดลอกไฟล์ตั้งค่าของผู้ใช้ไปที่ `/var/root/Library/Preferences/com.carriez.RustDesk/`
4. เรียก `launchctl bootstrap system ...` เพื่อเริ่ม daemon

ตอนตรวจ **ยังไม่ได้ติดตั้ง service**: ไม่มีไฟล์ plist ทั้งสอง และไม่มี process RustDesk กำลังทำงาน การถอดการติดตั้งใช้สคริปต์ `uninstall.scpt` ที่ลบไฟล์เดียวกัน

## การประเมินความปลอดภัย

ตัวแอปผ่านเกณฑ์พื้นฐานของ Apple (เซ็นถูกต้อง, notarized, เปิด Hardened Runtime) แต่มีจุดที่องค์กรควรตัดสินใจก่อนใช้งานจริง โดยเฉพาะการพึ่งเซิร์ฟเวอร์สาธารณะและการติดตั้ง daemon ระดับ root

| ประเด็น | หลักฐาน | ประเภท | ความเสี่ยง |
| --- | --- | --- | --- |
| ใช้เซิร์ฟเวอร์สาธารณะ | `rendezvous_server = rs-ny.rustdesk.com:21116` ใน config | สังเกตตรง | สูงสำหรับองค์กร: metadata การเชื่อมต่อและ relay ผ่านโครงสร้างภายนอก |
| Daemon ระดับ root | AppleScript เขียน LaunchDaemon + LaunchAgent และคัดลอก config ไป `/var/root` | สังเกตจากสคริปต์ในไบนารี | สูง: ถ้ารหัสผ่านถาวรหลุด เครื่องถูกควบคุมได้แม้อยู่หน้าจอล็อกอิน |
| HTTP ยอมรับ cert ไม่ถูกต้อง | สตริง `try again, danger accept invalid cert` ใน `hbbs_http/http_client.rs` | ยืนยันแล้ว (ดูหัวข้อ "ผลการ decompile" ด้านล่าง): fallback รับ cert ปลอมอัตโนมัติเมื่อ probe ล้ม | ปานกลาง: เสี่ยง MITM กับ API/การดาวน์โหลดอัปเดต ถ้า fallback ทำงานจริง |
| ไม่อยู่ใน App Sandbox | `app-sandbox = false` | สังเกตตรง | ปานกลาง: เป็นเรื่องปกติของแอป remote desktop แต่เพิ่มผลกระทบถ้าแอปถูกโจมตี |
| ผู้เซ็นเป็นบุคคล | Developer ID ชื่อ "zhou huabing" ไม่ตรงกับบริษัทใน copyright | สังเกตตรง | ต่ำ: ให้ผูก allowlist กับ Team ID `HZF9JMC8YN` แทนชื่อ |
| x86\_64 อย่างเดียว | `lipo -archs` ทุกไฟล์ = x86\_64; เครื่อง = arm64 | สังเกตตรง | ต่ำด้านความปลอดภัย แต่กินทรัพยากรเพิ่มเพราะรันบน Rosetta 2 และควรเปลี่ยนไปใช้บิลด์ arm64/universal |
| ส่งรหัส 2FA ผ่าน Telegram | `api.telegram.org/bot.../sendMessage` | สังเกตจากสตริง (ทำงานเมื่อผู้ใช้ตั้ง bot เอง) | ต่ำ: ข้อมูลการเชื่อมต่อ (รวม IP ต้นทาง) ออกไปบริการภายนอก |
| ความลับในไฟล์ config | `password`, `salt`, `key_pair` ใน `RustDesk.toml` สิทธิ์ `600` | สังเกตตรง | ต่ำ: ควรรวมใน backup policy และห้ามคัดลอกไฟล์นี้ไปเครื่องอื่น |

**ข้อแนะนำสำหรับใช้ในองค์กร**

- [ ] ตั้ง RustDesk Server เอง (hbbs/hbbr) แล้วกำหนด `custom-rendezvous-server`, `relay-server` และ public key ของเซิร์ฟเวอร์บริษัท
- [ ] ตั้ง `verification-method` ให้ใช้รหัสผ่านชั่วคราว หรือเปิด `approve-mode` ให้ฝั่งเครื่องต้องกดยอมรับทุกครั้ง
- [ ] เปิด 2FA แต่อย่าส่งรหัสผ่าน Telegram ถ้านโยบายห้ามส่งข้อมูลออกภายนอก
- [ ] ติดตั้ง service เฉพาะเครื่องที่ต้องเข้าถึงแบบ unattended และตรวจหา `com.carriez.RustDesk_service.plist` ผ่าน MDM
- [ ] ใน MDM กำหนด PPPC profile (Screen Recording/Accessibility) ผูกกับ Team ID `HZF9JMC8YN` + bundle ID `com.carriez.rustdesk`
- [ ] ปิดอัปเดตอัตโนมัติแล้วแจกจ่ายเวอร์ชันที่ตรวจแล้วผ่าน MDM แทน และเลือกบิลด์ arm64 สำหรับ Mac ชิป Apple

### ผลการ decompile: fallback รับใบรับรองที่ไม่ถูกต้อง (accept invalid cert)

ไล่เส้นทางโค้ดของข้อความ "danger accept invalid cert" แล้วยืนยันว่า **client HTTP ของแอปจะ fallback ไปยอมรับ cert ที่ไม่ถูกต้องโดยอัตโนมัติ** เมื่อการเชื่อมต่อแบบปกติล้มเหลว ไม่มี config หรือ flag มาคุมก่อน

**วิธีตรวจ:** Hopper เปิดไม่ได้เพราะ macOS ยังไม่ได้อนุญาต Automation ให้ตัวที่รัน REA (System Settings → Privacy & Security → Automation) จึงใช้ `objdump` ถอดรหัสแทน แล้วเทียบกับซอร์สโค้ด AGPL

**หลักฐานในไบนารี:**

- ชิ้นส่วนข้อความ log อยู่ที่ `__const` offset `0x1d30f6f`: `HTTP request failed:` → `try again with native-tls` → `try again with native-tls (x` → `try again, danger accept invalid cert` → `try again, danger accept invalid cert (x`
- ตารางตัวชี้ (ptr, len) อยู่ที่ `__DATA` `0x21c3e60` ถูกอ้างโดยฟังก์ชันใน `__text` ที่ `0x536400` ซึ่งมีโครงเป็น dispatch (`cmp $0x1`/`cmp $0x2`) เลือกข้อความ log ตามสถานะ retry — สอดคล้องกับบันได retry ของ source

**กลไกจริง (จาก `src/hbbs_http/http_client.rs`):** client ยิง `HEAD` probe ก่อน ถ้าพลาดจะไล่ retry เป็นขั้นๆ

1. Rustls — ตรวจ cert ตามปกติ → ถ้าพลาด
2. Rustls + `danger_accept_invalid_certs(true)` — **เลิกตรวจ cert** → ถ้าพลาด
3. native-tls → ถ้าพลาด
4. native-tls + รับ cert ที่ไม่ถูกต้อง → จากนั้น log error และคืน client; ผลที่สำเร็จจะถูก cache ไว้

**มีตัว strict:** `create_http_client_with_url_strict` ส่ง `Some(false)` จึงไม่รับ cert ปลอมเด็ดขาด (fallback ได้แค่ Rustls→native-tls) — แต่ยังไม่ได้ไล่ว่า call site ไหนใช้ strict ไหนใช้แบบปกติ

**ขอบเขตผลกระทบ:** เป็น HTTP ฝั่ง `hbbs_http` (API เว็บคอนโซล / address book / ตรวจอัปเดต) **ไม่ใช่ช่องรีโมตหลัก** ซึ่งเข้ารหัสด้วย libsodium/Curve25519 แยกจาก TLS cert validation

**นัย:** ถ้า attacker ทำให้การเชื่อม HTTPS ปกติล้มเหลว (block/รบกวน) แล้วเสนอ cert ปลอม client ตัวที่ไม่ strict อาจยอมรับและทำงานต่อ → เสี่ยง MITM ต่อ traffic API เหล่านั้น (เช่น เนื้อหา address book, การยืนยันตัวตน) — ควรบังคับ HTTPS ที่ถูกต้องในระดับเครือข่ายสำหรับงานองค์กรที่อ่อนไหวต่อการดัก traffic

**ไล่ต่อ: call site ไหนใช้ strict ไหนใช้แบบปกติ** (เทียบจาก source AGPL; ไบนารี strip ซิมโบลจึงไล่ในไบนารีโดยตรงไม่ได้)

| Call site | ชนิด client | ใช้ทำอะไร | รับ cert ปลอมได้ไหม |
| --- | --- | --- | --- |
| `updater/artifact.rs:125` | strict | ดึง metadata ของไฟล์อัปเดต | ❌ ไม่ (บังคับ HTTPS) |
| `updater/download.rs:35` | strict | ดาวน์โหลดไฟล์อัปเดต | ❌ ไม่ |
| `hbbs_http/downloader.rs:200` | strict (async) | ตัวดาวน์โหลดทั่วไป | ❌ ไม่ |
| `hbbs_http/account.rs:156` | ปกติ | warm-up ตรวจ TLS ของ API server (`/api/login-options`) | ⚠️ ได้ — และ**เซ็ตค่า cache** ให้ request ถัดไป |
| `hbbs_http/record_upload.rs:35` | ปกติ | อัปโหลด session recording | ⚠️ ได้ |
| `common.rs` → `post_request` / `get_request` | ปกติ (อ่านค่าจาก cache) | **ทุก API call จริง**: `/api/login`, `/api/oidc/auth`, `/api/audit`, address book `/api/ab/*` | ⚠️ ได้ ถ้า cache เป็น true |

**จุดที่เชื่อมกัน (สำคัญ):** `account.rs` เรียก warm-up แบบปกติก่อนเพื่อเดาชนิด TLS ของเซิร์ฟเวอร์ ถ้า probe ต้องยอมรับ cert ปลอมจึงจะสำเร็จ ค่า `danger_accept_invalid_cert = true` จะถูก `upsert_tls_cache` เก็บไว้ จากนั้น `post_request`/`get_request` ใน `common.rs` จะอ่านค่านี้ด้วย `get_cached_tls_accept_invalid_cert` แล้วส่งต่อเข้า `create_http_client_async(tls_type, danger)` — แปลว่า **traffic API จริงทั้งหมดไปยังเซิร์ฟเวอร์นั้นสืบทอดพฤติกรรมยอมรับ cert ปลอมต่อไปด้วย**

**สรุปความเสี่ยง:** เส้นทางอัปเดต (ตรวจ+ดาวน์โหลดไฟล์) ปลอดภัยเพราะบังคับ HTTPS เต็มรูปแบบ strict — แต่ **เส้นทาง API / login / address book / audit คือส่วนที่เสี่ยง** เพราะยอม fallback รับ cert ปลอมอัตโนมัติ (ช่องรีโมตหลักยังปลอดภัยด้วย libsodium แยกต่างหาก)

## ข้อจำกัดและหลักฐาน

การวิเคราะห์นี้เป็นแบบ **static** ทั้งหมด ไม่ได้รันแอปหรือดักจับ traffic จริง จึงยืนยันได้ว่าโค้ด "มี" ส่วนไหน แต่ยังยืนยันไม่ได้ว่า "ทำงานจริงเมื่อไร"

- **เครื่องมือ:** `codesign`, `spctl`, `lipo`, `otool -L`, `nm -u`, `strings`, `plutil` และ REA `inspect_rust_binary` (Evidence `ev_3cf4366c…f05ac`)
- **หา dependency จาก path ในสตริง:** ไบนารีไม่มี section `.dep-v0` ของ cargo-auditable รายชื่อ crate จึงมาจาก path ตอนคอมไพล์ (เช่น `tokio-1.44.2`, `rustls-0.23.28`) ซึ่งอาจไม่ครบทุกตัว
- **เวอร์ชัน rustc:** อนุมานจาก path `/rustc/eeb90cda1969…/` ไม่ได้พิสูจน์ว่าใช้คอมไพเลอร์ตัวไหน
- **decompile บางส่วน:** Hopper เปิดไม่ได้ (ขาดสิทธิ์ Automation) ส่วน fallback "accept invalid cert" ไล่ด้วย objdump + source แล้ว (ดูหัวข้อการประเมิน); ส่วนการเก็บ/เข้ารหัสรหัสผ่านยังไม่ได้ไล่ในระดับโค้ด
- **ไม่ได้เปิดดูค่าลับ:** อ่านเฉพาะชื่อ key ในไฟล์ config ยกเว้น `rendezvous_server` และ `nat_type`
- **ไม่ได้อ่าน TCC database:** จึงไม่ทราบว่าตอนนี้ให้สิทธิ์ Screen Recording/Accessibility แล้วหรือยัง
- **คำถามค้าง:** พอร์ต WebSocket 21118/21119 ไม่พบเป็นสตริงตรงๆ อาจคำนวณจากพอร์ตหลัก ต้องตรวจตอนรันจริง

ซอร์สโค้ดต้นฉบับ (AGPL-3.0): [github.com/rustdesk/rustdesk](https://github.com/rustdesk/rustdesk) — ใช้เทียบกับผลในเอกสารนี้ได้โดยตรง
