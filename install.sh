#!/bin/sh
# tokenscope installer: downloads a prebuilt static Linux binary from GitHub Releases.
#
#   curl -fsSL https://raw.githubusercontent.com/Cuong102904/tokenscope/main/install.sh | sh
#
# Environment:
#   VERSION      release tag to install, e.g. v0.1.0 (default: latest)
#   INSTALL_DIR  where to put the binary (default: ~/.local/bin)
set -eu

REPO="Cuong102904/tokenscope"
INSTALL_DIR="${INSTALL_DIR:-$HOME/.local/bin}"
VERSION="${VERSION:-latest}"

err() {
    echo "tokenscope install: $*" >&2
    exit 1
}

[ "$(uname -s)" = Linux ] || err "only Linux is supported (this is $(uname -s))"
case "$(uname -m)" in
    x86_64 | amd64) target=x86_64-unknown-linux-musl ;;
    aarch64 | arm64) target=aarch64-unknown-linux-musl ;;
    *) err "unsupported architecture: $(uname -m)" ;;
esac
for cmd in curl tar sha256sum; do
    command -v "$cmd" >/dev/null 2>&1 || err "$cmd is required"
done

if [ "$VERSION" = latest ]; then
    url=$(curl -fsSLI -o /dev/null -w '%{url_effective}' "https://github.com/$REPO/releases/latest") ||
        err "could not reach GitHub"
    VERSION=${url##*/}
    case "$VERSION" in v*) ;; *) err "no release found" ;; esac
fi

name="tokenscope-$VERSION-$target"
base="https://github.com/$REPO/releases/download/$VERSION"
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT

echo "Downloading tokenscope $VERSION ($target)..."
curl -fsSL -o "$tmp/$name.tar.gz" "$base/$name.tar.gz" || err "download failed: $base/$name.tar.gz"
curl -fsSL -o "$tmp/$name.tar.gz.sha256" "$base/$name.tar.gz.sha256" || err "checksum download failed"
(cd "$tmp" && sha256sum -c "$name.tar.gz.sha256" >/dev/null 2>&1) || err "checksum mismatch, not installing"
tar -xzf "$tmp/$name.tar.gz" -C "$tmp"

mkdir -p "$INSTALL_DIR"
install -m 755 "$tmp/$name/tokenscope" "$INSTALL_DIR/tokenscope"
echo "Installed $("$INSTALL_DIR/tokenscope" --version) to $INSTALL_DIR/tokenscope"

case ":$PATH:" in
    *":$INSTALL_DIR:"*) ;;
    *) echo "Note: $INSTALL_DIR is not on your PATH. Add this to your shell profile:"
       echo "  export PATH=\"$INSTALL_DIR:\$PATH\"" ;;
esac
echo "Next: tokenscope doctor && tokenscope serve"
