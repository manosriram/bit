#!/bin/sh
# Installs the latest bit release, and ripgrep if it's missing.
#
#   curl -fsSL https://raw.githubusercontent.com/manosriram/bit/main/install.sh | sh
#
# BIT_VERSION=v0.0.1   install that release instead of the latest
# BIT_INSTALL_DIR=dir  where bit goes (default ~/.local/bin)
# BIT_RELEASES=url     where releases live (default https://github.com/manosriram/bit/releases)
set -eu

releases=${BIT_RELEASES:-https://github.com/manosriram/bit/releases}
dir=${BIT_INSTALL_DIR:-$HOME/.local/bin}

say() { printf 'bit: %s\n' "$*"; }
die() { printf 'bit: error: %s\n' "$*" >&2; exit 1; }
has() { command -v "$1" >/dev/null 2>&1; }

# Runs a command as root, through sudo when we aren't root already.
root() {
	if [ "$(id -u)" -eq 0 ]; then "$@"
	elif has sudo; then sudo "$@"
	else die "need root to run: $*"
	fi
}

case "$(uname -s)-$(uname -m)" in
	Darwin-arm64) target=aarch64-apple-darwin ;;
	Darwin-x86_64) target=x86_64-apple-darwin ;;
	Linux-x86_64) target=x86_64-unknown-linux-gnu ;;
	Linux-aarch64 | Linux-arm64) target=aarch64-unknown-linux-gnu ;;
	*) die "no release for $(uname -s) $(uname -m); build from source: cargo install --path crates/cmd" ;;
esac

# Dependencies. curl and tar are needed to install anything, so they can't be installed here.
for tool in curl tar; do
	has "$tool" || die "$tool is required; install it and run this again"
done
if has rg; then
	say "ripgrep found"
else
	say "installing ripgrep (bit uses it to search code)"
	if has brew; then brew install ripgrep
	elif has apt-get; then root apt-get update -qq && root apt-get install -y ripgrep
	elif has dnf; then root dnf install -y ripgrep
	elif has pacman; then root pacman -S --noconfirm ripgrep
	elif has zypper; then root zypper --non-interactive install ripgrep
	elif has apk; then root apk add ripgrep
	else die "no known package manager; install ripgrep (https://github.com/BurntSushi/ripgrep) and run this again"
	fi
	has rg || die "ripgrep didn't install; install it and run this again"
fi

# The latest release, from where GitHub redirects /releases/latest.
version=${BIT_VERSION:-}
if [ -z "$version" ]; then
	url=$(curl -fsSLI -o /dev/null -w '%{url_effective}' "$releases/latest") || die "can't reach $releases"
	version=${url##*/}
	case "$version" in v*) ;; *) die "no release found at $releases" ;; esac
fi

name="bit-$version-$target"
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT
say "downloading $name"
curl -fsSL -o "$tmp/$name.tar.gz" "$releases/download/$version/$name.tar.gz" || die "download failed: $name.tar.gz"
curl -fsSL -o "$tmp/$name.tar.gz.sha256" "$releases/download/$version/$name.tar.gz.sha256" || die "download failed: $name.tar.gz.sha256"

want=$(cut -d' ' -f1 "$tmp/$name.tar.gz.sha256")
if has sha256sum; then got=$(sha256sum "$tmp/$name.tar.gz" | cut -d' ' -f1)
else got=$(shasum -a 256 "$tmp/$name.tar.gz" | cut -d' ' -f1)
fi
[ "$want" = "$got" ] || die "checksum mismatch for $name.tar.gz"

tar -xzf "$tmp/$name.tar.gz" -C "$tmp"
mkdir -p "$dir"
mv "$tmp/$name/bit" "$dir/bit"
chmod +x "$dir/bit"
say "installed $("$dir/bit" --version) to $dir/bit"

case ":$PATH:" in
	*":$dir:"*) ;;
	*)
		case "${SHELL:-}" in
			*zsh) rc=~/.zshrc ;;
			*bash) rc=~/.bashrc ;;
			*) rc=~/.profile ;;
		esac
		say "add $dir to your PATH: echo 'export PATH=\"$dir:\$PATH\"' >> $rc"
		;;
esac
