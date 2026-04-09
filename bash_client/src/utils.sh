#!/usr/bin/env bash
# ==============================================================================
# MELISA UTILITIES MODULE
# Description: Core helper functions, UI rendering, and cryptographic setup.
# ==============================================================================

# --- UI Rendering & ANSI Color Definitions ---
# Exported to ensure availability across all MELISA sub-shells and modules
export BOLD='\e[1m'
export RESET='\e[0m'
export GREEN='\e[32m'
export BLUE='\e[34m'
export CYAN='\e[36m'
export YELLOW='\e[33m'
export RED='\e[31m'

# --- Standardized Logging Interfaces ---
log_info()    { echo -e "${BOLD}${BLUE}[INFO]${RESET} $1"; }
log_success() { echo -e "${BOLD}${GREEN}[SUCCESS]${RESET} $1"; }

# Warnings and Errors are strictly routed to STDERR (>&2) to prevent pipeline corruption
log_warning() { echo -e "${BOLD}${YELLOW}⚠️ [WARNING]${RESET} $1" >&2; }
log_error()   { echo -e "${BOLD}${RED}[ERROR]${RESET} $1" >&2; }

# --- Cryptographic Identity Management ---

# Verifies the existence of a local SSH keypair.
# If none exists, generates a modern, high-security Ed25519 keypair.
ensure_ssh_key() {
    # Check for both modern Ed25519 and legacy RSA keys
    if [ ! -f ~/.ssh/id_ed25519 ] && [ ! -f ~/.ssh/id_rsa ]; then
        log_info "No local SSH identity found. Generating a high-security Ed25519 keypair..."
        
        # Ensure the .ssh directory exists with strict permissions
        mkdir -p ~/.ssh
        chmod 700 ~/.ssh 2>/dev/null
        
        # Generate an Ed25519 key (Modern Enterprise Standard)
        # -t ed25519 : Specifies the modern elliptical curve algorithm
        # -f         : Output file path
        # -N ""      : Empty passphrase (required for automated CLI interactions)
        # -q         : Quiet mode
        if ssh-keygen -t ed25519 -f ~/.ssh/id_ed25519 -N "" -q; then
            log_success "Cryptographic identity (Ed25519) successfully generated."
        else
            log_error "Failed to generate SSH keypair. Check local directory permissions."
            exit 1
        fi
    fi
}