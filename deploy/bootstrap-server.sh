#!/bin/sh
set -eu

if [ "$(id -u)" -ne 0 ] || [ "$#" -ne 1 ]; then
    echo "usage: sudo bootstrap-server.sh <deploy-public-key-file>" >&2
    exit 2
fi

public_key_file=$1
public_key=$(sed -n '1p' "$public_key_file")
case "$public_key" in
    ssh-ed25519\ *) ;;
    *) echo "deployment key must be one ssh-ed25519 public key" >&2; exit 2 ;;
esac
if [ "$(wc -l < "$public_key_file")" -ne 1 ]; then
    echo "deployment public-key file must contain exactly one line" >&2
    exit 2
fi

script_directory=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)

if ! getent passwd agentpulse-relay >/dev/null; then
    useradd --system --home-dir /var/lib/agentpulse-relay --shell /usr/sbin/nologin agentpulse-relay
fi
if ! getent passwd agentpulse-deploy >/dev/null; then
    useradd --create-home --home-dir /var/lib/agentpulse-deploy --shell /bin/sh agentpulse-deploy
fi

install -d -o agentpulse-relay -g agentpulse-relay -m 0700 /var/lib/agentpulse-relay
install -d -o root -g agentpulse-relay -m 0750 /etc/agentpulse-relay
install -d -o root -g root -m 0755 /opt/agentpulse-relay/releases
install -d -o agentpulse-deploy -g agentpulse-deploy -m 0700 /var/lib/agentpulse-deploy/.ssh
install -d -o agentpulse-deploy -g agentpulse-deploy -m 0700 /var/lib/agentpulse-deploy/incoming

authorized_keys=/var/lib/agentpulse-deploy/.ssh/authorized_keys
printf 'restrict %s\n' "$public_key" > "$authorized_keys"
chown agentpulse-deploy:agentpulse-deploy "$authorized_keys"
chmod 0600 "$authorized_keys"

install -o root -g root -m 0755 "$script_directory/agentpulse-relay-deploy" /usr/local/sbin/agentpulse-relay-deploy
install -o root -g root -m 0644 "$script_directory/agentpulse-relay.service" /etc/systemd/system/agentpulse-relay.service
printf '%s\n' 'agentpulse-deploy ALL=(root) NOPASSWD: /usr/local/sbin/agentpulse-relay-deploy *' > /etc/sudoers.d/agentpulse-relay-deploy
chmod 0440 /etc/sudoers.d/agentpulse-relay-deploy
visudo -cf /etc/sudoers.d/agentpulse-relay-deploy >/dev/null
systemctl daemon-reload
systemctl enable agentpulse-relay.service

echo "AgentPulse Relay service and deployment accounts are installed."
