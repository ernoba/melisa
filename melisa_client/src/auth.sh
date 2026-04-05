#!/usr/bin/env bash
# ==============================================================================
# MELISA AUTHENTICATION & CONNECTION MANAGER
# Description: Handles remote server profiles, active session state, 
#              and SSH multiplexing for low-latency command execution.
# ==============================================================================

CONFIG_DIR="$HOME/.config/melisa"
PROFILE_FILE="$CONFIG_DIR/profiles.conf"
ACTIVE_FILE="$CONFIG_DIR/active"

# ------------------------------------------------------------------------------
# INITIALIZATION
# ------------------------------------------------------------------------------

# Initializes the local configuration directory and profile storage.
# Enforces strict permissions to prevent unauthorized access to server lists.
init_auth() {
    mkdir -p "$CONFIG_DIR"
    chmod 700 "$CONFIG_DIR" 2>/dev/null
    touch "$PROFILE_FILE"
    chmod 600 "$PROFILE_FILE" 2>/dev/null
}


# ------------------------------------------------------------------------------
# CORE GETTERS
# ------------------------------------------------------------------------------

# Retrieves the connection string (user@host) for the currently active profile.
# Returns 1 (failure) silently if no active profile is set.
get_active_conn() {
    # Fail silently if the active state file is missing
    if [ ! -f "$ACTIVE_FILE" ]; then return 1; fi

    local active
    active=$(cat "$ACTIVE_FILE")

    # Extract the full stored value for the active profile name
    local entry
    entry=$(grep "^${active}=" "$PROFILE_FILE" | cut -d'=' -f2)

    # FIX: Strip the "|melisa_user" suffix — return ONLY the "user@host" part
    local conn
    conn=$(echo "$entry" | cut -d'|' -f1)

    # Fail silently if the profile exists in the active file but not in the config
    if [ -z "$conn" ]; then return 1; fi

    echo "$conn"
}

get_remote_user() {
    if [ ! -f "$ACTIVE_FILE" ]; then return 1; fi
    local active
    active=$(cat "$ACTIVE_FILE" 2>/dev/null)

    # Baca seluruh value dari profil (format: root@host|alice)
    local raw
    raw=$(grep "^${active}=" "$PROFILE_FILE" | cut -d'=' -f2)

    # Ekstrak bagian setelah "|" — ini adalah melisa username
    # Jika tidak ada "|", hasilnya kosong (profil lama yang belum diperbarui)
    echo "$raw" | cut -s -d'|' -f2
}

get_active_melisa_user() {
    if [ ! -f "$ACTIVE_FILE" ]; then return 1; fi

    local active
    active=$(cat "$ACTIVE_FILE")

    local entry
    entry=$(grep "^${active}=" "$PROFILE_FILE" | cut -d'=' -f2)

    if [ -z "$entry" ]; then return 1; fi

    # Extract the part after '|' as the MELISA username
    local melisa_user
    melisa_user=$(echo "$entry" | cut -d'|' -f2)

    if [ -z "$melisa_user" ] || [ "$melisa_user" = "$entry" ]; then
        # No melisa_user was stored — fall back to the SSH login username
        echo "$entry" | cut -d'@' -f1
    else
        echo "$melisa_user"
    fi
}

# ------------------------------------------------------------------------------
# PROFILE MANAGEMENT
# ------------------------------------------------------------------------------

# Registers a new remote server profile and configures SSH multiplexing.
# Stores the MELISA application username alongside the SSH connection string
# using a pipe-delimited format: name=user@host|melisa_user
auth_add() {
    local name=$1
    local user_host=$2 # Expected format: user@192.168.1.10

    if [ -z "$name" ] || [ -z "$user_host" ]; then
        log_error "Usage: melisa auth add <profile_name> <user@host>"
        exit 1
    fi

    # Ensure a local SSH keypair exists before attempting to copy it
    ensure_ssh_key

    log_info "Deploying public SSH key to ${BOLD}${user_host}${RESET}..."
    log_info "Please prepare to enter the remote server password."

    # Attempt to copy the SSH ID. Abort if the connection or authentication fails.
    ssh-copy-id "$user_host" || { log_error "Failed to establish a connection to the remote server."; exit 1; }

    # Setup Automatic SSH Multiplexing (ControlMaster)
    local host
    host=$(echo "$user_host" | cut -d'@' -f2)
    local ssh_user
    ssh_user=$(echo "$user_host" | cut -d'@' -f1)

    mkdir -p ~/.ssh/sockets
    chmod 700 ~/.ssh ~/.ssh/sockets 2>/dev/null
    touch ~/.ssh/config
    chmod 600 ~/.ssh/config 2>/dev/null

    if ! grep -q "Host $host" ~/.ssh/config 2>/dev/null; then
        cat <<EOF >> ~/.ssh/config

Host $host
    User $ssh_user
    ControlMaster auto
    ControlPath ~/.ssh/sockets/%r@%h:%p
    ControlPersist 10m
EOF
    fi

    # Prompt for the MELISA application username on the remote server.
    # This may differ from the SSH login user (e.g., SSH as root but MELISA user is "melisa").
    local melisa_user
    read -rp "$(echo -e "[SETUP] Enter your MELISA username on this server (leave blank if same as SSH user): ")" melisa_user

    # Default to the SSH user if left blank
    if [ -z "$melisa_user" ]; then
        melisa_user="$ssh_user"
    fi

    # Store the profile as: name=user@host|melisa_user
    # This keeps SSH connection and MELISA identity cleanly separated.
    if [ -f "$PROFILE_FILE" ]; then
        grep -v "^${name}=" "$PROFILE_FILE" > "${PROFILE_FILE}.tmp"
        mv "${PROFILE_FILE}.tmp" "$PROFILE_FILE"
    fi

    echo "${name}=${user_host}|${melisa_user}" >> "$PROFILE_FILE"
    echo "$name" > "$ACTIVE_FILE"

    log_success "Server profile '${name}' registered. Remote MELISA user: ${melisa_user}"
}

# Safely removes an existing server profile from the local configuration.
auth_remove() {
    local name=$1

    if [ -z "$name" ]; then
        log_error "Usage: melisa auth remove <profile_name>"
        return 1
    fi

    if ! grep -q "^${name}=" "$PROFILE_FILE"; then
        log_error "Server profile '${name}' was not found in the registry."
        return 1
    fi

    read -rp "$(echo -e "${YELLOW}Are you sure you want to permanently remove the profile '${name}'? (y/N): ${RESET}")" confirm
    if [[ ! "$confirm" =~ ^[Yy]$ ]]; then
        log_info "Profile deletion aborted by user."
        return 0
    fi

    grep -v "^${name}=" "$PROFILE_FILE" > "${PROFILE_FILE}.tmp"
    mv "${PROFILE_FILE}.tmp" "$PROFILE_FILE"

    local active
    active=$(cat "$ACTIVE_FILE" 2>/dev/null)
    if [ "$name" == "$active" ]; then
        rm -f "$ACTIVE_FILE"
        log_info "The active profile was deleted. Please use 'melisa auth switch' to select a new server."
    fi

    log_success "Server profile '${name}' has been successfully purged from the registry."
}

# Switches the active connection context to a different registered server.
auth_switch() {
    local name=$1

    if [ -z "$name" ]; then
        log_error "Usage: melisa auth switch <profile_name>"
        return 1
    fi

    if grep -q "^${name}=" "$PROFILE_FILE"; then
        echo "$name" > "$ACTIVE_FILE"
        log_success "Successfully switched active connection to server: ${BOLD}${name}${RESET}"
    else
        log_error "Server profile '${name}' not found! Execute 'melisa auth list' to view available profiles."
    fi
}

# Displays an enumerated list of all registered remote servers.
auth_list() {
    local active
    active=$(cat "$ACTIVE_FILE" 2>/dev/null)

    echo -e "\n${BOLD}${CYAN}=== MELISA REMOTE SERVER REGISTRY ===${RESET}"

    if [ ! -s "$PROFILE_FILE" ]; then
        echo "No servers are currently registered. Add one using 'melisa auth add <n> <user@host>'."
        return
    fi

    while IFS='=' read -r name stored_val; do
        if [ -z "$name" ]; then continue; fi

        # Parse the stored "user@host|melisa_user" format for clean display
        local conn melisa_user
        conn=$(echo "$stored_val" | cut -d'|' -f1)
        melisa_user=$(echo "$stored_val" | cut -d'|' -f2)

        # If no pipe separator, melisa_user equals conn — show nothing extra
        if [ "$melisa_user" = "$conn" ]; then
            melisa_user=""
        fi

        local melisa_tag=""
        [ -n "$melisa_user" ] && melisa_tag=" [melisa: ${melisa_user}]"

        if [ "$name" == "$active" ]; then
            echo -e "  ${GREEN}* ${name}${RESET} \t(${conn})${melisa_tag} ${YELLOW}<- [ACTIVE]${RESET}"
        else
            echo -e "    ${name} \t(${conn})${melisa_tag}"
        fi
    done < "$PROFILE_FILE"
    echo ""
}