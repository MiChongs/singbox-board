#!/bin/sh
# Runs before removal; upgrades keep the service running.
#   deb: remove | upgrade | ...   rpm: 0 = erase, 1 = upgrade
set -e

case "${1:-}" in
upgrade | failed-upgrade | 1) exit 0 ;;
esac

if [ -d /run/systemd/system ]; then
	systemctl disable --now singbox-board >/dev/null 2>&1 || true
elif command -v rc-service >/dev/null 2>&1; then
	rc-service singbox-board stop >/dev/null 2>&1 || true
	rc-update del singbox-board default >/dev/null 2>&1 || true
fi
