#!/bin/sh
# Runs after install and upgrade (deb: configure, rpm: 1/2, apk, Arch).
set -e

GROUP=singbox-board

# Members of this group may use the dashboard without sudo.
if ! grep -q "^${GROUP}:" /etc/group; then
	if command -v groupadd >/dev/null 2>&1; then
		groupadd --system "$GROUP"
	elif command -v addgroup >/dev/null 2>&1; then
		addgroup -S "$GROUP"
	fi
fi

if [ -d /run/systemd/system ]; then
	systemctl daemon-reload >/dev/null 2>&1 || true
	if systemctl is-active --quiet singbox-board; then
		systemctl restart singbox-board || true
	else
		systemctl enable --now singbox-board >/dev/null 2>&1 || true
	fi
elif command -v rc-update >/dev/null 2>&1 && [ -x /etc/init.d/singbox-board ]; then
	rc-update add singbox-board default >/dev/null 2>&1 || true
	rc-service singbox-board restart >/dev/null 2>&1 || rc-service singbox-board start || true
fi

cat <<'MSG'

singbox-board is installed and the daemon is running.
  1. sudo singbox-board update                 # install the sing-box core
  2. put your sing-box config at /etc/sing-box/config.json, then: sudo singbox-board start
  3. sudo usermod -aG singbox-board "$USER"    # use the dashboard without sudo (log in again)
  4. singbox-board                             # dashboard; first run asks about Sub-Store / http-meta

MSG
