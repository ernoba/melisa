#!/usr/bin/env bash
# ==============================================================================
# MELISA EXECUTION ENGINE
# Description: Handles remote code execution, project cloning, synchronization,
#              and artifact transfers via secure SSH pipelines.
# ==============================================================================

# --- UI Helpers (Minimalist & Clean) ---
# Define color variables explicitly to prevent empty variable evaluation errors
export BOLD='\e[1m'
export GREEN='\e[32m'
export RED='\e[31m'
export YELLOW='\e[33m'
export BLUE='\e[34m'
export RESET='\e[0m'

log_header()  { echo -e "\n${BLUE}::${RESET} ${BOLD}$1${RESET}"; }
log_stat()    { echo -e " ${GREEN}=>${RESET} $1: ${BOLD}$2${RESET}"; }
log_info()    { echo -e " ${BLUE}[INFO]${RESET} $1"; }
log_success() { echo -e " ${GREEN}[SUCCESS]${RESET} $1"; }
log_error()   { echo -ne " ${RED}[ERROR]${RESET} $1\n" >&2; }

# Source the local database module for path and project state resolution
source "$MELISA_LIB/db.sh"

# Validates that an active server connection is configured before proceeding
ensure_connected() {
    CONN=$(get_active_conn)
    if [ -z "$CONN" ]; then
        log_error "No active server connection found!"
        echo -e "  ${YELLOW}Tip:${RESET} Execute 'melisa auth add <name> <user@ip>' to register a server."
        exit 1
    fi
}

# ------------------------------------------------------------------------------
# REMOTE OPERATIONS (CONTAINER INTERACTION)
# ------------------------------------------------------------------------------

# Pipes a local script directly into a remote container's interpreter via SSH.
# Leaves zero footprint on the host machine.
exec_run() {
    ensure_connected
    local container=$1
    local file=$2
    
    if [ -z "$container" ] || [ -z "$file" ] || [ ! -f "$file" ]; then
        log_error "Usage: melisa run <container> <file>"
        exit 1
    fi
    
    # Dynamic interpreter resolution based on file extension
    local ext="${file##*.}"
    local interpreter="bash"
    if [ "$ext" == "py" ]; then interpreter="python3"; fi
    if [ "$ext" == "js" ]; then interpreter="node"; fi
    
    log_info "Executing '${BOLD}${file}${RESET}' inside '${container}' via server '${CONN}'..."
    # Stream the file content directly into the remote interpreter's STDIN
    cat "$file" | ssh "$CONN" "melisa --send $container $interpreter -"
}

# Compresses a local directory into a stream and extracts it directly inside the remote container.
exec_upload() {
    ensure_connected
    local container=$1
    local dir=$2
    local dest=$3
    
    if [ -z "$dest" ]; then
        log_error "Usage: melisa upload <container> <local_dir> <remote_dest>"
        exit 1
    fi
    
    log_info "Transferring '${dir}' to '${container}:${dest}' via server '${CONN}'..."
    # Tar stream execution: Compress locally, pipe over SSH, and extract remotely via MELISA
    tar -czf - -C "$dir" . | ssh "$CONN" "melisa --upload $container $dest"
}

# Uploads a script, executes it interactively (TTY), and cleans up afterward.
exec_run_tty() {
    ensure_connected
    local container=$1
    local file=$2
    
    if [ -z "$container" ] || [ -z "$file" ] || [ ! -f "$file" ]; then
        log_error "Usage: melisa run-tty <container> <file>"
        exit 1
    fi
    
    local filename=$(basename "$file")
    local dir=$(dirname "$file")
    local ext="${file##*.}"
    local interpreter="bash"
    [[ "$ext" == "py" ]] && interpreter="python3"
    [[ "$ext" == "js" ]] && interpreter="node"
    
    log_info "Provisioning artifact '${BOLD}${filename}${RESET}' in remote container..."
    
    # Securely upload the specific file to the container's /tmp directory
    if tar -czf - -C "$dir" "$filename" | ssh "$CONN" "melisa --upload $container /tmp" > /dev/null 2>&1; then
        log_success "Interactive session (TTY) initialized..."
        
        # Execute interactively (-t forces pseudo-tty allocation)
        ssh -t "$CONN" "melisa --send $container $interpreter /tmp/$filename"
        
        # Mandatory Cleanup Protocol
        ssh "$CONN" "melisa --send $container rm -f /tmp/$filename" > /dev/null 2>&1
        log_success "Execution cycle completed and artifacts purged."
    else
        log_error "Failed to transfer the artifact to the remote container."
    fi
}

# ------------------------------------------------------------------------------
# PROJECT ORCHESTRATION & SYNCHRONIZATION
# ------------------------------------------------------------------------------

# Visualizes the state of a directory after a synchronization event.
inspect_result() {
    local target=$1
    echo -e "\n\e[2m[Workspace State: $target]\e[0m"
    
    # Safely count entities, ignoring permission denied errors on restricted system files
    local files=$(find "$target" -type f 2>/dev/null | wc -l)
    local dirs=$(find "$target" -type d 2>/dev/null | wc -l)
    local size=$(du -sh "$target" 2>/dev/null | cut -f1)

    log_stat "Files" "$files"
    log_stat "Dirs"  "$dirs"
    log_stat "Size"  "$size"
    
    echo -e "\n\e[1;30mProject Topology (Depth 2):\e[0m"
    # Generate a clean, pseudo-tree visualization of the top two directory levels
    find "$target" -maxdepth 2 -not -path '*/.*' 2>/dev/null | sed "s|$target||" | sed 's|^/||' | grep -v "^$" | head -n 15 | sed 's/^/  /'
    
    [ "$files" -gt 15 ] && echo "  ..."
    echo ""
}

# Retrieves a project workspace from the master server via Git or Rsync.
exec_clone() {
    ensure_connected
    
    local project_name=""
    local force_clone=false

    # Robust argument parsing
    while [[ $# -gt 0 ]]; do
        case $1 in
            --force) force_clone=true; shift ;;
            *) [ -z "$project_name" ] && project_name=$1; shift ;;
        esac
    done

    if [ -z "$project_name" ]; then
        log_error "Usage: melisa clone <name> [--force]"
        exit 1
    fi

    log_header "Provisioning Workspace: $project_name"

    # --- ANTI-NESTING PROTOCOL ---
    # Prevents creating a folder inside a folder with the same name.
    local target_dir="./$project_name"
    if [ "$(basename "$PWD")" == "$project_name" ]; then
        target_dir="."
        log_info "Context Detected: Currently inside target directory. Syncing in place."
    fi

    if [ "$force_clone" = true ]; then
        log_info "Protocol: Force Overwrite (Direct Rsync)"
        local remote_path="~/$project_name/" 
        
        # Ensure the target directory exists if we aren't cloning in-place
        [ "$target_dir" != "." ] && mkdir -p "$target_dir"

        # Trailing slashes are CRITICAL for Rsync to copy contents rather than the directory itself
        if rsync -avz --progress "$CONN:$remote_path" "$target_dir/"; then
            local full_path="$(realpath "$target_dir")"
            db_update_project "$project_name" "$full_path"
            log_success "Synchronization complete at $full_path"
            inspect_result "$target_dir"
        else
            log_error "Rsync protocol failed. Verify server path and network connection."
        fi
    else
        log_info "Protocol: Version Control (Git Default)"
        
        # Git aborts if cloning into a non-empty directory. We trap this gracefully.
        if [ "$target_dir" == "." ] && [ "$(ls -A . 2>/dev/null)" ]; then
            log_error "Directory is not empty. Use '--force' for Rsync overwrite or navigate to an empty directory."
            exit 1
        fi

        if git clone "ssh://$CONN/opt/melisa/projects/$project_name" "$target_dir"; then
            local full_path="$(realpath "$target_dir")"
            db_update_project "$project_name" "$full_path"
            log_success "Repository successfully cloned to $full_path"
            inspect_result "$target_dir"
        else
            log_error "Git clone protocol failed."
        fi
    fi
}

# Pushes local changes to the remote repository and synchronizes untracked .env files.
exec_sync() {
    ensure_connected

    # --- PRE-FLIGHT: Pastikan git dan rsync tersedia ---
    # Bug lama: hanya ssh yang dicek di entry point, git/rsync tidak pernah dicek.
    for tool in git rsync; do
        if ! command -v "$tool" >/dev/null 2>&1; then
            log_error "Required tool '${tool}' is not installed or not in PATH."
            log_error "Install it: apt install ${tool}  OR  dnf install ${tool}"
            exit 1
        fi
    done

    # 1. Identifikasi project dari registry path lokal
    local project_name
    project_name=$(db_identify_by_pwd)

    if [ -z "$project_name" ]; then
        log_error "The current directory is not registered as a MELISA project workspace."
        log_error "Run 'melisa clone <project>' first, or register manually:"
        log_error "  echo \"myapp|\$(realpath .)\" >> ~/.config/melisa/registry"
        exit 1
    fi

    # 2. Pindah ke root project
    local project_root
    project_root=$(db_get_path "$project_name")
    cd "$project_root" || { log_error "Failed to access workspace root: $project_root"; exit 1; }

    # 3. Validasi: pastikan ini adalah git repository
    # Bug lama: tidak ada validasi. Jika di-clone dengan --force (rsync, tanpa .git),
    # semua perintah git berikut akan gagal tanpa pesan error yang informatif.
    if [ ! -d ".git" ]; then
        log_error "The workspace at '${project_root}' is not a Git repository."
        log_error "This project was likely cloned with '--force' (Rsync mode)."
        log_error "To use 'sync', you need a proper Git clone:"
        log_error "  rm -rf ${project_root} && melisa clone ${project_name}"
        exit 1
    fi

    local branch
    branch=$(git branch --show-current 2>/dev/null || echo "master")
    log_header "Synchronizing $project_name [Branch: $branch]"

    # 4. Stage, commit, dan push ke bare repo
    git add .
    git commit -m "melisa-sync: $(date +'%Y-%m-%d %H:%M')" --allow-empty > /dev/null

    log_info "Transmitting delta to host server..."
    if ! git push -f origin "$branch" 2>&1 | sed 's/^/  /'; then
        log_error "Git push protocol failed. Verify network connectivity and remote configuration."
        log_error "Test manually: git remote -v"
        exit 1
    fi

    # --- CATATAN PENTING (Mengapa --update dihapus) ---
    # Sebelumnya ada: ssh "$CONN" "melisa --update $project_name --force"
    # Ini DIHAPUS karena:
    #   a) git push ke bare repo sudah memicu post-receive hook secara otomatis.
    #   b) Post-receive hook menjalankan: sudo melisa --update-all $project_name
    #      yang memperbarui workspace SEMUA user termasuk Alice.
    #   c) Pemanggilan SSH eksplisit di sini berjalan sebagai 'root' (bukan Alice),
    #      sehingga update_project mencari /home/root/$project_name yang tidak ada.
    #   d) Hasilnya: pemanggilan selalu gagal secara diam-diam → dead code.
    # Post-receive hook sudah cukup. Jika diperlukan verifikasi, gunakan:
    #   ssh "$CONN" "melisa --projects"  ← hanya untuk diagnostic

    # 5. Sync .env files — DIPERBAIKI
    # Bug lama: rsync ke "$CONN:~/$project_name/" yang = "/root/myapp/" (salah)
    # Fix: gunakan path absolut berdasarkan remote melisa username
    log_info "Synchronizing environment configurations (.env)..."

    local remote_user
    remote_user=$(get_remote_user)

    local env_files
    env_files=$(find . -maxdepth 2 -type f -name ".env")

    if [ -n "$env_files" ]; then
        if [ -n "$remote_user" ]; then
            # PATH BENAR: /home/alice/myapp/ bukan /root/myapp/
            local remote_env_path="/home/${remote_user}/${project_name}/"
            echo "$env_files" | xargs -I {} rsync -azR "{}" "$CONN:${remote_env_path}"
            log_success ".env files synced to ${remote_user}@server:${remote_env_path}"
        else
            # Fallback: remote_user belum dikonfigurasi (profil lama)
            # Beri peringatan agar user memperbarui profil mereka
            log_warning ".env sync SKIPPED: remote MELISA username not configured."
            log_warning "Update your profile: melisa auth remove ${CONN} && melisa auth add <name> ${CONN}"
            log_warning "Or manually: rsync .env root@server:/home/<your-username>/${project_name}/"
        fi
    else
        log_info "No .env files found within 2 directory levels. Skipping env sync."
    fi

    log_success "Synchronization complete. Server will propagate changes via post-receive hook."
}

# Pulls the latest physical data from the host workspace to the local machine via Rsync.
exec_get() {
    ensure_connected

    local project_name=""
    local force_get=false

    while [[ $# -gt 0 ]]; do
        case $1 in
            --force) force_get=true; shift ;;
            *) [ -z "$project_name" ] && project_name=$1; shift ;;
        esac
    done

    [ -z "$project_name" ] && project_name=$(db_identify_by_pwd)

    if [ -z "$project_name" ]; then
        log_error "Project context unknown. Usage: melisa get <n> [--force]"
        exit 1
    fi

    local local_path
    local_path=$(db_get_path "$project_name")

    if [ -z "$local_path" ]; then
        if [ "$(basename "$PWD")" == "$project_name" ]; then
            local_path="$(realpath .)"
        else
            local_path="$(realpath .)/$project_name"
        fi
    fi

    # --- PERBAIKAN PATH REMOTE ---
    # Bug lama: remote_path="~/$project_name/" → /root/$project_name/ (salah)
    # Fix: gunakan get_remote_user() untuk path yang benar
    local remote_user
    remote_user=$(get_remote_user)
    local remote_path

    if [ -n "$remote_user" ]; then
        # Path yang benar: workspace milik user Alice di server
        remote_path="/home/${remote_user}/${project_name}/"
    else
        # Fallback untuk profil lama — berikan peringatan
        log_warning "Remote MELISA username not configured. Falling back to ~/ (may point to /root/)."
        log_warning "Run 'melisa auth add' again to set your remote username."
        remote_path="~/${project_name}/"
    fi
    # --- AKHIR PERBAIKAN ---

    log_header "Retrieving Data for Workspace: $project_name"

    local opts="-avz --progress --exclude='.git/'"

    if [ "$force_get" = true ]; then
        log_info "Protocol: Force Overwrite (Data Replacement)"
    else
        log_info "Protocol: Safe Sync (Ignoring existing local files)"
        opts="$opts --ignore-existing"
    fi

    mkdir -p "$local_path"

    if rsync $opts "$CONN:$remote_path" "$local_path/"; then
        db_update_project "$project_name" "$local_path"
        log_success "Data retrieval completed at: $local_path"
        inspect_result "$local_path"
    else
        log_error "Rsync protocol failed."
        log_error "Verify workspace exists: ssh $CONN 'ls -la /home/$remote_user/'"
    fi
}


# Transparently forwards unrecognized commands directly to the MELISA host environment.
exec_forward() {
    ensure_connected
    log_header "Forwarding Payload: melisa $*"
    # -t enforces pseudo-tty allocation, allowing interactive remote commands
    ssh -t "$CONN" "melisa $*" 
}

# ══════════════════════════════════════════════════════
#  SSH TUNNEL ENGINE — Cross-Network Port Forwarding
# ══════════════════════════════════════════════════════

exec_tunnel() {
    ensure_connected
    local container=$1
    local remote_port=$2
    local local_port=${3:-$remote_port}

    if [ -z "$container" ] || [ -z "$remote_port" ]; then
        log_error "Usage: melisa tunnel <container> <remote_port> [local_port]"
        log_error "Example: melisa tunnel mywebapp 3000 8080"
        exit 1
    fi

    if ! [[ "$remote_port" =~ ^[0-9]+$ ]] || ! [[ "$local_port" =~ ^[0-9]+$ ]]; then
        log_error "Port numbers must be integers."
        exit 1
    fi

    log_header "SSH Tunnel Setup: $container"
    log_info "Querying container IP from server '${CONN}'..."

    # Minta server return IP container via melisa --ip
    local CONTAINER_IP
    CONTAINER_IP=$(ssh "$CONN" "melisa --ip $container" 2>/dev/null | tr -d '[:space:]')

    if [ -z "$CONTAINER_IP" ]; then
        log_error "Could not retrieve IP for container '$container'."
        log_error "Make sure container is running: melisa --active"
        exit 1
    fi

    log_stat "Container IP" "$CONTAINER_IP"

    # Cek apakah local port sudah dipakai
    if command -v ss >/dev/null 2>&1; then
        if ss -tlnp 2>/dev/null | grep -q ":${local_port}[[:space:]]"; then
            log_error "Local port $local_port is already in use. Use: melisa tunnel $container $remote_port <free_port>"
            exit 1
        fi
    fi

    # Direktori untuk menyimpan PID tunnel
    local TUNNEL_DIR="$HOME/.config/melisa/tunnels"
    mkdir -p "$TUNNEL_DIR"

    local TUNNEL_KEY="${container}_${remote_port}"
    local PID_FILE="$TUNNEL_DIR/${TUNNEL_KEY}.pid"
    local META_FILE="$TUNNEL_DIR/${TUNNEL_KEY}.meta"

    # Stop tunnel lama jika ada
    if [ -f "$PID_FILE" ]; then
        local OLD_PID
        OLD_PID=$(cat "$PID_FILE" 2>/dev/null)
        if [ -n "$OLD_PID" ] && kill -0 "$OLD_PID" 2>/dev/null; then
            log_info "Terminating previous tunnel (PID $OLD_PID)..."
            kill "$OLD_PID" 2>/dev/null
            sleep 1
        fi
        rm -f "$PID_FILE" "$META_FILE"
    fi

    log_info "Establishing tunnel: localhost:${local_port} → [${CONN}] → ${CONTAINER_IP}:${remote_port}"

    # Buat SSH tunnel dengan -f (background) dan -N (no remote command)
    ssh -N -f \
        -L "${local_port}:${CONTAINER_IP}:${remote_port}" \
        -o ExitOnForwardFailure=yes \
        -o ServerAliveInterval=30 \
        -o ServerAliveCountMax=3 \
        -o StrictHostKeyChecking=no \
        "$CONN" 2>/tmp/melisa_tunnel_err.tmp

    local SSH_EXIT=$?
    if [ $SSH_EXIT -ne 0 ]; then
        log_error "SSH tunnel failed (exit: $SSH_EXIT)."
        [ -f /tmp/melisa_tunnel_err.tmp ] && cat /tmp/melisa_tunnel_err.tmp >&2
        log_error "Tip: Make sure port $remote_port is listening inside the container."
        exit 1
    fi

    # Tangkap PID proses ssh yang baru dibuat
    sleep 0.5
    local TUNNEL_PID
    TUNNEL_PID=$(pgrep -n -f "ssh.*-L.*${local_port}:${CONTAINER_IP}:${remote_port}" 2>/dev/null)

    # Simpan metadata tunnel
    echo "${TUNNEL_PID:-unknown}" > "$PID_FILE"
    {
        echo "container=$container"
        echo "container_ip=$CONTAINER_IP"
        echo "remote_port=$remote_port"
        echo "local_port=$local_port"
        echo "server=$CONN"
        echo "started=$(date '+%Y-%m-%d %H:%M:%S')"
    } > "$META_FILE"

    log_success "Tunnel active!"
    echo -e ""
    echo -e "  ${BOLD}${GREEN}► ACCESS URL  :${RESET}  http://localhost:${local_port}"
    echo -e "  ${BOLD}► ROUTE       :${RESET}  localhost:${local_port} → ${CONN} → ${CONTAINER_IP}:${remote_port}"
    echo -e "  ${BOLD}► PID         :${RESET}  ${TUNNEL_PID:-N/A}"
    echo -e "  ${BOLD}► STOP WITH   :${RESET}  melisa tunnel-stop ${container} ${remote_port}"
    echo -e ""
    echo -e "  ${YELLOW}[NOTE]${RESET} This tunnel works across different networks as long as"
    echo -e "  ${YELLOW}       ${RESET} the SSH port of '${CONN}' is reachable from this machine."
    echo -e ""
}

exec_tunnel_list() {
    local TUNNEL_DIR="$HOME/.config/melisa/tunnels"

    if [ ! -d "$TUNNEL_DIR" ] || [ -z "$(ls -A "$TUNNEL_DIR" 2>/dev/null)" ]; then
        log_info "No tunnels found."
        return
    fi

    log_header "Active Tunnels"
    printf "  ${BOLD}%-20s %-8s %-8s %-25s %s${RESET}\n" "CONTAINER" "R.PORT" "L.PORT" "SERVER" "STATUS"
    echo "  $(printf '─%.0s' {1..72})"

    local found=0
    for META_FILE in "$TUNNEL_DIR"/*.meta; do
        [ -f "$META_FILE" ] || continue
        local PID_FILE="${META_FILE%.meta}.pid"

        local container remote_port local_port server started pid status_str
        container=$(grep "^container=" "$META_FILE" | cut -d= -f2-)
        remote_port=$(grep "^remote_port=" "$META_FILE" | cut -d= -f2-)
        local_port=$(grep "^local_port=" "$META_FILE" | cut -d= -f2-)
        server=$(grep "^server=" "$META_FILE" | cut -d= -f2-)
        started=$(grep "^started=" "$META_FILE" | cut -d= -f2-)

        if [ -f "$PID_FILE" ]; then
            pid=$(cat "$PID_FILE" 2>/dev/null)
            if [ -n "$pid" ] && [ "$pid" != "unknown" ] && kill -0 "$pid" 2>/dev/null; then
                status_str="${GREEN}RUNNING${RESET} (PID $pid)"
            else
                status_str="${RED}DEAD${RESET}"
                rm -f "$PID_FILE" "$META_FILE"
                continue
            fi
        else
            status_str="${YELLOW}UNKNOWN${RESET}"
        fi

        printf "  %-20s %-8s %-8s %-25s " "$container" "$remote_port" "$local_port" "$server"
        echo -e "$status_str"
        echo -e "  ${BLUE}[INFO]${RESET} Access: http://localhost:${local_port}  |  Started: ${started}"
        echo ""
        found=$((found + 1))
    done

    [ $found -eq 0 ] && log_info "No active tunnels."
}

exec_tunnel_stop() {
    local container=$1
    local remote_port=$2

    if [ -z "$container" ]; then
        log_error "Usage: melisa tunnel-stop <container> [remote_port]"
        exit 1
    fi

    local TUNNEL_DIR="$HOME/.config/melisa/tunnels"
    local stopped=0

    for META_FILE in "$TUNNEL_DIR"/*.meta; do
        [ -f "$META_FILE" ] || continue

        local meta_container meta_port
        meta_container=$(grep "^container=" "$META_FILE" | cut -d= -f2-)
        meta_port=$(grep "^remote_port=" "$META_FILE" | cut -d= -f2-)

        [ "$meta_container" != "$container" ] && continue
        [ -n "$remote_port" ] && [ "$meta_port" != "$remote_port" ] && continue

        local PID_FILE="${META_FILE%.meta}.pid"
        if [ -f "$PID_FILE" ]; then
            local pid
            pid=$(cat "$PID_FILE" 2>/dev/null)
            if [ -n "$pid" ] && [ "$pid" != "unknown" ] && kill -0 "$pid" 2>/dev/null; then
                kill "$pid" 2>/dev/null
                log_success "Tunnel stopped (PID $pid) — ${container}:${meta_port}"
            else
                log_info "Tunnel process already dead."
            fi
        fi
        rm -f "$PID_FILE" "$META_FILE"
        stopped=$((stopped + 1))
    done

    if [ $stopped -eq 0 ]; then
        log_error "No tunnel found for '${container}'${remote_port:+ port ${remote_port}}."
    fi
}