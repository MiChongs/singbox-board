#!/usr/bin/env bash
# Builds the release archive and the deb/rpm/apk/Arch packages for one target
# from an already compiled binary.
#
#   packaging/package.sh <rust-target> <arch> <nfpm-arch> <version> [out-dir]
#   packaging/package.sh x86_64-unknown-linux-musl amd64 amd64 0.1.0 dist
#
# <arch> names the archive (singbox-board-linux-<arch>.tar.gz) the way
# install.sh expects; <nfpm-arch> is the Go-style name nfpm understands.
set -euo pipefail

if [[ $# -lt 4 ]]; then
	sed -n '2,9p' "$0" >&2
	exit 2
fi
target=$1 arch=$2 nfpm_arch=$3 version=$4 out=${5:-dist}

root=$(cd "$(dirname "$0")/.." && pwd)
cd "$root"
binary="target/$target/release/singbox-board"
[[ -x $binary ]] || {
	echo "missing $binary; run: cargo zigbuild --release --target $target" >&2
	exit 1
}
mkdir -p "$out"

# Version-less archive name so .../releases/latest/download/<name> always works.
name="singbox-board-linux-$arch"
stage=$(mktemp -d)
trap 'rm -rf "$stage"' EXIT
mkdir -p "$stage/$name/contrib"
cp "$binary" "$stage/$name/singbox-board"
cp README.md install.sh "$stage/$name/"
cp contrib/daemon.toml contrib/singbox-board.service contrib/singbox-board.openrc "$stage/$name/contrib/"
printf '%s\n' "$version" >"$stage/$name/VERSION"
chmod -R u=rwX,go=rX "$stage/$name"
chmod 755 "$stage/$name/singbox-board" "$stage/$name/install.sh"
tar -C "$stage" --owner=0 --group=0 --numeric-owner -czf "$out/$name.tar.gz" "$name"
echo "$out/$name.tar.gz"

# nfpm does not expand variables in file sources, so fill in the binary here.
sed "s|@BINARY@|$binary|" packaging/nfpm.yaml >"$stage/nfpm.yaml"
for packager in deb rpm apk archlinux; do
	VERSION=$version ARCH=$nfpm_arch \
		nfpm package --config "$stage/nfpm.yaml" --packager "$packager" --target "$out/" |
		sed -n 's/^.*created package: //p'
done
