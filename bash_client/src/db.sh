#!/usr/bin/env bash
# ==============================================================================
# MELISA LOCAL STATE REGISTRY (DB)
# Description: Manages the mapping between project names and local file paths.
#              Ensures context-aware command execution based on the working directory.
# ==============================================================================

# Define the local database architecture
DB_DIR="$HOME/.config/melisa"
DB_PATH="$DB_DIR/registry"

# Initialize the registry with strict enterprise permissions
mkdir -p "$DB_DIR"
chmod 700 "$DB_DIR" 2>/dev/null
touch "$DB_PATH"
chmod 600 "$DB_PATH" 2>/dev/null

# ------------------------------------------------------------------------------
# REGISTRY OPERATIONS
# ------------------------------------------------------------------------------

# Registers or updates a project's physical path in the local database.
db_update_project() {
    local name=$1
    local path=$2
    
    # Guard against empty arguments
    if [ -z "$name" ] || [ -z "$path" ]; then return 1; fi
    
    # Standardize to an absolute path.
    # We suppress errors (2>/dev/null) in case realpath is executed on a newly created, un-synced folder.
    path=$(realpath "$path" 2>/dev/null || echo "$path")
    
    # POSIX-Compliant Atomic Update
    # This completely replaces the brittle OS-specific 'sed_wrapper'.
    # 1. Filter out the existing entry (if any) to a temporary file
    if [ -f "$DB_PATH" ]; then
        grep -v "^${name}|" "$DB_PATH" > "${DB_PATH}.tmp"
        mv "${DB_PATH}.tmp" "$DB_PATH"
    fi
    
    # 2. Append the newly mapped absolute path
    echo "${name}|${path}" >> "$DB_PATH"
}

# Retrieves the absolute path of a project by its registered name.
db_get_path() {
    local name=$1
    if [ -z "$name" ]; then return 1; fi
    
    # Safely extract the path using the pipe delimiter
    grep "^${name}|" "$DB_PATH" | head -n 1 | cut -d'|' -f2
}

# Automatically identifies the active MELISA project based on the user's current working directory.
db_identify_by_pwd() {
    local current_dir=$(realpath "$PWD" 2>/dev/null || echo "$PWD")
    local best_match_name=""
    local longest_path=0

    # Iterate through the registry to find the most specific parent directory match
    while IFS='|' read -r name path; do
        # Skip empty lines or malformed entries
        if [ -z "$name" ] || [ -z "$path" ]; then continue; fi

        # Boundary Validation: Ensure we match exact directories, not just string prefixes.
        # This prevents a project named "/path/to/app" from incorrectly matching "/path/to/apple".
        if [[ "$current_dir" == "$path" ]] || [[ "$current_dir" == "${path}/"* ]]; then
            # Select the deepest (most specific) path match
            if [ ${#path} -gt $longest_path ]; then
                longest_path=${#path}
                best_match_name="$name"
            fi
        fi
    done < "$DB_PATH"
    
    echo "$best_match_name"
}