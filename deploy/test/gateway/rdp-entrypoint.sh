#!/bin/sh
# RDP_PASSWORD: password of user "aegiron". RDP_SECURITY: xrdp security_layer (negotiate | tls | rdp).
set -e
echo "aegiron:${RDP_PASSWORD:?}" | chpasswd
sed -i "s/^security_layer=.*/security_layer=${RDP_SECURITY:-negotiate}/" /etc/xrdp/xrdp.ini
# The desktop is a blue root window plus one xterm (no window manager needed).
cat > /etc/xrdp/startwm.sh <<'WM'
#!/bin/sh
xsetroot -solid '#1f5fa8'
exec xterm -geometry 100x30+40+40 -fa Monospace -fs 12 -bg black -fg '#7CFC00'
WM
chmod +x /etc/xrdp/startwm.sh
rm -f /var/run/xrdp/*.pid /var/run/xrdp*.pid
xrdp-sesman
exec xrdp --nodaemon
