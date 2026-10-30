#!/bin/sh
# SSH_PASSWORD: password of "pwuser". SSH_AUTHORIZED_KEYS: public key line(s) for "keyuser" (key only).
set -e
echo "pwuser:${SSH_PASSWORD:?}" | chpasswd
usermod -p '*' keyuser   # no usable password, but not locked for key logins
install -d -m 700 -o keyuser -g keyuser /home/keyuser/.ssh
printf '%s\n' "${SSH_AUTHORIZED_KEYS:?}" > /home/keyuser/.ssh/authorized_keys
chown keyuser:keyuser /home/keyuser/.ssh/authorized_keys
chmod 600 /home/keyuser/.ssh/authorized_keys
cat > /etc/ssh/sshd_config.d/aegiron-test.conf <<'CONF'
PasswordAuthentication yes
KbdInteractiveAuthentication no
PermitRootLogin no
CONF
ssh-keygen -A
exec /usr/sbin/sshd -D -e
