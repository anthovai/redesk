# Aegiron agent สำหรับ Windows

ตัวติดตั้งแบบ NSIS **ติดตั้งรายผู้ใช้ ไม่ต้องใช้สิทธิ์ admin** รองรับ Windows 10/11 (64-bit) หน้าติดตั้งเป็นภาษาไทยหรืออังกฤษตามภาษาของ Windows

| ติดตั้งอะไร | ที่ไหน |
|---|---|
| `aegiron.exe` — โปรแกรมพร้อมหน้าต่าง + ไอคอนใน tray | `%LOCALAPPDATA%\Programs\Aegiron\` |
| `aegiron-agent.exe` — CLI (init, set-password, set, info) | ที่เดียวกัน |
| การตั้งค่าและตัวตนเครื่อง (`agent.toml`) | `%APPDATA%\Aegiron\` |
| log | `%LOCALAPPDATA%\Aegiron\aegiron.log` |
| เปิดอัตโนมัติเมื่อล็อกอิน (เริ่มใน tray) | `HKCU\…\Run\Aegiron` |
| ทางลัด | Start menu และ Desktop |

## สร้างตัวติดตั้ง (บน Mac หรือ Linux)

```sh
brew install mingw-w64 makensis && rustup target add x86_64-pc-windows-gnu   # ทำครั้งเดียว
deploy/windows/build-installer.sh                                             # แบบให้ผู้ใช้กรอกเอง

# แบบใส่ค่าเซิร์ฟเวอร์ขององค์กรไว้ล่วงหน้า (ผู้ใช้กดถัดไปอย่างเดียว)
SERVER_URL=https://aegiron.kgarden.co.th SERVER_KEY='<server key>' deploy/windows/build-installer.sh
```

ได้ไฟล์ `dist/Aegiron-Agent-Setup-<version>[-<โดเมน>].exe` พร้อมไฟล์ `.sha256`

## ติดตั้ง

**แบบที่ง่ายที่สุด (เหมือน RustDesk):** ใช้ตัวติดตั้งแบบทั่วไป เว้นหน้าเซิร์ฟเวอร์ว่างไว้ แล้วให้ผู้ใช้**เข้าสู่ระบบในโปรแกรม**ด้วยอีเมลและรหัสผ่าน (รวม 2FA) ของคอนโซล Aegiron เครื่องจะลงทะเบียนเป็นของผู้ใช้คนนั้นและเข้าสมุดที่อยู่ให้เอง ไม่ต้องคัดลอกกุญแจหรือ token

**ทีละเครื่อง (pre-configured):** ดับเบิลคลิก → ยินดีต้อนรับ → *เชื่อมต่อกับเซิร์ฟเวอร์ Aegiron* (กรอกไว้ให้แล้วถ้าใช้แบบ pre-configured) → ติดตั้ง → ✓ เปิด Aegiron ตอนนี้

**หลายเครื่องพร้อมกัน (GPO / Intune / RMM):** รันในบริบทของผู้ใช้ (user context) ไม่ใช่ SYSTEM

```bat
Aegiron-Agent-Setup-<version>.exe /S /SERVER=https://aegiron.kgarden.co.th /KEY=<server key> /TOKEN=rdk_...
"%LOCALAPPDATA%\Programs\Aegiron\uninstall.exe" /S          :: ถอนการติดตั้งแบบเงียบ (เก็บ ID เครื่องไว้)
```

- **ติดตั้งทับเวอร์ชันเดิม (อัปเกรด):** ID การตั้งค่า และรหัสผ่านถาวรจะยังอยู่ ตัวติดตั้งปิดโปรแกรมเก่าให้เอง
- ตอนถอนการติดตั้ง จะถามว่าจะลบ ID ของเครื่องด้วยหรือไม่ ถ้ารันแบบเงียบจะเก็บไว้เสมอ

## ข้อควรรู้

- **ยังไม่ได้เซ็นโค้ด:** Windows SmartScreen จะเตือน "Windows protected your PC" (กด More info → Run anyway) ถ้าจะใช้กับทั้งองค์กร ให้เซ็นด้วยใบรับรอง code signing ขององค์กร เช่น `osslsigncode sign -pkcs12 org.p12 -pass … -t http://timestamp.digicert.com -in setup.exe -out setup-signed.exe` และเซ็นทั้ง `aegiron.exe` กับ `aegiron-agent.exe` ก่อน build ตัวติดตั้ง
- **ควบคุมเครื่องได้เฉพาะตอนที่ผู้ใช้ล็อกอินอยู่** agent รันใน session ของผู้ใช้ (จับภาพหน้าจอจาก service ใน session 0 ไม่ได้) จึงยังควบคุมหน้าจอล็อกอินหรือหน้าต่าง UAC ไม่ได้
- **การแสดงผล:** ใช้ Direct3D 12 (wgpu) ถ้าใช้ไม่ได้จะเปลี่ยนไปใช้ OpenGL เอง
- **ภาษาไทย:** ใช้ฟอนต์ Leelawadee UI ที่มากับ Windows
- **Tray:** ปิดหน้าต่างแล้วโปรแกรมยังทำงานอยู่ที่ tray ถ้าจะออกจริงให้คลิกขวาที่ไอคอนแล้วเลือก "ออกจาก Aegiron" ถ้าเปิดโปรแกรมซ้ำ (หรือคลิกลิงก์ `aegiron://`) จะส่งต่อไปที่หน้าต่างเดิมผ่าน named pipe ของผู้ใช้คนนั้น (`\\.\pipe\aegiron-…`, ไม่รับการเชื่อมต่อจากเครื่องอื่น)

## เซ็นโค้ด (Authenticode)

`build-installer.sh` เซ็นให้ได้ด้วย [osslsigncode](https://github.com/mtrojnar/osslsigncode) (`brew install osslsigncode`) โดยเซ็น `aegiron.exe` และ `aegiron-agent.exe` ก่อนแพ็ก, เซ็น `uninstall.exe` ระหว่างที่ NSIS สร้าง (`!uninstfinalize`) แล้วเซ็นตัวติดตั้งเป็นขั้นสุดท้าย และตรวจด้วย `osslsigncode verify`

```sh
# PKCS#12 (.p12/.pfx ที่มี certificate chain + private key)
AEGIRON_WIN_SIGN_PKCS12=org-codesign.p12 AEGIRON_WIN_SIGN_PASS_FILE=~/.secrets/p12-pass deploy/windows/build-installer.sh

# หรือ certificate + key แยกไฟล์ (PEM/DER/SPC)
AEGIRON_WIN_SIGN_CERT=org.crt AEGIRON_WIN_SIGN_KEY=org.key deploy/windows/build-installer.sh
```

| ตัวแปร | ความหมาย |
|---|---|
| `AEGIRON_WIN_SIGN_PKCS12` | ไฟล์ PKCS#12 (ใช้อย่างใดอย่างหนึ่งกับ CERT+KEY) |
| `AEGIRON_WIN_SIGN_CERT`, `AEGIRON_WIN_SIGN_KEY` | certificate และ private key |
| `AEGIRON_WIN_SIGN_PASS_FILE` | ไฟล์ที่มีรหัสผ่านของ key (แนะนำ) |
| `AEGIRON_WIN_SIGN_PASS` | รหัสผ่านโดยตรง — สคริปต์เขียนลงไฟล์ชั่วคราวสิทธิ์ 600 ไม่ส่งผ่าน command line |
| `AEGIRON_WIN_SIGN_TIMESTAMP` | RFC 3161 timestamp server (ค่าเริ่มต้น `http://timestamp.digicert.com`) ตั้ง `none` เพื่อไม่ใส่ timestamp — ลายเซ็นจะหมดอายุพร้อมใบรับรอง |
| `AEGIRON_WIN_SIGN_CAFILE` | CA bundle สำหรับขั้นตรวจ (เช่นใบรับรองจาก CA ภายในองค์กร) |

สคริปต์ตรวจค่าทั้งหมดก่อนเริ่ม build: ไม่มี osslsigncode, ไฟล์อ่านไม่ได้, ตั้งทั้ง PKCS#12 และ CERT/KEY, ตั้งรหัสผ่านสองแบบ หรือ URL timestamp ผิดรูปแบบ จะหยุดพร้อมข้อความ ถ้าเซ็นไม่สำเร็จ (เช่นรหัสผ่านผิด) จะแสดงข้อความของ osslsigncode แล้วหยุด ไม่ตั้งตัวแปรเหล่านี้ = build แบบไม่เซ็นเหมือนเดิม

ใบรับรองแบบ OV ทั่วไปยังทำให้ SmartScreen เตือนจนกว่าไฟล์จะมีประวัติการดาวน์โหลดพอ ใบรับรอง EV หรือ key ที่อยู่ใน HSM/token ต้องใช้ PKCS#11 ของ osslsigncode ซึ่งสคริปต์นี้ยังไม่รองรับ

