# ==============================================================================
# MELISA CLIENT (Rust) — Windows Installer (PowerShell)
# ==============================================================================
# Installs the melisa-client binary on Windows 10 / 11.
# Requires PowerShell 5.1+ (pre-installed on all modern Windows versions).
#
# Usage (run as standard user — UAC prompts appear where needed):
#   .\install.ps1
#   .\install.ps1 -NoBuild       # install a pre-built melisa.exe
#   .\install.ps1 -SkipOpenSsh   # skip OpenSSH capability check
# ==============================================================================

[CmdletBinding()]
param(
    [switch]$NoBuild,
    [switch]$SkipOpenSsh
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

# ── Colour helpers ────────────────────────────────────────────────────────────

function Write-MelisaInfo    { param([string]$Msg) Write-Host "  [+] $Msg" -ForegroundColor Cyan }
function Write-MelisaSuccess { param([string]$Msg) Write-Host "  [v] $Msg" -ForegroundColor Green }
function Write-MelisaWarning { param([string]$Msg) Write-Host "  [!] $Msg" -ForegroundColor Yellow }
function Write-MelisaError   { param([string]$Msg) Write-Host "  [ERROR] $Msg" -ForegroundColor Red }
function Write-MelisaFatal   {
    param([string]$Msg)
    Write-Host "`n[FATAL] $Msg" -ForegroundColor Red
    exit 1
}

Write-Host ""
Write-Host "╔═══════════════════════════════════════════╗" -ForegroundColor Cyan
Write-Host "║    MELISA CLIENT — WINDOWS INSTALLER      ║" -ForegroundColor Cyan
Write-Host "╚═══════════════════════════════════════════╝`n" -ForegroundColor Cyan

$ScriptDir = Split-Path -Parent $MyInvocation.MyCommand.Path

# ── Step 1: Windows version check ────────────────────────────────────────────

Write-MelisaInfo "Checking Windows version..."
$OsVersion = [System.Environment]::OSVersion.Version
if ($OsVersion.Major -lt 10) {
    Write-MelisaFatal "MELISA Client requires Windows 10 or later. Detected: $OsVersion"
}
Write-MelisaSuccess "Windows $($OsVersion.Major).$($OsVersion.Minor) detected."

# ── Step 2: Ensure OpenSSH Client is installed ────────────────────────────────

if (-not $SkipOpenSsh) {
    Write-MelisaInfo "Verifying OpenSSH Client capability..."

    $sshPath = (Get-Command ssh.exe -ErrorAction SilentlyContinue)?.Source
    if (-not $sshPath) {
        Write-MelisaWarning "OpenSSH Client not found. Attempting installation..."

        # Check if the Optional Feature is available
        $feature = Get-WindowsCapability -Online | Where-Object { $_.Name -like 'OpenSSH.Client*' }
        if ($feature -and $feature.State -ne 'Installed') {
            try {
                Write-MelisaInfo "Installing OpenSSH.Client via Windows Optional Features (may require elevation)..."
                # Run in a new elevated process if needed
                $isAdmin = ([Security.Principal.WindowsPrincipal][Security.Principal.WindowsIdentity]::GetCurrent()).IsInRole(
                    [Security.Principal.WindowsBuiltinRole]::Administrator
                )
                if ($isAdmin) {
                    Add-WindowsCapability -Online -Name $feature.Name | Out-Null
                    Write-MelisaSuccess "OpenSSH Client installed."
                } else {
                    Write-MelisaInfo "Elevating to install OpenSSH Client (UAC prompt may appear)..."
                    $cmd = "Add-WindowsCapability -Online -Name '$($feature.Name)'"
                    Start-Process powershell -Verb RunAs -ArgumentList "-NoProfile -Command `"$cmd`"" -Wait
                    Write-MelisaSuccess "OpenSSH Client installation triggered. Verifying..."
                }
            } catch {
                Write-MelisaWarning "Automatic OpenSSH installation failed: $_"
                Write-Host ""
                Write-Host "Please install OpenSSH Client manually:" -ForegroundColor Yellow
                Write-Host "  Settings -> Apps -> Optional Features -> Add a feature -> OpenSSH Client"
                Write-Host "  — or in an elevated PowerShell prompt:" -ForegroundColor Yellow
                Write-Host "  Add-WindowsCapability -Online -Name OpenSSH.Client~~~~0.0.1.0" -ForegroundColor White
                Write-Host ""
            }
        } elseif ($feature -and $feature.State -eq 'Installed') {
            Write-MelisaSuccess "OpenSSH Client is already installed."
        } else {
            Write-MelisaWarning "OpenSSH Client optional feature not available on this system."
            Write-Host "Please install OpenSSH manually before using melisa." -ForegroundColor Yellow
        }

        # Re-check after install attempt
        $sshPath = (Get-Command ssh.exe -ErrorAction SilentlyContinue)?.Source
        if (-not $sshPath) {
            Write-MelisaWarning "ssh.exe still not found in PATH. Tunnel and remote commands require it."
        }
    } else {
        Write-MelisaSuccess "OpenSSH Client is present: $sshPath"
    }
}

# ── Step 3: Install / verify Rust toolchain ──────────────────────────────────

if (-not $NoBuild) {
    Write-MelisaInfo "Verifying Rust toolchain..."

    $cargoPath = (Get-Command cargo.exe -ErrorAction SilentlyContinue)?.Source
    if (-not $cargoPath) {
        Write-MelisaWarning "Rust toolchain not found. Installing via rustup..."

        $rustupUrl      = "https://win.rustup.rs/x86_64"
        $rustupInstaller = "$env:TEMP\rustup-init.exe"

        Write-MelisaInfo "Downloading rustup-init.exe..."
        try {
            Invoke-WebRequest -Uri $rustupUrl -OutFile $rustupInstaller -UseBasicParsing
        } catch {
            Write-MelisaFatal "Failed to download rustup-init.exe: $_`nPlease install Rust manually from https://rustup.rs"
        }

        Write-MelisaInfo "Running rustup-init.exe (this may take a few minutes)..."
        $rustupProcess = Start-Process -FilePath $rustupInstaller `
            -ArgumentList "-y", "--no-modify-path" `
            -Wait -PassThru -NoNewWindow

        if ($rustupProcess.ExitCode -ne 0) {
            Write-MelisaFatal "rustup-init.exe exited with code $($rustupProcess.ExitCode)."
        }

        # Add Cargo to PATH for this session
        $cargoHome = "$env:USERPROFILE\.cargo\bin"
        if (Test-Path $cargoHome) {
            $env:PATH = "$cargoHome;$env:PATH"
            Write-MelisaSuccess "Rust toolchain installed. Cargo is now in PATH for this session."
        } else {
            Write-MelisaFatal "Rust was installed but Cargo binary directory not found at '$cargoHome'."
        }
    } else {
        Write-MelisaSuccess "Rust toolchain is already present."
    }
}

# ── Step 4: Compile the binary ────────────────────────────────────────────────

if (-not $NoBuild) {
    Write-MelisaInfo "Compiling melisa-client (release build)..."

    $cargoToml = Join-Path $ScriptDir "Cargo.toml"
    if (-not (Test-Path $cargoToml)) {
        Write-MelisaFatal "Cargo.toml not found at '$ScriptDir'. Run this installer from the melisa_client_rs directory."
    }

    Push-Location $ScriptDir
    try {
        $buildProc = Start-Process -FilePath "cargo" `
            -ArgumentList "build", "--release" `
            -Wait -PassThru -NoNewWindow
        if ($buildProc.ExitCode -ne 0) {
            Write-MelisaFatal "Compilation failed with exit code $($buildProc.ExitCode)."
        }
    } finally {
        Pop-Location
    }

    $BinaryPath = Join-Path $ScriptDir "target\release\melisa.exe"
    Write-MelisaSuccess "Compilation completed: $BinaryPath"
} else {
    $BinaryPath = Join-Path $ScriptDir "melisa.exe"
    if (-not (Test-Path $BinaryPath)) {
        Write-MelisaFatal "Pre-built binary not found at '$BinaryPath'. Run without -NoBuild to compile from source."
    }
}

# ── Step 5: Install to user-local directory ───────────────────────────────────

Write-MelisaInfo "Provisioning installation directories..."
$InstallDir  = Join-Path $env:LOCALAPPDATA "melisa\bin"
$ConfigDir   = Join-Path $env:APPDATA "melisa"
$DataDir     = Join-Path $env:LOCALAPPDATA "melisa"

foreach ($dir in @($InstallDir, $ConfigDir, $DataDir)) {
    if (-not (Test-Path $dir)) {
        New-Item -ItemType Directory -Path $dir -Force | Out-Null
    }
}

Write-MelisaInfo "Deploying melisa.exe to $InstallDir ..."
Copy-Item -Path $BinaryPath -Destination (Join-Path $InstallDir "melisa.exe") -Force
Write-MelisaSuccess "Binary deployed."

# ── Step 6: Add InstallDir to user PATH ──────────────────────────────────────

Write-MelisaInfo "Checking user PATH configuration..."
$UserPath = [System.Environment]::GetEnvironmentVariable("PATH", "User") ?? ""

if ($UserPath -notlike "*$InstallDir*") {
    $NewPath = if ($UserPath) { "$UserPath;$InstallDir" } else { $InstallDir }
    [System.Environment]::SetEnvironmentVariable("PATH", $NewPath, "User")
    $env:PATH = "$env:PATH;$InstallDir"
    Write-MelisaSuccess "Added '$InstallDir' to user PATH."
    $PathUpdated = $true
} else {
    Write-MelisaSuccess "PATH already contains '$InstallDir'."
    $PathUpdated = $false
}

# ── Step 7: Verify ────────────────────────────────────────────────────────────

Write-MelisaInfo "Verifying installation..."
$melisaCmd = (Get-Command melisa.exe -ErrorAction SilentlyContinue)?.Source
if ($melisaCmd) {
    Write-MelisaSuccess "melisa.exe is accessible at: $melisaCmd"
} else {
    Write-MelisaWarning "melisa.exe installed but not yet visible in this session's PATH."
}

Write-Host ""
Write-Host "╔══════════════════════════════════════════════════════════╗" -ForegroundColor Green
Write-Host "║  [SUCCESS] MELISA Client (Rust) installed successfully!  ║" -ForegroundColor Green
Write-Host "╚══════════════════════════════════════════════════════════╝" -ForegroundColor Green

if ($PathUpdated) {
    Write-Host ""
    Write-Host "IMPORTANT: Open a new terminal window for the PATH change to take effect." -ForegroundColor Yellow
}

Write-Host ""
Write-Host "Run: " -NoNewline
Write-Host "melisa auth add <name> <user@host>" -ForegroundColor Cyan -NoNewline
Write-Host " to register your first server."
Write-Host ""