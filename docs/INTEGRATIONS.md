# การเชื่อมต่อผ่านผู้ให้บริการอื่น (AnyDesk, TeamViewer, RustDesk, RDP, VNC, SSH)

Aegiron รวมเครื่องที่ใช้โปรแกรมรีโมตต่างยี่ห้อไว้ในคอนโซลเดียว (หน้า **การเชื่อมต่อ**) ด้วยสิทธิ์ตามกลุ่ม vault ที่เข้ารหัส และ audit log ชุดเดียวกับ Aegiron มีกลไก 3 แบบตามความสามารถของแต่ละผู้ให้บริการ

| ผู้ให้บริการ | เปิดเครื่อง | ดึงข้อมูลจาก API | ข้อจำกัด |
|---|---|---|---|
| AnyDesk | เปิดแอป AnyDesk ด้วย `anydesk:<ID>` | ไม่มี (ใช้ CSV import) | REST API สาธารณะไม่เปิดข้อมูลอุปกรณ์หรือเซสชัน และรายละเอียด auth อยู่ในพอร์ทัลลูกค้าเท่านั้น |
| TeamViewer | เปิดแอปด้วย `teamviewer10://control?device=<ID>` | ✅ อุปกรณ์ + ประวัติการเชื่อมต่อ | ต้องใช้ script token ของ TeamViewer ที่มีสิทธิ์ API |
| RustDesk | เปิดแอปด้วย `rustdesk://connection/new/<ID>` | ไม่มี | — |
| RDP / VNC / SSH | **เปิดในเบราว์เซอร์ผ่าน gateway** หรือเปิดโปรแกรมในเครื่อง (`.rdp`, `vnc://`, `ssh://`) | — | gateway ต้องเข้าถึงเครื่องปลายทางได้ทางเครือข่าย |

Aegiron ไม่ได้ควบคุมเครื่องผ่านโปรโตคอลของ AnyDesk หรือ TeamViewer เอง เพราะเป็นโปรโตคอลปิดและการทำ reverse engineering ขัดกับเงื่อนไขการใช้งานของทั้งสองเจ้า Aegiron ทำหน้าที่เป็นศูนย์กลางที่เปิดโปรแกรมของเจ้านั้นให้ เก็บรหัสผ่าน และบันทึกว่าใครเปิดเครื่องไหนเมื่อไร

## 1. ตัวเปิดโปรแกรมและ vault

- **รหัสผ่านไม่ถูกใส่ในลิงก์** (เพราะจะไปค้างใน browser history และรายการ process) เมื่อกด "เปิดใน AnyDesk/TeamViewer/RustDesk" เซิร์ฟเวอร์จะคืนลิงก์ที่มีแค่ ID ส่วนคอนโซลคัดลอกรหัสผ่านไปที่คลิปบอร์ดและล้างให้ภายใน 30 วินาที ถ้าเบราว์เซอร์ไม่อนุญาตจะแสดงกล่องให้กดคัดลอกเอง
- **ID ของผู้ให้บริการต้องเป็นตัวเลขเท่านั้น** (AnyDesk รับ alias แบบ `name@ad` ได้ด้วย) URL handler ของโปรแกรมเหล่านี้เคยมีช่องโหว่ argument injection เช่น TeamViewer [CVE-2020-13699](https://nvd.nist.gov/vuln/detail/CVE-2020-13699) ดังนั้นห้ามมีอักขระอื่นปนมา ค่าที่ซิงก์มาจาก API ก็ผ่านการตรวจแบบเดียวกัน
- ชื่อผู้ใช้ โดเมน และโฮสต์ห้ามมีอักขระควบคุม เพื่อกันการแทรกบรรทัดในไฟล์ `.rdp` เช่น `alternate shell:s:cmd.exe` และกันโฮสต์ที่ขึ้นต้นด้วย `-` (`ssh -oProxyCommand=…`)
- **Vault:** รหัสผ่านและ API token เข้ารหัสด้วย AES-256-GCM ใช้กุญแจจาก `AEGIRON_VAULT_KEY` (base64 ขนาด 32 ไบต์) หรือไฟล์ `data/vault.key` ที่สร้างให้อัตโนมัติ **เก็บกุญแจแยกจาก backup ของฐานข้อมูล** เพราะถ้าได้ dump ฐานข้อมูลไปอย่างเดียวจะยังอ่านรหัสผ่านไม่ได้ กุญแจเดียวกันนี้ใช้เข้ารหัส client secret ของ SSO รหัสผ่านบัญชีบริการ LDAP และรหัสผ่าน SMTP ด้วย
- **สิทธิ์:** ผู้ดูแลระบบเห็นทุกรายการ ผู้ใช้ทั่วไปเห็นเฉพาะรายการที่ตัวเองสร้าง และรายการในกลุ่มที่ตัวเองเป็นสมาชิก เฉพาะผู้ดูแลระบบที่แชร์รายการเข้ากลุ่มได้
- **Audit:** `endpoint.created/updated/deleted/imported`, `endpoint.launch` (บันทึกด้วยว่ามีการปล่อยรหัสผ่านหรือไม่), `gateway.start/end/failed`, `integration.*`

## 2. Gateway สำหรับ RDP / VNC / SSH ในเบราว์เซอร์

```
Browser (guacamole-common-js) ──WSS /ws/gateway──▶ aegiron-server ──TCP──▶ guacd ──RDP/VNC/SSH──▶ เครื่องปลายทาง
                                               ตรวจสิทธิ์, ถอดรหัสรหัสผ่านจาก vault,
                                               handshake กับ guacd, บันทึก audit
```

- เปิดใช้โดยตั้ง `AEGIRON_GUACD_ADDR=host:4822` (docker-compose ตั้งไว้ให้แล้ว)
- เบราว์เซอร์ไม่ได้รับรหัสผ่าน: เซิร์ฟเวอร์ใส่รหัสใน `connect` ของ guacd เอง
- เซิร์ฟเวอร์ส่งต่อเฉพาะ instruction ที่สมบูรณ์ ตอบ ping ภายในของ tunnel เอง และนับ byte ของแต่ละเซสชัน (ดูได้ในหน้าเซสชัน)
- **บันทึกเซสชัน:** ตั้ง `AEGIRON_GATEWAY_RECORDING_PATH` (เป็น path **ภายใน container ของ guacd**) แล้ว guacd จะบันทึกทุกเซสชันเป็นไฟล์ Guacamole recording ชื่อ `<sid>` (sid อยู่ใน audit `gateway.start`) สำหรับ SSH จะได้ typescript ด้วยที่ `<sid>.typescript` และ `<sid>.typescript.timing` (เล่นซ้ำด้วย `scriptreplay`) ไฟล์ recording เปิดดูย้อนหลังได้ด้วย `guacamole-common-js` (`Guacamole.SessionRecording`) ซึ่งยังไม่มีหน้าเล่นซ้ำในคอนโซล
  - guacd ทำงานเป็น uid 1000 แต่ Docker volume ที่สร้างใหม่เป็นของ root ถ้าไม่ `chown` ให้ก่อน guacd จะเขียนไม่ได้ (log: `Creation of recording failed: Permission denied`) และ**ไม่บันทึกอะไรเลยโดยไม่แจ้งผู้ใช้** docker-compose.yml (service `guacd-recordings`), `deploy/macos/install.sh` และ `make dev-guacd` จัดการให้แล้ว ถ้าตั้ง guacd เอง ให้รัน `docker run --rm --user root --entrypoint chown -v <volume>:/recordings guacamole/guacd:1.5.5 guacd:guacd /recordings`
- **Audit ของ gateway:** `gateway.end` มี `reason` (`browser_closed` = ผู้ใช้ปิด, `target_closed` = เครื่องปลายทาง/guacd ตัด เช่นรหัสผิด) และ `error` เมื่อ guacd แจ้งสาเหตุ (เช่น `SSL/TLS connection failed (untrusted/self-signed certificate?) (status 519)`) guacd 1.5.5 บางครั้งทิ้งข้อความ error ถ้าการเชื่อมต่อล้มเหลวเร็วมาก คอนโซลจึงแสดง "เชื่อมต่อไม่สำเร็จ" ทุกครั้งที่ปิดก่อนเชื่อมต่อได้
- **รหัสผ่านกับ "เปิดด้วยโปรแกรมในเครื่อง":** เมื่อเปิด gateway ไว้ รหัสผ่าน RDP/VNC/SSH จะถูกคัดลอกไปเบราว์เซอร์ให้เฉพาะผู้ดูแลระบบและผู้สร้างรายการเท่านั้น สมาชิกกลุ่มที่ได้รับแชร์ใช้ผ่าน gateway ได้แต่ไม่เห็นรหัส (ถ้าปิด gateway ทุกคนที่มีสิทธิ์ยังได้รับรหัสเพื่อเปิดโปรแกรมในเครื่อง)
- **SSH ด้วยกุญแจ:** เลือก "ยืนยันตัวตนด้วย: กุญแจ SSH" แล้ววาง private key (OpenSSH หรือ PEM) ที่**ไม่มี passphrase** กุญแจเก็บเข้ารหัสใน vault ใช้ได้เฉพาะผ่าน gateway และไม่ถูกส่งไปเบราว์เซอร์ในทุกกรณี ทดสอบแล้วทั้ง ed25519 และ RSA กับ OpenSSH 9.2
- RDP ใช้ `resize-method=display-update` ทำให้ปรับขนาดหน้าต่างแล้วความละเอียดหน้าจอปลายทางปรับตาม ตั้ง "ไม่ตรวจใบรับรอง" ได้ทีละเครื่อง สำหรับเซิร์ฟเวอร์ภายในที่ใช้ใบรับรอง self-signed
- SSH: guacd **ไม่ได้ตรวจ host key** (log จะเตือน "No known host keys provided") ถ้าต้องการกัน MITM ภายในเครือข่าย ให้วาง guacd ไว้ในเครือข่ายเดียวกับเซิร์ฟเวอร์ปลายทาง

### ทดสอบ gateway กับเซิร์ฟเวอร์จริง (end-to-end)

```bash
deploy/test/gateway-e2e.sh          # up + ทดสอบ + down ในคำสั่งเดียว
deploy/test/gateway-e2e.sh up       # เปิดเครื่องทดสอบ ฐานข้อมูล และเซิร์ฟเวอร์ค้างไว้
deploy/test/gateway-e2e.sh run      # รันเฉพาะการทดสอบ (GWTEST_ONLY=ssh-key-rsa,vnc-password เลือกบางกรณี)
deploy/test/gateway-e2e.sh down     # หยุด container ทั้งหมดและลบฐานข้อมูลทดสอบ
```

ต้องมี Docker (เช่น Colima), Go, bun, PostgreSQL 16 และ Google Chrome สคริปต์แยกจากระบบจริงทั้งหมด: ฐานข้อมูล `aegiron_gw`, เซิร์ฟเวอร์ที่ `127.0.0.1:21300` พร้อม data dir ของตัวเอง (`GWTEST_DIR`, ค่าเริ่มต้น `$TMPDIR/aegiron-gwtest`), guacd ที่ `127.0.0.1:14822` และ container ชื่อ `aegiron-gwtest-*` จาก [deploy/test/gateway/compose.yml](../deploy/test/gateway/compose.yml)

| เครื่องทดสอบ | ซอฟต์แวร์ | ใช้ทดสอบ |
|---|---|---|
| `aegiron-gwtest-rdp` | xrdp (security negotiate) | RDP โหมด any, rdp, nla (คาดว่าล้มเหลว), รหัสผิด |
| `aegiron-gwtest-rdp-tls` | xrdp (TLS อย่างเดียว, self-signed) | โหมด tls, ปฏิเสธใบรับรองเมื่อไม่ติ๊ก "ไม่ตรวจใบรับรอง" |
| `aegiron-gwtest-rdp-nla` | FreeRDP shadow server (`/sec:nla`) | NLA/CredSSP จริง: รหัสถูกเข้าได้ รหัสผิดถูกปฏิเสธ |
| `aegiron-gwtest-vnc` | TigerVNC (VncAuth) | VNC รหัสถูก/ผิด |
| `aegiron-gwtest-ssh` | OpenSSH 9.2 | SSH รหัสผ่าน, กุญแจ ed25519 และ RSA, รหัสผิด |

[web/scripts/e2e-gateway.ts](../web/scripts/e2e-gateway.ts) ใช้ Chrome แบบ headless (playwright-core) ล็อกอินคอนโซล เพิ่มการเชื่อมต่อผ่านฟอร์ม (VNC, SSH key) และ API แล้วตรวจแต่ละกรณี: หน้าจอปลายทางแสดงผลจริง (อ่านพิกเซลจาก canvas), พิมพ์คำสั่งแล้วตรวจผลในเครื่องปลายทาง (RDP/VNC ตรวจไฟล์ที่ xterm เขียน, SSH ตรวจผลลัพธ์ใน typescript), คลิปบอร์ดทั้งสองทาง, ปล่อยเครื่องว่าง 25 วินาทีแล้วยังใช้งานได้, ตัดการเชื่อมต่อ, audit `gateway.start/end`, ไฟล์ recording, สิทธิ์ตามกลุ่ม (เห็น/เข้าได้เฉพาะรายการในกลุ่ม และถูกตัดสิทธิ์เมื่อออกจากกลุ่ม), WebSocket ที่ไม่มี session หรือมาจาก Origin อื่นถูกปฏิเสธ และไม่มีรหัสผ่านหรือกุญแจโผล่ใน WebSocket frame หรือ response ใดที่เบราว์เซอร์ได้รับ ภาพหน้าจอเก็บใน `$GWTEST_DIR/shots`

**สิ่งที่การทดสอบนี้ไม่ได้พิสูจน์เกี่ยวกับ Windows:** xrdp และ FreeRDP shadow ไม่ใช่ Windows ผลนี้ยืนยันว่า Aegiron ส่งพารามิเตอร์ ข้อมูลรับรอง และสตรีมผ่าน guacd ได้ถูกต้อง และ guacd (FreeRDP 2) ต่อ RDP, TLS และ NLA ได้ แต่ยังไม่ได้ทดสอบกับ Windows Server/Windows 10-11 จริง เช่น NLA กับ Kerberos/โดเมน, การ login ด้วยบัญชีโดเมน, RemoteFX/GFX, display-update ของ Windows, คลิปบอร์ดของ rdpclip และ Restricted Admin เครื่อง shadow server ส่งภาพหน้าจอได้น้อยมาก จึงตรวจเฉพาะการ login ด้วย NLA ไม่ได้ตรวจภาพ

ข้อจำกัดที่พบ (ไม่ใช่ bug ของ Aegiron): โหมด `rdp` (Standard RDP Security แบบ RC4) ของ guacd 1.5.5 ใช้กับ xrdp ไม่ได้ (`MAC checksum error for non-FIPS PDU`), xrdp ไม่รองรับ NLA และเมื่อรหัสผิด xrdp จะแสดงหน้า login ของตัวเองแทนการปฏิเสธ, image `guacamole/guacd:1.5.5` มีแค่ amd64 จึงรันแบบ emulation บน Apple Silicon

## 3. TeamViewer API

1. ใน TeamViewer Management Console → Profile → Apps → **Create script token** ให้สิทธิ์อ่านเท่านั้น ได้แก่ Computers & Contacts (ดูรายการ) และ Connection reporting (อ่านรายงาน) ตามคำแนะนำของ TeamViewer ให้ token มีสิทธิ์น้อยที่สุด ([KB](https://www.teamviewer.com/en/global/support/knowledge-base/teamviewer-remote/for-developers/use-the-teamviewer-api/)) และมีรายงานจากผู้ใช้ว่าการอ่านรายงานต้องใช้ *user* token ไม่ใช่ company token
2. Aegiron → ตั้งค่าเซิร์ฟเวอร์ → **TeamViewer API** → ใส่ token แล้วเลือกกลุ่มสำหรับอุปกรณ์ใหม่
3. ระบบจะซิงก์ทุก `AEGIRON_SYNC_MINUTES` นาที (ค่าเริ่มต้น 15) หรือกด "ซิงก์ตอนนี้"
   - `GET /api/v1/devices` → อุปกรณ์จะปรากฏในหน้าการเชื่อมต่อ พร้อมสถานะออนไลน์ ชื่อ/ID จะตามข้อมูลจาก TeamViewer ส่วนกลุ่ม รหัสผ่าน และแท็กที่แก้ใน Aegiron จะคงไว้
   - `GET /api/v1/reports/connections` → ดูได้ที่หน้าเซสชัน แท็บ "จาก TeamViewer API" ระบบดึงต่อจากเวลาที่ดึงไว้ล่าสุด และไล่หน้าด้วย `next_offset`/`offset_id` (หน้าละไม่เกิน 1000 รายการ)

> ทดสอบกับ mock ที่จำลองรูปแบบ response ตามเอกสาร **ยังไม่ได้ทดสอบกับบัญชี TeamViewer จริง** ชื่อ field ของรายงานต่างกันในแต่ละแหล่ง (`devicename` กับ `targetname`) โค้ดจึงอ่านได้ทั้งสองแบบ และเก็บ JSON ดิบไว้ใน `external_sessions.raw`

## 4. นำเข้า CSV (ใช้กับ AnyDesk และย้ายรายชื่อจากระบบอื่น)

```csv
name,provider,address,port,username,password,tags,note
เครื่องบัญชี 1,anydesk,123456789,,,,finance;office,
Server DC,rdp,dc01.corp.local,3389,administrator,,servers,
สาขาเชียงใหม่,teamviewer,987654321,,,,branch,
```

- `provider` ต้องเป็นหนึ่งใน `anydesk | teamviewer | rustdesk | rdp | vnc | ssh`
- แถวที่ไม่ผ่านการตรวจจะถูกข้ามและแจ้งเลขบรรทัด แถวอื่นยังนำเข้าตามปกติ
- รหัสผ่านใน CSV จะถูกเข้ารหัสเข้า vault ทันที ควรลบไฟล์ CSV ต้นฉบับหลังนำเข้า
