# ติดตั้ง ReDesk บน Mac Server

ชุดติดตั้งนี้ทำให้ Mac เครื่องหนึ่งเป็นเซิร์ฟเวอร์ ReDesk แบบเปิดตลอด 24 ชั่วโมง ทุก service เริ่มเองตอนบูตผ่าน launchd **โดยไม่ต้องมีใครล็อกอินค้างไว้**

```
อินเทอร์เน็ต ──443──▶ Caddy (HTTPS, Let's Encrypt) ──▶ redesk-server (127.0.0.1:21114) ──▶ PostgreSQL 16 (127.0.0.1:5432)
                                                              └──▶ guacd ใน Colima (127.0.0.1:4822) ──▶ เครื่อง RDP/VNC/SSH ในเครือข่าย
```

| ส่วน | รันเป็นผู้ใช้ | launchd label |
|---|---|---|
| redesk-server | `_redesk` (บัญชีระบบแบบซ่อน ไม่มีรหัสผ่าน ไม่มี shell) | `co.redesk.server` |
| Caddy | `_redesk` (macOS อนุญาตให้ผู้ใช้ทั่วไปเปิดพอร์ต 80/443 ได้) | `co.redesk.caddy` |
| PostgreSQL 16 | admin ที่เป็นเจ้าของ Homebrew | `co.redesk.postgresql` |
| Colima + guacd | admin ที่เป็นเจ้าของ Homebrew | `co.redesk.colima` |
| Backup รายวัน 02:30 | root | `co.redesk.backup` |

## สิ่งที่ต้องเตรียม

1. **Mac** ที่ใช้ macOS 13 ขึ้นไป (Apple Silicon หรือ Intel) RAM อย่างน้อย 8 GB (guacd ใน Colima ใช้ 2 GB) ต่อสายแลนและเปิดเครื่องตลอดเวลา
2. **Homebrew** ติดตั้งด้วยบัญชี admin ที่จะดูแลเครื่อง (https://brew.sh)
3. **โดเมน** เช่น `redesk.kgarden.co.th` ตั้ง DNS record A/AAAA ชี้ไปที่ public IP ของสำนักงาน
4. **Router/Firewall:** forward พอร์ต **TCP 80 และ 443** มาที่ Mac เครื่องนี้ (Let's Encrypt ใช้ 80 ในการยืนยันโดเมน) ห้ามเปิดพอร์ตอื่น เพราะ 21114, 5432, 4822 ฟังอยู่แค่บน localhost
5. ตั้ง IP ภายในของ Mac ให้คงที่ (DHCP reservation)
6. ถ้าจะใช้ gateway RDP/VNC/SSH: Mac ต้องเข้าถึงเครื่องปลายทางในเครือข่ายได้

## ติดตั้ง

คัดลอก `redesk-server-macos-<version>.tar.gz` ไปที่ Mac server (AirDrop, scp หรือ USB) แล้วรันด้วยบัญชี admin ที่เป็นเจ้าของ Homebrew:

```sh
shasum -a 256 redesk-server-macos-*.tar.gz      # เทียบกับไฟล์ SHA256SUMS
tar -xzf redesk-server-macos-*.tar.gz
cd redesk-server-macos-*/

sudo ./install.sh --domain redesk.kgarden.co.th --email it@kgarden.co.th --dry-run   # ดูก่อนว่าจะทำอะไรบ้าง
sudo ./install.sh --domain redesk.kgarden.co.th --email it@kgarden.co.th --server-power
```

| ตัวเลือก | |
|---|---|
| `--domain` | ชื่อโดเมนของเซิร์ฟเวอร์ (บังคับ) |
| `--email` | อีเมลสำหรับ Let's Encrypt และใช้เป็นบัญชี admin คนแรก (บังคับในการติดตั้งครั้งแรก) |
| `--server-power` | ตั้งค่าไม่ให้เครื่องหลับ และให้เปิดเครื่องเองเมื่อไฟกลับมา |
| `--no-gateway` | ไม่ติดตั้ง Colima/guacd (จะไม่มีการเปิด RDP/VNC/SSH ในเบราว์เซอร์) |
| `--database-url` | ใช้ PostgreSQL ที่มีอยู่แล้วแทนการติดตั้งใหม่ |
| `--dry-run` | แสดงขั้นตอนทั้งหมดโดยยังไม่เปลี่ยนแปลงอะไร |

เมื่อติดตั้งเสร็จ สคริปต์จะแสดง **รหัสผ่าน admin ครั้งแรก (แสดงครั้งเดียว)** และ **server public key** สำหรับใช้ติดตั้ง agent

ขั้นตอนหลังติดตั้ง:
1. เปิด `https://<โดเมน>` ล็อกอิน แล้ว **เปลี่ยนรหัสผ่านและเปิด 2FA** ที่หน้า "บัญชีของฉัน"
2. **สำรองกุญแจไว้นอกเครื่อง** (เก็บแยกจาก backup ฐานข้อมูล):
   `sudo cp /usr/local/var/redesk/data/{server_ed25519.key,vault.key} /Volumes/<USB>/`
3. ถ้าจะใช้กับองค์กรจริง ให้ไปที่ ตั้งค่าเซิร์ฟเวอร์ แล้วเปิด "บังคับใช้ enrollment token" และเลือกนโยบายการเข้าถึง "ตามกลุ่ม"

## ติดตั้ง agent บนเครื่องที่จะถูกควบคุม (Mac)

ใช้ไฟล์ `redesk-agent-macos-<version>.tar.gz` (เป็น universal binary รันได้ทั้ง Apple Silicon และ Intel) ทำตาม `README.txt` ในไฟล์นั้น สรุปคือ:

```sh
redesk-agent init --server https://redesk.kgarden.co.th --key <server key> [--enroll-token rdk_...]
redesk-agent install
```

> agent ใช้ลายเซ็นแบบ ad-hoc ทุกครั้งที่ build ใหม่ macOS จะถามสิทธิ์ Screen Recording/Accessibility ซ้ำ ถ้าจะแจกจ่ายผ่าน MDM พร้อม PPPC profile ให้เซ็นด้วย Developer ID ขององค์กร

## ดูแลระบบ

```sh
sudo launchctl kickstart -k system/co.redesk.server     # restart server (หลังแก้ /usr/local/etc/redesk/redesk.env)
sudo launchctl print system/co.redesk.server | grep state
tail -f /usr/local/var/log/redesk/server.log            # หรือ caddy.log, postgresql.log, colima.log, backup.log
sudo /usr/local/redesk/bin/redesk-backup                # สั่ง backup ทันที
```

**อัปเกรด:** แตกไฟล์ kit เวอร์ชันใหม่ แล้วรัน `sudo ./install.sh` อีกครั้ง (ไม่ต้องใส่ `--domain`) สคริปต์จะคงค่าตั้ง ฐานข้อมูล และกุญแจเดิมไว้ เปลี่ยนแค่โปรแกรมกับหน้าเว็บ ส่วน migration ของฐานข้อมูลจะรันเองตอนเซิร์ฟเวอร์เริ่ม

**กู้คืน:** `pg_restore --clean --if-exists -d "<REDESK_DATABASE_URL>" db-....dump` แล้วนำ `server_ed25519.key` และ `vault.key` กลับไปไว้ที่ `/usr/local/var/redesk/data/` (เจ้าของ `_redesk`, สิทธิ์ 600)

**ไฟล์บันทึกเซสชัน RDP/VNC/SSH** อยู่ใน Docker volume `redesk-recordings` ใน Colima:
`docker --context colima run --rm -v redesk-recordings:/r alpine ls -la /r`

**ถอนการติดตั้ง:** `sudo /usr/local/redesk/uninstall.sh` (เก็บข้อมูลไว้) หรือเพิ่ม `--purge` เพื่อลบทุกอย่าง

## ตำแหน่งไฟล์

| Path | เนื้อหา |
|---|---|
| `/usr/local/redesk/` | โปรแกรม หน้าเว็บ และสคริปต์ |
| `/usr/local/etc/redesk/redesk.env` | ค่าตั้ง (มีรหัสผ่านฐานข้อมูล สิทธิ์ 640 root:_redesk) |
| `/usr/local/etc/redesk/Caddyfile` | ค่าตั้ง HTTPS |
| `/usr/local/var/redesk/data/` | **server key + vault key** |
| `/usr/local/var/redesk/caddy/` | ใบรับรอง TLS ของ Caddy |
| `/usr/local/var/redesk/backups/` | backup รายวัน |
| `/usr/local/var/log/redesk/` | log (หมุนเวียนด้วย newsyslog) |
