# ANALISIS KESALAHAN NETWORK BRIDGE & PERBAIKAN

## 🔴 MASALAH UTAMA

Dari output error:
```
[ERROR] Failed to restart lxc-net even after override. Check 'sudo journalctl -u lxc-net' for details.
[WARNING] lxcbr0 did not appear after override. VirtIO-FS chattr limitation is non-fatal
[ERROR] Host network bridge 'lxcbr0' is down and auto-repair failed.
```

## 🔍 ROOT CAUSE (PENYEBAB AKAR)

### 1. **No Retry Mechanism** ❌
- Ketika `lxc-net restart` gagal di OrbStack, tidak ada mekanisme retry.
- Fungsi langsung melanjutkan tanpa memastikan layanan benar-benar hidup.

### 2. **Missing Error Propagation** ❌
- `ensure_host_network_ready()` tidak mengembalikan `Result<bool>` atau `bool`.
- Tidak ada cara untuk mengetahui apakah operasi berhasil atau gagal.
- Fungsi `verify_host_runtime()` tidak bisa membedakan antara sukses dan gagal.

### 3. **Insufficient Sleep Duration** ❌
- Sleep hanya 2 detik sebelum memeriksa lxcbr0.
- Di OrbStack (VM), network bridge butuh waktu lebih lama untuk muncul.
- Seharusnya ada polling loop dengan timeout yang lebih panjang.

### 4. **No Service Status Verification** ❌
- Tidak ada pengecekan apakah service `lxc-net` benar-benar running.
- Hanya memeriksa apakah device `/sys/class/net/lxcbr0` ada.
- Jika service gagal start, tidak ada error yang jelas.

### 5. **Missing Diagnostic Information** ❌
- Ketika lxc-net restart gagal, tidak ada attempt untuk fetch journal errors.
- User harus manual jalankan `sudo journalctl -u lxc-net` untuk debug.
- Pesan error kurang informatif untuk troubleshooting.

### 6. **No Fallback Strategy** ❌
- Jika auto-repair gagal, tidak ada fallback atau alternative approach.
- Error langsung dikembalikan ke user tanpa solusi.

### 7. **Race Condition** ❌
- Daemon reload di step 3 tidak dijamin selesai sebelum restart lxc-net.
- Perlu tambahan delay antara `daemon-reload` dan `systemctl restart`.

## 📝 FILE YANG PERLU DIPERBAIKI

### Primary Files:
1. **src/core/container/network.rs**
   - `apply_orbstack_lxcnet_override()` - Tambah retry & error checking
   - `ensure_host_network_ready()` - Return Result untuk error propagation
   - Tambah fungsi baru: `check_lxcnet_service_status()`

2. **src/core/container/lifecycle.rs**
   - `verify_host_runtime()` - Handle error dari ensure_host_network_ready()
   - Tambah polling loop dengan timeout lebih panjang
   - Improve error messages

## 🔧 PERBAIKAN YANG AKAN DILAKUKAN

### Fix 1: Return Type untuk ensure_host_network_ready()
```rust
pub async fn ensure_host_network_ready(audit: bool) -> Result<bool, String>
```

### Fix 2: Retry Logic dengan Exponential Backoff
- Max 3 attempts untuk restart lxc-net
- Delay: 2s, 4s, 8s antara attempts
- Polling hingga 30 detik untuk lxcbr0 muncul

### Fix 3: Service Status Verification
- Check `systemctl is-active lxc-net`
- Verify service enabled: `systemctl is-enabled lxc-net`

### Fix 4: Better Error Messages
- Capture stderr dari lxc-net restart
- Show actual systemd errors jika ada
- Display lxc-net service status

### Fix 5: Fallback Strategy
- Jika daemon-reload gagal, skip ke restart langsung
- Jika restart gagal 3x, offer `melisa --setup` command
- Provide detailed troubleshooting steps

### Fix 6: Longer Polling Timeout
- Dari 2 detik menjadi 30 detik dengan polling interval 1 detik
- Cek setiap detik apakah lxcbr0 ada dan sudah punya IP

### Fix 7: Add Diagnostic Function
```rust
async fn diagnose_lxcnet_failure() -> String
```
- Capture `journalctl -u lxc-net` output
- Check /etc/default/lxc-net config
- Verify override file exists dan benar
