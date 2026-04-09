#!/usr/bin/env bash
# ==============================================================================
# MELISA CLIENT (Rust) — Installer for Linux and macOS
# ==============================================================================
# Installs the melisa-client binary compiled from Rust source.
# Supports: Ubuntu/Debian, Fedora/RHEL/Arch/Alpine, macOS (Homebrew or manual).
#
# Usage:
#   ./install.sh            — auto-detect and install everything
#   ./install.sh --no-build — skip compilation, install pre-built binary only
# ==============================================================================

set -o pipefail

BOLD="\e[1m"
RESET="\e[0m"
GREEN="\e[32m"
CYAN="\e[36m"
RED="\e[31m"
YELLOW="\e[33m"

log_info()    { echo -e "  ${CYAN}[+]${RESET} $1"; }
log_success() { echo -e "  ${GREEN}[v]${RESET} ${BOLD}$1${RESET}"; }
log_warning() { echo -e "  ${YELLOW}[!]${RESET} $1"; }
log_error()   { echo -e "  ${RED}[ERROR]${RESET} $1" >&2; }
log_fatal()   { echo -e "\n${RED}[FATAL]${RESET} $1" >&2; exit 1; }

echo -e "\n${BOLD}${CYAN}╔═══════════════════════════════════════════╗${RESET}"
echo -e "${BOLD}${CYAN}║    MELISA CLIENT — RUST INSTALLER         ║${RESET}"
echo -e "${BOLD}${CYAN}╚═══════════════════════════════════════════╝${RESET}\n"

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
BUILD_FLAG=true
for arg in "$@"; do
    [[ "$arg" == "--no-build" ]] && BUILD_FLAG=false
done

# ── Step 1: Detect operating system ──────────────────────────────────────────

detect_os() {
    if [[ "$OSTYPE" == "darwin"* ]]; then
        echo "macos"
    elif [[ -f /etc/os-release ]]; then
        # shellcheck source=/dev/null
        source /etc/os-release
        echo "${ID:-linux}"
    else
        echo "linux"
    fi
}
OS=$(detect_os)
log_info "Detected OS: ${BOLD}${OS}${RESET}"

# ── Step 2: Install OpenSSH client if missing ─────────────────────────────────

log_info "Verifying OpenSSH client installation..."
if ! command -v ssh >/dev/null 2>&1; then
    log_warning "OpenSSH client not found. Installing..."
    case "$OS" in
        ubuntu|debian|linuxmint|raspbian)
            sudo apt-get update -qq && sudo apt-get install -y openssh-client ;;
        fedora|rhel|centos|rocky|alma)
            sudo dnf install -y openssh-clients ;;
        arch|manjaro)
            sudo pacman -S --noconfirm openssh ;;
        alpine)
            sudo apk add --no-cache openssh-client ;;
        macos)
            # OpenSSH is bundled with macOS; if absent, install via Homebrew
            if command -v brew >/dev/null 2>&1; then
                brew install openssh
            else
                log_fatal "OpenSSH is missing and Homebrew is not installed. Please install OpenSSH manually."
            fi ;;
        *)
            log_fatal "Cannot auto-install OpenSSH on '${OS}'. Please install it manually." ;;
    esac
    log_success "OpenSSH client installed."
else
    log_success "OpenSSH client is already present."
fi

# ── Step 3: Install Rust / Cargo if needed ────────────────────────────────────

if [[ "$BUILD_FLAG" == "true" ]]; then
    log_info "Verifying Rust toolchain..."
    if ! command -v cargo >/dev/null 2>&1; then
        log_warning "Rust toolchain not found. Installing via rustup..."
        curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --no-modify-path
        # Make cargo available in this session
        # shellcheck source=/dev/null
        source "$HOME/.cargo/env"
        log_success "Rust toolchain installed."
    else
        log_success "Rust toolchain is already present ($(cargo --version))."
    fi
fi

# ── Step 4: Compile the binary ────────────────────────────────────────────────

if [[ "$BUILD_FLAG" == "true" ]]; then
    log_info "Compiling melisa-client (release build)..."
    if [[ ! -f "$SCRIPT_DIR/Cargo.toml" ]]; then
        log_fatal "Cargo.toml not found at '$SCRIPT_DIR'. Run this installer from the repository root."
    fi
    (
        cd "$SCRIPT_DIR"
        cargo build --release 2>&1
    ) || log_fatal "Compilation failed. Check the error output above."
    BINARY="$SCRIPT_DIR/target/release/melisa"
    log_success "Compilation completed: ${BINARY}"
else
    # Look for a pre-built binary in the script directory
    BINARY="$SCRIPT_DIR/melisa"
    if [[ ! -f "$BINARY" ]]; then
        log_fatal "Pre-built binary not found at '$BINARY'. Run without --no-build to compile from source."
    fi
fi

# ── Step 5: Install system directories ───────────────────────────────────────

log_info "Provisioning local system directories..."
mkdir -p ~/.local/bin
mkdir -p ~/.local/share/melisa
mkdir -p ~/.config/melisa
chmod 700 ~/.config/melisa 2>/dev/null || true

# ── Step 6: Deploy the compiled binary ───────────────────────────────────────

log_info "Deploying binary to ~/.local/bin/melisa ..."
cp -f "$BINARY" ~/.local/bin/melisa
chmod +x ~/.local/bin/melisa
log_success "Binary deployed."

# ── Step 7: Register ~/.local/bin in PATH ────────────────────────────────────

log_info "Verifying PATH configuration..."
SHELL_RC=""
case "$SHELL" in
    */zsh)  SHELL_RC="$HOME/.zshrc" ;;
    */bash) SHELL_RC="$HOME/.bashrc" ;;
    *)      SHELL_RC="$HOME/.profile" ;;
esac

RELOAD_NEEDED=false
if [[ ":$PATH:" != *":$HOME/.local/bin:"* ]]; then
    if ! grep -q 'export PATH="$HOME/.local/bin:$PATH"' "$SHELL_RC" 2>/dev/null; then
        echo -e '\n# MELISA Client Environment' >> "$SHELL_RC"
        echo 'export PATH="$HOME/.local/bin:$PATH"' >> "$SHELL_RC"
        log_info "Registered ~/.local/bin in $(basename "$SHELL_RC")."
        RELOAD_NEEDED=true
    else
        RELOAD_NEEDED=true
    fi
    export PATH="$HOME/.local/bin:$PATH"
else
    log_success "System PATH is already correctly configured."
fi

# ── Step 8: Verify installation ───────────────────────────────────────────────

log_info "Verifying installation..."
if command -v melisa >/dev/null 2>&1; then
    log_success "melisa is accessible in PATH."
else
    log_warning "melisa binary was installed but is not yet in PATH for this session."
fi

echo -e "\n${BOLD}${GREEN}[SUCCESS] MELISA Client (Rust) has been successfully deployed!${RESET}"

if [[ "$RELOAD_NEEDED" == "true" ]]; then
    echo -e "\n${YELLOW}IMPORTANT: You must reload your shell to apply the new PATH.${RESET}"
    echo -e "Execute: ${BOLD}source ${SHELL_RC}${RESET}"
fi

echo -e "\nRun ${BOLD}melisa auth add <name> <user@ip>${RESET} to register your first server."