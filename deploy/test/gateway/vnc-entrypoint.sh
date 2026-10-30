#!/bin/sh
# VNC_PASSWORD: VNC password (classic VncAuth uses at most 8 characters).
set -e
printf '%s\n' "${VNC_PASSWORD:?}" | vncpasswd -f > "$HOME/vncpw"
chmod 600 "$HOME/vncpw"
Xvnc :0 -rfbport 5900 -rfbauth "$HOME/vncpw" -SecurityTypes VncAuth -geometry 1280x800 -depth 24 \
     -AcceptCutText -SendCutText -localhost=0 &
export DISPLAY=:0
for i in $(seq 50); do xsetroot -solid '#2e7d32' 2>/dev/null && break; sleep 0.2; done
# Restart the terminal if someone exits it, so the display never goes blank.
while true; do xterm -geometry 100x30+40+40 -fa Monospace -fs 12 -bg black -fg white; done
