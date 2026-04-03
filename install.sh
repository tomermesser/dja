#!/usr/bin/env sh
# dja installer — https://github.com/tomermesser/dja
# Usage: curl -fsSL https://raw.githubusercontent.com/tomermesser/dja/main/install.sh | sh
set -e

REPO="tomermesser/dja"
BINARY_NAME="dja"
INSTALL_DIR="${DJA_INSTALL_DIR:-$HOME/.local/bin}"

GREEN='\033[0;32m'
YELLOW='\033[1;33m'
RED='\033[0;31m'
NC='\033[0m'

info()  { printf "${GREEN}[dja]${NC} %s\n" "$1"; }
warn()  { printf "${YELLOW}[dja]${NC} %s\n" "$1"; }
error() { printf "${RED}[dja]${NC} %s\n" "$1"; exit 1; }

detect_os() {
    case "$(uname -s)" in
        Linux*)  OS="linux";;
        Darwin*) OS="darwin";;
        *) error "Unsupported OS: $(uname -s)";;
    esac
}

detect_arch() {
    case "$(uname -m)" in
        x86_64|amd64)  ARCH="x86_64";;
        arm64|aarch64) ARCH="aarch64";;
        *) error "Unsupported architecture: $(uname -m)";;
    esac
}

get_target() {
    case "$OS" in
        darwin)
            case "$ARCH" in
                aarch64) TARGET="aarch64-apple-darwin";;
                *) error "Only Apple Silicon (arm64) Macs are supported. Intel Macs are not supported.";;
            esac
            ;;
        linux)
            case "$ARCH" in
                x86_64)  TARGET="x86_64-unknown-linux-gnu";;
                *) error "Only x86_64 Linux is supported. For ARM Linux, build from source: cargo install --path .";;
            esac
            ;;
    esac
}

get_latest_version() {
    VERSION=$(curl -fsSL "https://api.github.com/repos/${REPO}/releases/latest" \
        | grep '"tag_name":' | sed -E 's/.*"([^"]+)".*/\1/')
    if [ -z "$VERSION" ]; then
        error "Could not fetch latest release version"
    fi
}

install_binary() {
    DOWNLOAD_URL="https://github.com/${REPO}/releases/download/${VERSION}/dja-${TARGET}"
    TEMP_DIR=$(mktemp -d)

    info "Downloading dja ${VERSION} for ${TARGET}..."
    if ! curl -fsSL "$DOWNLOAD_URL" -o "${TEMP_DIR}/dja"; then
        error "Download failed from: $DOWNLOAD_URL"
    fi

    mkdir -p "$INSTALL_DIR"
    mv "${TEMP_DIR}/dja" "${INSTALL_DIR}/dja"
    chmod +x "${INSTALL_DIR}/dja"
    rm -rf "$TEMP_DIR"

    info "Installed to ${INSTALL_DIR}/dja"
}

ensure_in_path() {
    case ":$PATH:" in
        *":${INSTALL_DIR}:"*) ;;
        *)
            SHELL_RC=""
            case "$SHELL" in
                */zsh)  SHELL_RC="$HOME/.zshrc";;
                */bash) SHELL_RC="$HOME/.bashrc";;
            esac
            if [ -n "$SHELL_RC" ] && ! grep -q "$INSTALL_DIR" "$SHELL_RC" 2>/dev/null; then
                printf '\nexport PATH="%s:$PATH"\n' "$INSTALL_DIR" >> "$SHELL_RC"
                warn "Added ${INSTALL_DIR} to PATH in ${SHELL_RC}"
                warn "Run: source ${SHELL_RC}"
            fi
            export PATH="${INSTALL_DIR}:$PATH"
            ;;
    esac
}

main() {
    info "Installing dja..."
    detect_os
    detect_arch
    get_target
    get_latest_version
    install_binary
    ensure_in_path

    info "dja ${VERSION} installed successfully!"
    echo ""
    info "Run setup:"
    info "  dja init"
    echo ""
}

main
