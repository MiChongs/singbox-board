#!/bin/sh
# singbox-board one-click installer, upgrader and uninstaller.
#
#   sudo sh -c "$(curl -fsSL https://raw.githubusercontent.com/MiChongs/singbox-board/main/install.sh)"
#   sudo sh -c "$(curl -fsSL .../install.sh)" install.sh --mirror https://ghfast.top/
#   curl -fsSL .../install.sh | sudo sh     # non-interactive: no first-run question
#   sudo sh install.sh --uninstall [--purge]
#
# Installs the static binary from the latest GitHub release (verified against
# SHA256SUMS), the systemd unit or OpenRC script, /etc/singbox-board/daemon.toml
# and the desktop entry of the system tray,
# starts the daemon, installs the sing-box core and asks whether to enable the
# optional components (Sub-Store, http-meta). Re-running it upgrades in place.
set -eu

REPO=${SBB_REPO:-MiChongs/singbox-board}
MIRROR=${SBB_MIRROR:-}
VERSION=""
PREFIX=/usr/local
ROOT=""
LOCAL=""
START=1
CORE=1
SUB_STORE=""
HTTP_META=""
ACTION=install
PURGE=0
GROUP=singbox-board

usage() {
	cat <<EOF
Usage: install.sh [options]

Options:
  --version vX.Y.Z    install this release instead of the latest
  --mirror URL        prefix for github.com downloads, e.g. https://ghfast.top/
                      (also written to daemon.toml for sing-box and component downloads)
  --prefix DIR        install the binary into DIR/bin (default: /usr/local)
  --local DIR         install from an extracted release archive in DIR
  --no-start          install files only; do not enable or start the daemon
  --no-core           do not install the sing-box core
  --sub-store yes|no  answer the first-run question without prompting
  --http-meta yes|no  answer the first-run question without prompting
  --uninstall         remove singbox-board (keeps configuration and data)
  --purge             with --uninstall: also remove configuration, components,
                      the sing-box binary and the $GROUP group
  --root DIR          stage files below DIR without touching services (testing)
  -h, --help          show this help

Environment: SBB_REPO (default $REPO), SBB_MIRROR
EOF
}

if [ -t 1 ]; then
	BOLD=$(printf '\033[1m') CYAN=$(printf '\033[1;36m') YELLOW=$(printf '\033[1;33m')
	RED=$(printf '\033[1;31m') RESET=$(printf '\033[0m')
else
	BOLD="" CYAN="" YELLOW="" RED="" RESET=""
fi
say() { printf '%s==>%s %s\n' "$CYAN" "$RESET" "$*"; }
warn() { printf '%swarning:%s %s\n' "$YELLOW" "$RESET" "$*" >&2; }
die() {
	printf '%serror:%s %s\n' "$RED" "$RESET" "$*" >&2
	exit 1
}
has() { command -v "$1" >/dev/null 2>&1; }

while [ $# -gt 0 ]; do
	case $1 in
	--version | --mirror | --prefix | --local | --sub-store | --http-meta | --root)
		[ $# -ge 2 ] || die "$1 needs a value"
		case $1 in
		--version) VERSION=$2 ;;
		--mirror) MIRROR=$2 ;;
		--prefix) PREFIX=$2 ;;
		--local) LOCAL=$2 ;;
		--sub-store) SUB_STORE=$2 ;;
		--http-meta) HTTP_META=$2 ;;
		--root) ROOT=$2 ;;
		esac
		shift 2
		;;
	--no-start) START=0 && shift ;;
	--no-core) CORE=0 && shift ;;
	--uninstall) ACTION=uninstall && shift ;;
	--purge) PURGE=1 && shift ;;
	-h | --help) usage && exit 0 ;;
	*) die "unknown option: $1 (see --help)" ;;
	esac
done

BIN=$ROOT$PREFIX/bin/singbox-board
CONF=$ROOT/etc/singbox-board/daemon.toml
UNIT=$ROOT/etc/systemd/system/singbox-board.service
OPENRC=$ROOT/etc/init.d/singbox-board
APPS=$ROOT$PREFIX/share/applications
ICONS=$ROOT$PREFIX/share/icons/hicolor/scalable/apps
SOCKET=/run/singbox-board/daemon.sock

if [ -z "$ROOT" ] && [ "$(id -u)" != 0 ]; then
	die "run as root, e.g. curl -fsSL <url>/install.sh | sudo sh"
fi

init_system() {
	if [ -n "$ROOT" ]; then
		echo none
	elif [ -d /run/systemd/system ]; then
		echo systemd
	elif has rc-update && has openrc-run; then
		echo openrc
	else
		echo none
	fi
}

detect_arch() {
	case $(uname -m) in
	x86_64 | amd64) echo amd64 ;;
	aarch64 | arm64) echo arm64 ;;
	armv7* | armv8l) echo armv7 ;;
	i386 | i486 | i586 | i686) echo 386 ;;
	riscv64) echo riscv64 ;;
	loongarch64) echo loong64 ;;
	*) die "unsupported architecture $(uname -m); build from source: cargo build --release" ;;
	esac
}

fetch() { # url dest
	url=$1
	case $url in
	https://github.com/*) [ -n "$MIRROR" ] && url="${MIRROR%/}/$url" ;;
	esac
	if has curl; then
		curl -fL --retry 3 --connect-timeout 15 --progress-bar -o "$2" "$url"
	elif has wget; then
		wget -q -O "$2" "$url"
	else
		die "curl or wget is required"
	fi
}

sha256_of() {
	if has sha256sum; then
		sha256sum "$1" | cut -d' ' -f1
	elif has shasum; then
		shasum -a 256 "$1" | cut -d' ' -f1
	elif has openssl; then
		openssl dgst -sha256 "$1" | awk '{print $NF}'
	else
		die "sha256sum, shasum or openssl is required to verify the download"
	fi
}

# Prints the directory of an extracted release archive.
obtain_release() {
	arch=$(detect_arch)
	name=singbox-board-linux-$arch
	if [ -n "$LOCAL" ]; then
		[ -x "$LOCAL/singbox-board" ] || die "$LOCAL does not contain an extracted $name archive"
		echo "$LOCAL"
		return
	fi
	if [ -n "$VERSION" ]; then
		base=https://github.com/$REPO/releases/download/$VERSION
	else
		base=https://github.com/$REPO/releases/latest/download
	fi
	say "downloading $name.tar.gz (${VERSION:-latest})" >&2
	fetch "$base/$name.tar.gz" "$TMP/$name.tar.gz"
	fetch "$base/SHA256SUMS" "$TMP/SHA256SUMS"
	expected=$(awk -v f="$name.tar.gz" '{n = $2; sub(/^\*/, "", n); if (n == f) print $1}' "$TMP/SHA256SUMS")
	[ -n "$expected" ] || die "SHA256SUMS has no entry for $name.tar.gz"
	actual=$(sha256_of "$TMP/$name.tar.gz")
	[ "$expected" = "$actual" ] || die "checksum mismatch for $name.tar.gz (expected $expected, got $actual)"
	say "checksum verified" >&2
	tar -xzf "$TMP/$name.tar.gz" -C "$TMP"
	echo "$TMP/$name"
}

ensure_group() {
	grep -q "^$GROUP:" /etc/group && return 0
	if has groupadd; then
		groupadd --system "$GROUP"
	elif has addgroup; then
		addgroup -S "$GROUP"
	else
		warn "cannot create group $GROUP; only root can use the dashboard"
		return 0
	fi
	say "created group $GROUP"
}

add_invoking_user() {
	user=${SUDO_USER:-}
	if [ -z "$user" ] || [ "$user" = root ]; then
		return 0
	fi
	if id -nG "$user" 2>/dev/null | tr ' ' '\n' | grep -qx "$GROUP"; then
		return 0
	fi
	if has usermod; then
		usermod -aG "$GROUP" "$user"
	elif has addgroup; then
		addgroup "$user" "$GROUP"
	else
		return 0
	fi
	say "added $user to $GROUP (log in again to use the dashboard without sudo)"
}

install_service() { # release-dir
	case $(init_system) in
	systemd)
		mkdir -p "$(dirname "$UNIT")"
		sed "s|/usr/bin/singbox-board|$PREFIX/bin/singbox-board|" "$1/contrib/singbox-board.service" >"$UNIT"
		systemctl daemon-reload
		if [ "$START" = 1 ]; then
			systemctl enable singbox-board >/dev/null 2>&1
			systemctl restart singbox-board
			say "daemon running (systemctl status singbox-board)"
		fi
		;;
	openrc)
		sed "s|/usr/bin/singbox-board|$PREFIX/bin/singbox-board|" "$1/contrib/singbox-board.openrc" >"$OPENRC"
		chmod 755 "$OPENRC"
		if [ "$START" = 1 ]; then
			rc-update add singbox-board default >/dev/null 2>&1 || true
			rc-service singbox-board restart >/dev/null 2>&1 || rc-service singbox-board start
			say "daemon running (rc-service singbox-board status)"
		fi
		;;
	*)
		[ -n "$ROOT" ] || warn "no systemd or OpenRC found; start the daemon yourself: $PREFIX/bin/singbox-board daemon"
		START=0
		;;
	esac
}

# Desktop entry and icon of `singbox-board tray`; release archives before
# v0.1.7 do not have them.
install_desktop() { # release-dir
	[ -f "$1/contrib/singbox-board.desktop" ] || return 0
	mkdir -p "$APPS" "$ICONS"
	sed "s|/usr/bin/singbox-board|$PREFIX/bin/singbox-board|" "$1/contrib/singbox-board.desktop" >"$APPS/singbox-board.desktop"
	cp "$1/contrib/singbox-board.svg" "$ICONS/singbox-board.svg"
	chmod 644 "$APPS/singbox-board.desktop" "$ICONS/singbox-board.svg"
}

wait_for_daemon() {
	i=0
	while [ $i -lt 20 ]; do
		[ -S "$SOCKET" ] && "$BIN" status >/dev/null 2>&1 && return 0
		sleep 1
		i=$((i + 1))
	done
	return 1
}

# Only ask when our own stdin is a terminal. With `curl ... | sudo sh` stdin is
# the pipe, and /dev/tty may be a pty that sudo (sudo-rs, use_pty) never feeds
# keyboard input into, so reading it would hang and Ctrl-C would not help.
can_prompt() {
	[ -t 0 ] && [ -t 1 ]
}

post_start() {
	wait_for_daemon || {
		warn "the daemon did not come up; check: journalctl -u singbox-board"
		return 0
	}
	if [ "$CORE" = 1 ]; then
		if "$BIN" status --json | grep -q '"core_version": null'; then
			say "installing the sing-box core from MiChongs/sing-box"
			"$BIN" update || warn "sing-box core install failed; retry with: sudo singbox-board update"
		else
			say "sing-box core already installed (upgrade it with: sudo singbox-board update)"
		fi
	fi
	if [ -n "$SUB_STORE$HTTP_META" ]; then
		"$BIN" setup --sub-store "${SUB_STORE:-no}" --http-meta "${HTTP_META:-no}" ||
			warn "component setup failed; retry with: sudo singbox-board setup"
	elif "$BIN" status --json | grep -q '"setup_required": true'; then
		if can_prompt; then
			echo
			"$BIN" setup || warn "component setup failed; retry with: sudo singbox-board setup"
		else
			say "optional components (Sub-Store, http-meta) not chosen yet:"
			echo "      sudo singbox-board setup      # or open the dashboard: singbox-board"
		fi
	fi
}

do_install() {
	if [ -z "$ROOT" ] && [ "$PREFIX" != /usr ] && [ -x /usr/bin/singbox-board ]; then
		die "singbox-board is installed by a package manager (/usr/bin/singbox-board); upgrade it with that instead"
	fi
	TMP=$(mktemp -d)
	trap 'rm -rf "$TMP"' EXIT INT TERM
	release=$(obtain_release)

	mkdir -p "$(dirname "$BIN")"
	cp "$release/singbox-board" "$BIN.new"
	chmod 755 "$BIN.new"
	mv -f "$BIN.new" "$BIN"
	say "installed $BIN ($("$BIN" --version))"

	if [ -f "$CONF" ]; then
		say "keeping existing $CONF"
	else
		mkdir -p "$(dirname "$CONF")"
		"$BIN" daemon --print-default-config >"$CONF"
		if [ -n "$MIRROR" ]; then
			sed -i "s|^# mirror = \"\"|mirror = \"$MIRROR\"|" "$CONF"
		fi
		say "wrote $CONF"
	fi

	if [ -z "$ROOT" ]; then
		ensure_group
		add_invoking_user
	fi
	install_service "$release"
	install_desktop "$release"
	[ "$START" = 1 ] && post_start

	cat <<EOF

${BOLD}singbox-board is installed.${RESET}
  sing-box config     /etc/sing-box/config.json  (then: sudo singbox-board start)
  daemon config       /etc/singbox-board/daemon.toml
  dashboard           singbox-board
  system tray         singbox-board tray  (or "singbox-board" in the application menu)
  status / logs       singbox-board status | singbox-board logs -f
  uninstall           sudo sh install.sh --uninstall   (add --purge to remove data)
EOF
}

do_uninstall() {
	case $(init_system) in
	systemd)
		systemctl disable --now singbox-board >/dev/null 2>&1 || true
		rm -f "$UNIT"
		systemctl daemon-reload
		;;
	openrc)
		rc-service singbox-board stop >/dev/null 2>&1 || true
		rc-update del singbox-board default >/dev/null 2>&1 || true
		rm -f "$OPENRC"
		;;
	esac
	if [ "$PURGE" = 1 ]; then
		core=$(sed -n 's/^binary = "\(.*\)"/\1/p' "$CONF" 2>/dev/null | head -n 1)
		core=${core:-/usr/local/bin/sing-box}
		rm -f "$ROOT$core" "$ROOT$core.bak" "$ROOT$core.new"
		rm -rf "$ROOT/etc/singbox-board" "$ROOT/var/lib/singbox-board"
		if [ -z "$ROOT" ] && grep -q "^$GROUP:" /etc/group; then
			if has groupdel; then groupdel "$GROUP"; elif has delgroup; then delgroup "$GROUP"; fi
		fi
		say "removed configuration, components and $core (kept /etc/sing-box and /var/lib/sing-box)"
	fi
	rm -f "$BIN" "$APPS/singbox-board.desktop" "$ICONS/singbox-board.svg"
	say "singbox-board uninstalled"
}

case $ACTION in
install) do_install ;;
uninstall) do_uninstall ;;
esac
