# การเชื่อมต่อผ่านผู้ให้บริการอื่น (AnyDesk, TeamViewer, RustDesk, RDP, VNC, SSH)

ReDesk รวมเครื่องที่ใช้โปรแกรมรีโมตต่างยี่ห้อไว้ในคอนโซลเดียว (หน้า **การเชื่อมต่อ**) ด้วยสิทธิ์ตามกลุ่ม vault ที่เข้ารหัส และ audit log ชุดเดียวกับ ReDesk มีกลไก 3 แบบตามความสามารถของแต่ละผู้ให้บริการ

| ผู้ให้บริการ | เปิดเครื่อง | ดึงข้อมูลจาก API | ข้อจำกัด |
|---|---|---|---|
| AnyDesk | เปิดแอป AnyDesk ด้วย `anydesk:<ID>` | ไม่มี (ใช้ CSV import) | REST API สาธารณะไม่เปิดข้อมูลอุปกรณ์หรือเซสชัน และรายละเอียด auth อยู่ในพอร์ทัลลูกค้าเท่านั้น |
| TeamViewer | เปิดแอปด้วย `teamviewer10://control?device=<ID>` | ✅ อุปกรณ์ + ประวัติการเชื่อมต่อ | ต้องใช้ script token ของ TeamViewer ที่มีสิทธิ์ API |
| RustDesk | เปิดแอปด้วย `rustdesk://connection/new/<ID>` | ไม่มี | — |
| RDP / VNC / SSH | **เปิดในเบราว์เซอร์ผ่าน gateway** หรือเปิดโปรแกรมในเครื่อง (`.rdp`, `vnc://`, `ssh://`) | — | gateway ต้องเข้าถึงเครื่องปลายทางได้ทางเครือข่าย |

ReDesk ไม่ได้ควบคุมเครื่องผ่านโปรโตคอลของ AnyDesk หรือ TeamViewer เอง เพราะเป็นโปรโตคอลปิดและการทำ reverse engineering ขัดกับเงื่อนไขการใช้งานของทั้งสองเจ้า ReDesk ทำหน้าที่เป็นศูนย์กลางที่เปิดโปรแกรมของเจ้านั้นให้ เก็บรหัสผ่าน และบันทึกว่าใครเปิดเครื่องไหนเมื่อไร

## 1. ตัวเปิดโปรแกรมและ vault

- **รหัสผ่านไม่ถูกใส่ในลิงก์** (เพราะจะไปค้างใน browser history และรายการ process) เมื่อกด "เปิดใน AnyDesk/TeamViewer/RustDesk" เซิร์ฟเวอร์จะคืนลิงก์ที่มีแค่ ID ส่วนคอนโซลคัดลอกรหัสผ่านไปที่คลิปบอร์ดและล้างให้ภายใน 30 วินาที ถ้าเบราว์เซอร์ไม่อนุญาตจะแสดงกล่องให้กดคัดลอกเอง
- **ID ของผู้ให้บริการต้องเป็นตัวเลขเท่านั้น** (AnyDesk รับ alias แบบ `name@ad` ได้ด้วย) URL handler ของโปรแกรมเหล่านี้เคยมีช่องโหว่ argument injection เช่น TeamViewer [CVE-2020-13699](https://nvd.nist.gov/vuln/detail/CVE-2020-13699) ดังนั้นห้ามมีอักขระอื่นปนมา ค่าที่ซิงก์มาจาก API ก็ผ่านการตรวจแบบเดียวกัน
- ชื่อผู้ใช้ โดเมน และโฮสต์ห้ามมีอักขระควบคุม เพื่อกันการแทรกบรรทัดในไฟล์ `.rdp` เช่น `alternate shell:s:cmd.exe` และกันโฮสต์ที่ขึ้นต้นด้วย `-` (`ssh -oProxyCommand=…`)
- **Vault:** รหัสผ่านและ API token เข้ารหัสด้วย AES-256-GCM ใช้กุญแจจาก `REDESK_VAULT_KEY` (base64 ขนาด 32 ไบต์) หรือไฟล์ `data/vault.key` ที่สร้างให้อัตโนมัติ **เก็บกุญแจแยกจาก backup ของฐานข้อมูล** เพราะถ้าได้ dump ฐานข้อมูลไปอย่างเดียวจะยังอ่านรหัสผ่านไม่ได้
- **สิทธิ์:** ผู้ดูแลระบบเห็นทุกรายการ ผู้ใช้ทั่วไปเห็นเฉพาะรายการที่ตัวเองสร้าง และรายการในกลุ่มที่ตัวเองเป็นสมาชิก เฉพาะผู้ดูแลระบบที่แชร์รายการเข้ากลุ่มได้
- **Audit:** `endpoint.created/updated/deleted/imported`, `endpoint.launch` (บันทึกด้วยว่ามีการปล่อยรหัสผ่านหรือไม่), `gateway.start/end/failed`, `integration.*`

## 2. Gateway สำหรับ RDP / VNC / SSH ในเบราว์เซอร์

```
Browser (guacamole-common-js) ──WSS /ws/gateway──▶ redesk-server ──TCP──▶ guacd ──RDP/VNC/SSH──▶ เครื่องปลายทาง
                                               ตรวจสิทธิ์, ถอดรหัสรหัสผ่านจาก vault,
                                               handshake กับ guacd, บันทึก audit
```

- เปิดใช้โดยตั้ง `REDESK_GUACD_ADDR=host:4822` (docker-compose ตั้งไว้ให้แล้ว)
- เบราว์เซอร์ไม่ได้รับรหัสผ่าน: เซิร์ฟเวอร์ใส่รหัสใน `connect` ของ guacd เอง
- เซิร์ฟเวอร์ส่งต่อเฉพาะ instruction ที่สมบูรณ์ ตอบ ping ภายในของ tunnel เอง และนับ byte ของแต่ละเซสชัน (ดูได้ในหน้าเซสชัน)
- **บันทึกเซสชัน:** ตั้ง `REDESK_GATEWAY_RECORDING_PATH` (เป็น path **ภายใน container ของ guacd**) แล้ว guacd จะบันทึกทุกเซสชันเป็นไฟล์ Guacamole recording ชื่อตาม session id สำหรับ SSH จะได้ typescript ด้วย ไฟล์เหล่านี้เปิดดูย้อนหลังได้ด้วย `guacamole-common-js` (`Guacamole.SessionRecording`) ซึ่งยังไม่มีหน้าเล่นซ้ำในคอนโซล
- RDP ใช้ `resize-method=display-update` ทำให้ปรับขนาดหน้าต่างแล้วความละเอียดหน้าจอปลายทางปรับตาม ตั้ง "ไม่ตรวจใบรับรอง" ได้ทีละเครื่อง สำหรับเซิร์ฟเวอร์ภายในที่ใช้ใบรับรอง self-signed
- SSH: guacd **ไม่ได้ตรวจ host key** (log จะเตือน "No known host keys provided") ถ้าต้องการกัน MITM ภายในเครือข่าย ให้วาง guacd ไว้ในเครือข่ายเดียวกับเซิร์ฟเวอร์ปลายทาง

## 3. TeamViewer API

1. ใน TeamViewer Management Console → Profile → Apps → **Create script token** ให้สิทธิ์อ่านเท่านั้น ได้แก่ Computers & Contacts (ดูรายการ) และ Connection reporting (อ่านรายงาน) ตามคำแนะนำของ TeamViewer ให้ token มีสิทธิ์น้อยที่สุด ([KB](https://www.teamviewer.com/en/global/support/knowledge-base/teamviewer-remote/for-developers/use-the-teamviewer-api/)) และมีรายงานจากผู้ใช้ว่าการอ่านรายงานต้องใช้ *user* token ไม่ใช่ company token
2. ReDesk → ตั้งค่าเซิร์ฟเวอร์ → **TeamViewer API** → ใส่ token แล้วเลือกกลุ่มสำหรับอุปกรณ์ใหม่
3. ระบบจะซิงก์ทุก `REDESK_SYNC_MINUTES` นาที (ค่าเริ่มต้น 15) หรือกด "ซิงก์ตอนนี้"
   - `GET /api/v1/devices` → อุปกรณ์จะปรากฏในหน้าการเชื่อมต่อ พร้อมสถานะออนไลน์ ชื่อ/ID จะตามข้อมูลจาก TeamViewer ส่วนกลุ่ม รหัสผ่าน และแท็กที่แก้ใน ReDesk จะคงไว้
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
