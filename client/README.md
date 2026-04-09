# MELISA — Rombak Besar: Input Guard + Cross-Platform Rust Client

Dokumen ini menjelaskan **dua perubahan besar** pada kodebase MELISA:

1. **Input Guard** — modul filter keamanan baru di sisi server (`src/core/guard/`)
2. **Rust Client** — penulisan ulang `melisa_client` dari Bash ke Rust (`melisa_client_rs/`)
   berjalan native di **Linux**, **macOS**, dan **Windows**

---

## Bagian 1 — Input Guard (Server-Side)

### File baru

```
src/core/guard/
├── mod.rs      ← deklarasi modul publik
└── filter.rs   ← seluruh logika validasi + 30+ unit test
```

### Cara kerja

`filter_input(raw_input: &str) -> FilterResult` dipanggil **satu kali** di
dalam REPL loop di `melisa_cli.rs`, sebelum `execute_command`. Setiap input
yang gagal dikembalikan sebagai `FilterResult::Block(reason)` dengan pesan
human-readable dan TIDAK dieksekusi.

#### Aturan yang diterapkan (urutan prioritas)

| # | Aturan | Keterangan |
|---|--------|-----------|
| 1 | **Length guard** | Input > 1 024 karakter → blokir |
| 2 | **Null-byte guard** | `\0` tidak pernah valid dalam perintah MELISA |
| 3 | **Shell-injection guard** | Blokir `` $( ` ${ && || ; \n \r > < | `` kecuali pada konteks `--send` (pass-through ke container) dan `cd` |
| 4 | **Path-traversal guard** | Selalu aktif — blokir `../` `..\` `..%2f` `..%5c` |

#### Konteks pengecualian (penting!)

* **`--send`** — argumennya diteruskan verbatim ke container shell via LXC.
  Perintah sah seperti `apt update && apt upgrade -y` harus lolos.
  Injection tetap terblokir oleh layer SSH/LXC yang menerima argumen sebagai
  array, bukan string shell.
* **`cd`** — dieksekusi lokal di REPL. Injection tidak relevan, tetapi
  path-traversal tetap diblokir.

### Integrasi ke kode yang sudah ada

#### 1. `src/core/mod.rs` — tambahkan satu baris

```rust
pub mod container;
pub mod guard;          // ← TAMBAHKAN INI
pub mod metadata;
pub mod project;
pub mod root_check;
pub mod setup;
pub mod user;
```

#### 2. `src/cli/melisa_cli.rs` — sisipkan gate di dalam REPL loop

Cari blok ini (sekitar baris 60-65):

```rust
Ok(line) => {
    let input = line.trim();
    if input.is_empty() {
        continue;
    }
    match execute_command(input, &p_info.user, &p_info.home).await {
```

Ganti dengan:

```rust
Ok(line) => {
    let input = line.trim();
    if input.is_empty() {
        continue;
    }

    // ── Input security gate ──────────────────────────────────────────
    if let crate::core::guard::FilterResult::Block(reason) =
        crate::core::guard::filter_input(input)
    {
        eprintln!("{}[BLOCKED]{} {}", RED, RESET, reason);
        let _ = rl.add_history_entry(input);
        let _ = rl.append_history(&history_path);
        continue;
    }
    // ────────────────────────────────────────────────────────────────

    match execute_command(input, &p_info.user, &p_info.home).await {
```

Detail lengkap ada di `patches/guard_integration.patch.txt`.

---

## Bagian 2 — Rust Client (Cross-Platform)

### Mengapa ditulis ulang ke Rust?

| Masalah Bash Client | Solusi Rust Client |
|--------------------|--------------------|
| Tidak jalan di Windows (`bash` tidak tersedia) | Binary native `.exe` dari `cargo build` |
| Tidak ada validasi input sebelum SSH | `filter.rs` memblokir injection sebelum `Command::new` |
| Bergantung pada `ssh-copy-id` (tidak ada di Windows) | Implementasi manual fallback di `auth.rs` |
| Multiplexing hanya Unix socket | Deteksi platform, nonaktifkan di Windows |
| Installer hanya `install.sh` | Tersedia `install.sh` (Linux/macOS) dan `install.ps1` (Windows) |

### Struktur file

```
melisa_client_rs/
├── Cargo.toml          ← dependencies: colored, dirs
├── install.sh          ← installer Linux/macOS (auto-install Rust + OpenSSH)
├── install.ps1         ← installer Windows PowerShell
└── src/
    ├── main.rs         ← entry point + command router
    ├── auth.rs         ← manajemen profil SSH (setara auth.sh)
    ├── color.rs        ← konstanta ANSI + helpers log (setara exec.sh warna)
    ├── db.rs           ← registry proyek lokal (setara db.sh)
    ├── exec.rs         ← operasi remote SSH/SCP (setara exec.sh)
    ├── filter.rs       ← sanitisasi argumen sebelum SSH
    └── platform.rs     ← deteksi OS + resolusi path + cek dependensi
```

### Pemetaan modul ke file Bash lama

| File Bash lama | Modul Rust baru | Fungsi utama |
|----------------|-----------------|-------------|
| `src/melisa` | `src/main.rs` | Entry point & router |
| `src/auth.sh` | `src/auth.rs` | Profil SSH, `auth_add/remove/switch/list` |
| `src/exec.sh` | `src/exec.rs` | `exec_run`, `exec_upload`, `exec_tunnel`, dll |
| `src/db.sh` | `src/db.rs` | `db_update_project`, `db_get_path` |
| `src/utils.sh` | `src/color.rs` | Warna ANSI, `log_info/success/error` |
| *(baru)* | `src/filter.rs` | Sanitisasi argumen sebelum dikirim ke SSH |
| *(baru)* | `src/platform.rs` | Deteksi OS, path config, cek dependency |
| `install.sh` | `install.sh` + `install.ps1` | Installer multi-platform |

### Platform matrix

| Fitur | Linux | macOS | Windows |
|-------|-------|-------|---------|
| Binary name | `melisa` | `melisa` | `melisa.exe` |
| SSH client | `ssh` | `ssh` | `ssh.exe` (OpenSSH inbox Win10+) |
| ssh-copy-id | ✅ pakai sistem | ✅ pakai sistem | ❌ → manual fallback di `auth.rs` |
| SSH Multiplexing | ✅ ControlMaster | ✅ ControlMaster | ❌ (tidak ada Unix socket) |
| Rsync sync | ✅ | ✅ | ❌ → fallback ke `scp -r` |
| Config dir | `~/.config/melisa` | `~/.config/melisa` | `%APPDATA%\melisa` |
| Data dir | `~/.local/share/melisa` | `~/.local/share/melisa` | `%LOCALAPPDATA%\melisa` |
| File permissions (600/700) | ✅ | ✅ | N/A (NTFS ACL) |

### Cara instalasi

#### Linux / macOS

```bash
# Clone atau copy folder melisa_client_rs ke mesin lokal
cd melisa_client_rs
chmod +x install.sh
./install.sh
# Setelah selesai, muat ulang shell:
source ~/.bashrc   # atau ~/.zshrc
```

Installer akan otomatis:
1. Mendeteksi OS dan package manager
2. Menginstall OpenSSH client jika belum ada
3. Menginstall Rust (rustup) jika belum ada
4. `cargo build --release`
5. Copy binary ke `~/.local/bin/melisa`
6. Mendaftarkan PATH di shell RC

#### Windows (PowerShell)

```powershell
# Buka PowerShell (tidak perlu Administrator untuk sebagian besar langkah)
cd melisa_client_rs
Set-ExecutionPolicy -Scope Process -ExecutionPolicy Bypass
.\install.ps1
# Buka terminal baru agar PATH berlaku
```

Installer akan otomatis:
1. Memeriksa Windows 10/11
2. Menginstall OpenSSH Client via Windows Optional Features (UAC prompt sekali)
3. Mengunduh dan menjalankan `rustup-init.exe`
4. `cargo build --release`
5. Copy `melisa.exe` ke `%LOCALAPPDATA%\melisa\bin\`
6. Mendaftarkan path ke user PATH (tanpa perlu Administrator)

### Perintah yang tersedia (identik dengan Bash client)

```
melisa auth add <n> <user@host>       Daftarkan server remote
melisa auth switch <n>                Ganti server aktif
melisa auth list                      Tampilkan semua server
melisa auth remove <n>                Hapus profil server

melisa clone <n> [--force]            Clone workspace proyek
melisa sync  <n>                      Push perubahan lokal ke server
melisa get   <n> [--force]            Pull data master ke lokal

melisa run     <container> <file>     Eksekusi script di container (background)
melisa run-tty <container> <file>     Eksekusi script dengan TTY interaktif
melisa upload  <container> <dir> <dst>  Transfer direktori ke container
melisa shell                          Buka SSH shell langsung ke host

melisa tunnel      <cont> <port> [lp] Forward port container ke localhost
melisa tunnel-list                    Tampilkan tunnel aktif
melisa tunnel-stop <cont> [port]      Stop tunnel berjalan
```

---

## Ringkasan perubahan keseluruhan

```
PERUBAHAN BARU
══════════════
src/core/guard/mod.rs          ← modul guard baru (server)
src/core/guard/filter.rs       ← 30+ test, 4 aturan keamanan (server)
melisa_client_rs/              ← penulisan ulang client ke Rust
  Cargo.toml
  install.sh
  install.ps1
  src/main.rs
  src/auth.rs
  src/color.rs
  src/db.rs
  src/exec.rs
  src/filter.rs
  src/platform.rs

PERUBAHAN PADA FILE YANG ADA
═════════════════════════════
src/core/mod.rs                ← +1 baris: pub mod guard;
src/cli/melisa_cli.rs          ← +9 baris: filter gate di REPL loop
```

Semua kode baru mengikuti gaya penulisan yang sama:
- Konstanta warna identik (`GREEN`, `RED`, `YELLOW`, `RESET`, `BOLD`, `CYAN`, `BLUE`)
- Pola `println!("{}[TAG]{} pesan", COLOR, RESET)` di semua output
- Unit test `#[cfg(test)] mod tests { ... }` di setiap modul
- Tidak ada `unwrap()` tanpa fallback pada path kritis
- Dokumentasi `///` pada semua fungsi publik