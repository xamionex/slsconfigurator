#!/usr/bin/env bash
# Installer for SLSconfigurator.
#
# Installs the latest release binary into ~/.local/bin.
#
# Usage: curl -fsSL https://raw.githubusercontent.com/xamionex/slsconfigurator/main/install.sh | sh
#
# SLSCONFIGURATOR_URL overrides the download URL (testing, forks).
set -eu

REPO="xamionex/slsconfigurator"
BIN_NAME="slsconfigurator"
RELEASE_PAGE="https://github.com/$REPO/releases/latest"
DOWNLOAD_BASE="https://github.com/$REPO/releases/download"
VERSION=""

info() { printf '%s\n' "==> $*"; }
warn() { printf '%s\n' "!! $*" >&2; }
die()  { printf '%s\n' "!! $*" >&2; exit 1; }

if [ "${1:-}" = "--help" ] || [ "${1:-}" = "-h" ]; then
    cat <<EOF
Usage: curl -fsSL https://raw.githubusercontent.com/$REPO/main/install.sh | sh

Downloads the latest $BIN_NAME release and installs it to ~/.local/bin.

Environment:
  SLSCONFIGURATOR_URL     override the download URL
EOF
    exit 0
elif [ -n "${1:-}" ]; then
    die "unknown argument: $1 (this installer only installs for the current user)"
fi

BIN_DIR="$HOME/.local/bin"

command -v curl >/dev/null 2>&1 || die "curl is required but not installed"
command -v mktemp >/dev/null 2>&1 || die "mktemp is required but not installed"

# The release page redirect ends in the tag, so the version is known before the download.
resolve_version() {
    page="$(curl -fsSIL -o /dev/null -w '%{url_effective}' "$RELEASE_PAGE" 2>/dev/null || true)"
    page="${page%%\?*}"
    page="${page%/}"
    tag="${page##*/}"
    case "$tag" in
        v[0-9]*) printf '%s\n' "$tag" ;;
        *) return 1 ;;
    esac
}

if [ -n "${SLSCONFIGURATOR_URL:-}" ]; then
    URL="$SLSCONFIGURATOR_URL"
    info "using SLSCONFIGURATOR_URL override"
else
    VERSION="$(resolve_version || true)"
    if [ -n "$VERSION" ]; then
        URL="$DOWNLOAD_BASE/$VERSION/$BIN_NAME"
        info "latest release is $VERSION"
    else
        URL="$RELEASE_PAGE/download/$BIN_NAME"
        warn "could not determine the latest release version"
    fi
fi
info "downloading $BIN_NAME${VERSION:+ $VERSION} from $URL"

TMP_DIR="$(mktemp -d)"
trap 'rm -rf "$TMP_DIR"' EXIT
curl -fsSL -o "$TMP_DIR/$BIN_NAME" "$URL" \
    || die "download failed; is there a release with a '$BIN_NAME' asset yet?"

# Sanity check: the asset must be an executable ELF binary.
head -c 4 "$TMP_DIR/$BIN_NAME" | grep -q "$(printf '\177ELF')" || die "downloaded file is not an ELF binary"

chmod 755 "$TMP_DIR/$BIN_NAME"
info "installing $BIN_NAME${VERSION:+ $VERSION} to $BIN_DIR/$BIN_NAME"
mkdir -p "$BIN_DIR"
install -m 755 "$TMP_DIR/$BIN_NAME" "$BIN_DIR/$BIN_NAME"

info "done"
info "run 'slsconfigurator' to edit \$XDG_CONFIG_HOME/SLSsteam/config.yaml (or ~/.config/SLSsteam/config.yaml)"
case ":$PATH:" in
    *":$BIN_DIR:"*) ;;
    *) warn "$BIN_DIR is not on your PATH; add it before running slsconfigurator" ;;
esac
