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
- **Tray:** ปิดหน้าต่างแล้วโปรแกรมยังทำงานอยู่ที่ tray ถ้าจะออกจริงให้คลิกขวาที่ไอคอนแล้วเลือก "ออกจาก Aegiron" ถ้าเปิดโปรแกรมซ้ำจะสลับไปที่หน้าต่างเดิม (ใช้พอร์ต 127.0.0.1:21119 ตรวจว่าเปิดอยู่แล้วหรือไม่)
