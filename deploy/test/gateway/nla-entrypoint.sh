#!/bin/sh
# RDP_PASSWORD: password of "aegiron", checked by NLA (NTLM hash in a SAM file).
set -e
cd "$HOME"
winpr-hash -u aegiron -p "${RDP_PASSWORD:?}" -f sam > sam
Xvfb :0 -screen 0 1280x800x24 -nolisten tcp &
export DISPLAY=:0
for i in $(seq 50); do xsetroot -solid '#8e24aa' 2>/dev/null && break; sleep 0.2; done
(while true; do xterm -geometry 100x30+40+40 -fa Monospace -fs 12 -bg black -fg white; done) &
# NLA alone checks the credentials (NTLM against the SAM file); the shadow server makes its
# own self-signed certificate.
exec freerdp-shadow-cli /port:3389 /sec:nla /sam-file:"$HOME/sam"
