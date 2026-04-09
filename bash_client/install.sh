#!/usr/bin/env bash
# ==============================================================================
# MELISA MODULAR CLIENT INSTALLER
# Description: Deploys the MELISA client, configures system directories,
#              and registers the executable to the user's PATH.
# ==============================================================================

# Enforce strict execution (stop script if any unhandled command fails)
set -e

# --- Visual Rendering ---
BOLD="\e[1m"
RESET="\e[0m"
GREEN="\e[32m"
CYAN="\e[36m"
RED="\e[31m"
YELLOW="\e[33m"

echo -e "${BOLD}${CYAN}Initializing MELISA Client Deployment...${RESET}\n"

# 1. Path Resolution: Ensure we are copying from the correct source directory
# This allows the installer to be safely executed from any location
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
SRC_DIR="$SCRIPT_DIR/src"

if [ ! -d "$SRC_DIR" ]; then
    echo -e "${RED}[FATAL] Source directory 'src/' not found at $SCRIPT_DIR.${RESET}"
    echo -e "Please ensure you are executing this installer from the repository root."
    exit 1
fi

# 2. Provision Local System Directories
echo -e "  [+] Provisioning local system directories..."
mkdir -p ~/.local/bin
mkdir -p ~/.local/share/melisa
mkdir -p ~/.config/melisa

# 2.5 Resolve Potential Permission Conflicts (Auto-Fix)
# Reclaim ownership of target directories just in case they were locked by root previously
echo -e "  [+] Sanitizing directory ownership (sudo may prompt for password)..."
sudo chown -R "$USER":"$USER" ~/.local/bin ~/.local/share/melisa ~/.config/melisa

# 3. Deploy Source Files
echo -e "  [+] Deploying core binaries and modules..."

# Validate that the main executable exists before attempting to copy
if [ ! -f "$SRC_DIR/melisa" ]; then
    echo -e "${RED}[FATAL] Core executable 'src/melisa' is missing.${RESET}"
    exit 1
fi

cp -f "$SRC_DIR/melisa" ~/.local/bin/melisa

# [CRITICAL FIX]: Safe Iteration for .sh modules without hiding errors
SH_FOUND=false
for file in "$SRC_DIR"/*.sh; do
    # Check if the glob actually matched a file
    if [ -f "$file" ]; then
        SH_FOUND=true
        # Explicitly copy and catch error without hiding stderr
        cp -f "$file" ~/.local/share/melisa/ || echo -e "  ${RED}[ERROR] Failed to copy $(basename "$file"). Check folder permissions!${RESET}"
    fi
done

if [ "$SH_FOUND" = false ]; then
    echo -e "  ${YELLOW}[WARN] No supplementary .sh modules found to copy.${RESET}"
fi

# 4. Enforce Execution Permissions
echo -e "  [+] Setting strict execution permissions..."
chmod +x ~/.local/bin/melisa
chmod +x ~/.local/share/melisa/*.sh 2>/dev/null || true # Ensure modules are executable too

# 5. Dynamic PATH Registration (Multi-Shell Support)
echo -e "  [+] Verifying PATH environment variables..."

# Detect the user's active shell configuration file
SHELL_RC=""
if [[ "$SHELL" == *"zsh"* ]]; then
    SHELL_RC="$HOME/.zshrc"
elif [[ "$SHELL" == *"bash"* ]]; then
    SHELL_RC="$HOME/.bashrc"
else
    SHELL_RC="$HOME/.profile"
fi

RELOAD_NEEDED=false

# Check if ~/.local/bin is already present in the system PATH
if [[ ":$PATH:" != *":$HOME/.local/bin:"* ]]; then
    # Check if the export command already exists in the file to prevent duplicates
    if ! grep -q 'export PATH="$HOME/.local/bin:$PATH"' "$SHELL_RC" 2>/dev/null; then
        echo -e "  ${YELLOW}[INFO] Registering ~/.local/bin to your PATH in $(basename "$SHELL_RC")...${RESET}"
        echo -e '\n# MELISA Client Environment' >> "$SHELL_RC"
        echo 'export PATH="$HOME/.local/bin:$PATH"' >> "$SHELL_RC"
        RELOAD_NEEDED=true
    else
        echo -e "  [v] PATH registration already exists in $(basename "$SHELL_RC"), but requires reloading."
        RELOAD_NEEDED=true
    fi
    
    # Temporarily apply to the current session so the rest of the script/user can use it immediately
    export PATH="$HOME/.local/bin:$PATH"
else
    echo -e "  [v] System PATH is already configured correctly."
fi

echo -e "\n${BOLD}${GREEN}[SUCCESS] MELISA Client has been successfully deployed!${RESET}"

# Provide clear instructions if the user needs to refresh their terminal
if [ "$RELOAD_NEEDED" = true ]; then
    echo -e "\n${YELLOW}IMPORTANT: You must reload your shell to apply the new PATH.${RESET}"
    echo -e "Please execute: ${BOLD}source $SHELL_RC${RESET}"
fi

echo -e "Execute ${BOLD}melisa --help${RESET} to initialize your first connection."