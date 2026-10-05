#!/bin/sh
# Runs after removal. `dpkg --purge` also deletes the components installed
# by the daemon; the sing-box core and its configuration are left alone.
set -e

if [ -d /run/systemd/system ]; then
	systemctl daemon-reload >/dev/null 2>&1 || true
fi

if [ "${1:-}" = purge ]; then
	rm -rf /var/lib/singbox-board
fi
