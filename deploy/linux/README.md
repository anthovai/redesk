# Aegiron สำหรับ Linux

แอปบนเครื่อง (`aegiron` — หน้าต่าง, tray, หน้าต่างควบคุม) และ agent แบบ command line (`aegiron-agent`) สำหรับ Linux x86_64 และ aarch64 ทำงานแบบรายผู้ใช้ ไม่ต้องมี daemon ระดับ root

## รองรับ

| | |
|---|---|
| ระบบ | Ubuntu 24.04 ขึ้นไป, Debian 13, Fedora 40 ขึ้นไป หรือ distro ที่มี glibc 2.39 และ PipeWire 1.0 ขึ้นไป (Ubuntu 22.04 / Debian 12 ใช้ไม่ได้: PipeWire เก่าเกินไปสำหรับ xcap) |
| X11 | จับภาพจอผ่าน xcap (XCB/RandR) หลายจอได้, เมาส์/คีย์บอร์ดผ่าน XTEST (enigo), แสดงเคอร์เซอร์ของเครื่องปลายทาง |
| Wayland | ผ่าน XDG desktop portal: ScreenCast (ภาพจาก PipeWire) + RemoteDesktop (เมาส์/คีย์บอร์ด) ครั้งแรกระบบจะถามผู้ใช้ที่เครื่องว่าจะแชร์จอไหม แล้วจำการอนุญาตไว้ (restore token) จนกว่าจะเพิกถอนในการตั้งค่าของเดสก์ท็อป เคอร์เซอร์อยู่ในภาพวิดีโอ |
| เสียง | ส่งเสียงที่เครื่องเล่นอยู่ (monitor ของลำโพงหลัก) ผ่าน PulseAudio หรือ PipeWire (pipewire-pulse) และเล่นเสียงจากเครื่องปลายทางผ่าน ALSA |
| คลิปบอร์ด | ข้อความและรายชื่อไฟล์ ทั้ง X11 และ Wayland (data-control: KDE, wlroots; GNOME ใช้ผ่าน Xwayland) |
| Tray | StatusNotifierItem (KDE, Xfce, Cinnamon, Budgie, GNOME ที่ติดตั้ง extension AppIndicator) ถ้าไม่มี tray ปิดหน้าต่างแล้วโปรแกรมจะออก และ `--background` จะเปิดหน้าต่างแทน |

**Wayland ต้องมี** `xdg-desktop-portal` กับ backend ของเดสก์ท็อป (`xdg-desktop-portal-gnome`, `-kde` หรือ `-wlr`) และ PipeWire ถ้าไม่มีหรือผู้ใช้กดไม่อนุญาต ผู้ควบคุมจะเห็นข้อความ "เครื่องปลายทางแชร์หน้าจอไม่ได้" พร้อมเหตุผล (`auth_result` reason `screen_unavailable`) และใน log ของ agent; backend `-wlr` (Sway, Hyprland) มีแต่ ScreenCast จึงดูได้อย่างเดียว ไม่มีเมาส์/คีย์บอร์ด ตั้ง `AEGIRON_CAPTURE=x11` เพื่อบังคับใช้ X11 (Xwayland เห็นเฉพาะหน้าต่าง X11) หรือ `AEGIRON_CAPTURE=portal` เพื่อใช้ portal บน X11

## ติดตั้ง

| ไฟล์ | ใช้กับ |
|---|---|
| `aegiron_<version>-1_amd64.deb` / `_arm64.deb` | Ubuntu, Debian: `sudo apt install ./aegiron_<version>-1_amd64.deb` |
| `aegiron-<version>-1.x86_64.rpm` / `.aarch64.rpm` | Fedora, openSUSE, RHEL: `sudo dnf install ./aegiron-<version>-1.x86_64.rpm` |
| `aegiron-linux-<arch>-<version>.tar.gz` | distro อื่น หรือไม่มีสิทธิ์ root: แตกไฟล์แล้ว `./install.sh` (ลงใน `~/.local`) |

| ติดตั้งอะไร | ที่ไหน (แพ็กเกจ / tar.gz) |
|---|---|
| `aegiron`, `aegiron-agent` | `/usr/bin` / `~/.local/bin` |
| เมนูแอปและตัวเปิดลิงก์ `aegiron://` (`x-scheme-handler/aegiron`, `redesk`) | `/usr/share/applications/aegiron.desktop` / `~/.local/share/applications` |
| ไอคอน | `…/share/icons/hicolor/256x256/apps/aegiron.png` |
| systemd user unit ของ agent แบบไม่มีหน้าต่าง (ไม่ได้เปิดไว้) | `/usr/lib/systemd/user/aegiron-agent.service` / ไฟล์ใน tar.gz |
| การตั้งค่าและตัวตนเครื่อง (`agent.toml`), socket กันเปิดซ้ำ | `~/.config/Aegiron/` |
| log ของแอป | `~/.local/share/Aegiron/aegiron.log` |
| เปิดอัตโนมัติเมื่อล็อกอิน (เริ่มใน tray) | `~/.config/autostart/aegiron.desktop` (เปิด/ปิดจากหน้าตั้งค่าของแอป) |

- **เปิดซ้ำ:** เปิดแอปครั้งที่สองหรือคลิกลิงก์ `aegiron://connect/<id>` จะส่งต่อให้โปรแกรมที่เปิดอยู่ผ่าน Unix socket แล้วออก
- **agent แบบไม่มีหน้าต่าง** (kiosk, เครื่องที่ไม่มีคนใช้): `aegiron-agent init …` แล้ว `aegiron-agent install` (เขียน `~/.config/systemd/user/aegiron-agent.service` และเปิดทันที) หรือ `systemctl --user enable --now aegiron-agent.service` จากแพ็กเกจ อย่าเปิดทั้งแอปและ agent สำหรับผู้ใช้คนเดียวกัน ถ้าไม่มีแอปเปิดอยู่ การขออนุญาตจะใช้ `zenity` หรือ `kdialog`
- **ถอนการติดตั้ง:** `sudo apt remove aegiron` / `sudo dnf remove aegiron` / `./install.sh --uninstall` การตั้งค่าใน `~/.config/Aegiron` ยังอยู่

## สร้างแพ็กเกจ

```sh
deploy/linux/build-packages.sh             # บน Linux ที่มี dependency ครบ (ดู Dockerfile)
deploy/linux/build-packages.sh --docker    # บน Mac หรือที่ไหนก็ได้ที่มี Docker (ค่าเริ่มต้นเมื่อไม่ใช่ Linux)
PLATFORM=linux/amd64 deploy/linux/build-packages.sh --docker   # อีกสถาปัตยกรรม (ผ่าน emulation ช้า)
```

ได้ `.deb`, `.rpm` และ `.tar.gz` ของสถาปัตยกรรมเครื่องที่ build ใน `dist/` พร้อมไฟล์ `.sha256` สคริปต์ใช้ `cargo deb` และ `cargo generate-rpm` (ค่าใน `[package.metadata.*]` ของ `agent/Cargo.toml`) บน Ubuntu 24.04 ตาม `deploy/linux/Dockerfile` dependency ตอน build: PipeWire และ libspa (headers), clang (bindgen), ALSA, XCB, EGL, D-Bus; libpulse ไม่ต้องใช้ตอน build เพราะโหลดตอนรัน (เครื่องที่ไม่มีก็ใช้งานได้ แค่ไม่มีเสียง)

ยังไม่มี AppImage: `.tar.gz` ใช้แทนสำหรับ distro อื่น (ต้องมี glibc และ PipeWire ตามตารางข้างบน)

## ทดสอบ

CI (`.github/workflows/ci.yml`) build และทดสอบบน Ubuntu 24.04 ทั้ง x86_64 และ aarch64 ด้วย `deploy/linux/test-x11.sh`: Xvfb 1280×720 แล้วรัน `cargo test` รวมการทดสอบที่ปกติข้ามไว้ — จับภาพจอจริงแล้วตรวจสีทั้ง JPEG และ H.264 (`agent/tests/linux_x11.rs`), เลื่อนเมาส์ผ่าน XTEST แล้วอ่านตำแหน่งกลับ, คลิปบอร์ดไป-กลับกับ `xclip`, เสียงผ่าน PulseAudio null sink (เล่นผ่าน cpal/ALSA แล้วจับจาก monitor) จากนั้น job แยก build แพ็กเกจและติดตั้ง `.deb` บน Ubuntu 24.04 ใหม่เพื่อตรวจ dependency, `aegiron-agent --help`, ไฟล์ `.desktop` และตัวเปิดลิงก์

รันเองบนเครื่อง Linux หรือใน container ของ `Dockerfile`:

```sh
deploy/linux/test-x11.sh
```

**ยังไม่ได้ทดสอบกับเดสก์ท็อป Wayland จริง** (GNOME, KDE): โค้ด portal ทดสอบได้เฉพาะการแปลงภาพและ keysym ใน `cargo test`; ลำดับ CreateSession → SelectDevices → SelectSources → Start → OpenPipeWireRemote และการส่งเมาส์/คีย์บอร์ดตามเอกสาร xdg-desktop-portal ยังต้องลองบนเครื่องจริง เช่นเดียวกับ tray บน KDE/GNOME และ autostart หลังล็อกอิน
