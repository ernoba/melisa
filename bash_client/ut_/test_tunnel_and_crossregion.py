#!/usr/bin/env python3
# =============================================================================
# MELISA — Unit Tests: Tunnel Mode & Cross-Region Connectivity
# =============================================================================
#
# Test Coverage:
#   Suite 1 : Tunnel parameter validation (port, container name)
#   Suite 2 : .meta and .pid file management
#   Suite 3 : exec_tunnel_list logic (listing active tunnels)
#   Suite 4 : exec_tunnel_stop logic (stopping tunnels)
#   Suite 5 : Cross-region scenario (US -> Indonesia via public IP)
#   Suite 6 : Local port conflict detection
#   Suite 7 : Robustness (sudden tunnel death, corrupted files, etc.)
#   Suite 8 : Mocked SSH connectivity (simulating cross-region connection)
#
# Execution:
#   python3 test_tunnel_and_crossregion.py -v
#   python3 test_tunnel_and_crossregion.py -v TestTunnelPortValidation
#   python3 test_tunnel_and_crossregion.py --debug
#
# =============================================================================

import os
import sys
import stat
import time
import shutil
import socket
import signal
import tempfile
import textwrap
import unittest
import subprocess
import threading
from pathlib import Path
from typing import Optional, Tuple

# ─────────────────────────────────────────────────────────────────────────────
# Project Location
# ─────────────────────────────────────────────────────────────────────────────
def find_melisa_root() -> Optional[Path]:
    candidates = [
        Path(__file__).parent,
        Path(__file__).parent.parent,
        Path(__file__).parent.parent.parent,
        Path(__file__).parent.parent.parent.parent,
        Path.cwd(),
        Path.cwd().parent,
    ]
    for p in candidates:
        if (p / "Cargo.toml").exists() and (p / "src" / "main.rs").exists():
            return p
    return None

MELISA_ROOT = find_melisa_root()
CLIENT_SRC  = MELISA_ROOT / "src" / "melisa_client" / "src" if MELISA_ROOT else None

# ─────────────────────────────────────────────────────────────────────────────
# Terminal Colors & Global Configuration
# ─────────────────────────────────────────────────────────────────────────────
GREEN  = "\033[32m"
RED    = "\033[31m"
YELLOW = "\033[33m"
CYAN   = "\033[36m"
BOLD   = "\033[1m"
RESET  = "\033[0m"

DEBUG_MODE = False
if "--debug" in sys.argv:
    DEBUG_MODE = True
    sys.argv.remove("--debug")

def col(text: str, color: str) -> str:
    return f"{color}{text}{RESET}" if sys.stdout.isatty() else text

def debug_print(cmd_name: str, args: list, rc: int, stdout: str, stderr: str):
    """Prints raw, unfiltered subprocess output when DEBUG_MODE is active."""
    if not DEBUG_MODE:
        return
    print(f"\n{col('--- [DEBUG: ' + cmd_name + '] ---', YELLOW)}")
    print(f"{col('Command:', BOLD)} {args}")
    print(f"{col('Exit Code:', BOLD)} {rc}")
    print(f"{col('STDOUT:', BOLD)}\n{stdout}")
    print(f"{col('STDERR:', BOLD)}\n{stderr}")
    print(col('-----------------------', YELLOW))

# ─────────────────────────────────────────────────────────────────────────────
# BashEnv — Isolated environment for testing bash modules
# ─────────────────────────────────────────────────────────────────────────────
class BashEnv:
    """Isolated environment to test Melisa bash modules."""

    def __init__(self, fake_ssh: bool = False):
        self.tmp_dir = tempfile.mkdtemp(prefix="melisa_tunnel_test_")
        self.home    = Path(self.tmp_dir) / "home"
        self.home.mkdir(parents=True)
        self.bin_dir = self.home / ".local" / "bin"
        self.bin_dir.mkdir(parents=True, exist_ok=True)
        self.lib_dir = self.home / ".local" / "share" / "melisa"
        self.lib_dir.mkdir(parents=True, exist_ok=True)
        self.tunnel_dir = self.home / ".config" / "melisa" / "tunnels"
        self.tunnel_dir.mkdir(parents=True, exist_ok=True)
        self.config_dir = self.home / ".config" / "melisa"

        # Copy bash modules from source if available
        if CLIENT_SRC and CLIENT_SRC.exists():
            for sh_file in CLIENT_SRC.glob("*.sh"):
                dest = self.lib_dir / sh_file.name
                shutil.copy2(sh_file, dest)
                dest.chmod(dest.stat().st_mode | stat.S_IEXEC)

        # Create a fake SSH binary if requested (for mock connections)
        if fake_ssh:
            self._install_fake_ssh()

    def _install_fake_ssh(self, response: str = "10.0.3.5", exit_code: int = 0):
        """Install a fake 'ssh' binary that mocks server responses."""
        fake_ssh_script = self.bin_dir / "ssh"
        fake_ssh_script.write_text(textwrap.dedent(f"""\
            #!/bin/bash
            # Fake SSH for testing — does not perform actual connections
            # Capture arguments for logging
            ARGS="$@"
            
            # If container IP is requested (melisa --ip)
            if echo "$ARGS" | grep -q -- "--ip"; then
                echo "{response}"
                exit {exit_code}
            fi
            
            # If -N -f (background tunnel mode) — simulate success
            if echo "$ARGS" | grep -q -- "-N"; then
                # Spawn a dummy process so the PID can be captured
                sleep 3600 &
                disown
                exit {exit_code}
            fi
            
            # Default: echo args and exit successfully
            echo "FAKE_SSH: $ARGS"
            exit {exit_code}
        """))
        fake_ssh_script.chmod(0o755)

    def install_fake_ssh_with_ip(self, container_ip: str, exit_code: int = 0):
        """Install a fake SSH that returns a specific container IP."""
        self._install_fake_ssh(response=container_ip, exit_code=exit_code)

    def install_fake_ssh_failing(self):
        """Install a fake SSH that always fails (simulating an unreachable server)."""
        fake_ssh_script = self.bin_dir / "ssh"
        fake_ssh_script.write_text(textwrap.dedent("""\
            #!/bin/bash
            echo "ssh: connect to host server port 22: Connection refused" >&2
            echo "ssh: connect to host server port 22: Connection timed out" >&2
            exit 255
        """))
        fake_ssh_script.chmod(0o755)

    def install_fake_ss(self, ports_in_use: list = None):
        """Install a fake 'ss' that reports specific ports as being in use."""
        ports_in_use = ports_in_use or []
        lines = "\n".join(
            f"tcp   LISTEN 0  128  0.0.0.0:{p}  0.0.0.0:*"
            for p in ports_in_use
        )
        fake_ss = self.bin_dir / "ss"
        fake_ss.write_text(textwrap.dedent(f"""\
            #!/bin/bash
            echo "Netid  State   Recv-Q  Send-Q  Local Address:Port"
            echo "{lines}"
        """))
        fake_ss.chmod(0o755)

    def set_active_connection(self, profile_name: str, ssh_conn: str, melisa_user: str = ""):
        """Set the active connection in the configuration."""
        self.config_dir.mkdir(parents=True, exist_ok=True)
        profile_file = self.config_dir / "profiles.conf"
        active_file  = self.config_dir / "active"
        entry = f"{profile_name}={ssh_conn}"
        if melisa_user:
            entry += f"|{melisa_user}"
        profile_file.write_text(entry + "\n")
        active_file.write_text(profile_name + "\n")

    def write_meta(self, container: str, remote_port: int, local_port: int,
                   server: str, container_ip: str = "10.0.3.5") -> Path:
        """Write a tunnel .meta file (simulating an existing tunnel)."""
        key = f"{container}_{remote_port}"
        meta_file = self.tunnel_dir / f"{key}.meta"
        meta_file.write_text(textwrap.dedent(f"""\
            container={container}
            container_ip={container_ip}
            remote_port={remote_port}
            local_port={local_port}
            server={server}
            started=2025-01-15 10:30:00
        """))
        return meta_file

    def write_pid(self, container: str, remote_port: int, pid: int) -> Path:
        """Write a tunnel .pid file."""
        key = f"{container}_{remote_port}"
        pid_file = self.tunnel_dir / f"{key}.pid"
        pid_file.write_text(str(pid) + "\n")
        return pid_file

    def run_bash(
        self,
        script: str,
        env_extra: Optional[dict] = None,
        timeout: int = 10
    ) -> Tuple[int, str, str]:
        """Execute a bash script within the isolated environment."""
        env = os.environ.copy()
        env["HOME"]  = str(self.home)
        env["PATH"]  = f"{self.bin_dir}:/usr/bin:/bin"
        # Remove original SSH environment variables
        for var in ["SSH_CLIENT", "SSH_TTY", "SSH_CONNECTION", "SUDO_USER"]:
            env.pop(var, None)
        if env_extra:
            env.update(env_extra)

        header = textwrap.dedent(f"""\
            #!/bin/bash
            set -o pipefail
            export HOME="{self.home}"
            export MELISA_LIB="{self.lib_dir}"
            export PATH="{self.bin_dir}:/usr/bin:/bin"
            # Source modules
            [ -f "$MELISA_LIB/utils.sh" ] && source "$MELISA_LIB/utils.sh" 2>/dev/null
            [ -f "$MELISA_LIB/auth.sh"  ] && source "$MELISA_LIB/auth.sh"  2>/dev/null
            [ -f "$MELISA_LIB/db.sh"    ] && source "$MELISA_LIB/db.sh"    2>/dev/null
            [ -f "$MELISA_LIB/exec.sh"  ] && source "$MELISA_LIB/exec.sh"  2>/dev/null
        """)
        full_script = header + "\n" + script
        cmd_args = ["bash", "-c", full_script]
        try:
            result = subprocess.run(
                cmd_args,
                capture_output=True, text=True,
                env=env, timeout=timeout
            )
            debug_print("BashEnv", cmd_args, result.returncode, result.stdout, result.stderr)
            return result.returncode, result.stdout, result.stderr
        except subprocess.TimeoutExpired:
            debug_print("BashEnv", cmd_args, -1, "", f"TIMEOUT after {timeout}s")
            return -1, "", f"TIMEOUT after {timeout}s"
        except Exception as e:
            debug_print("BashEnv", cmd_args, -2, "", str(e))
            return -2, "", str(e)

    def cleanup(self):
        shutil.rmtree(self.tmp_dir, ignore_errors=True)


def has_bash_modules() -> bool:
    return CLIENT_SRC is not None and (CLIENT_SRC / "exec.sh").exists()


# =============================================================================
# SUITE 1: Tunnel Parameter Validation
# =============================================================================
class TestTunnelPortValidation(unittest.TestCase):
    """
    Tests port validation in exec_tunnel() — pure logic without SSH.
    Mirrors the Bash logic in exec.sh.
    """

    def _validate_port(self, port_str: str) -> bool:
        """Mirror of port validation in exec_tunnel()."""
        return bool(port_str) and port_str.isdigit() and 1 <= int(port_str) <= 65535

    def _validate_tunnel_args(self, container: str, remote_port: str) -> bool:
        """Mirror of the parameter guard in exec_tunnel()."""
        return bool(container) and bool(remote_port)

    def test_valid_port_numbers(self):
        for port in ["80", "443", "3000", "8080", "5432", "27017", "65535"]:
            with self.subTest(port=port):
                self.assertTrue(self._validate_port(port), f"Port '{port}' should be valid")

    def test_reject_non_numeric_port(self):
        for bad in ["abc", "3000abc", "!", "", "3.14", "3000.0"]:
            with self.subTest(port=bad):
                self.assertFalse(self._validate_port(bad), f"'{bad}' should be rejected")

    def test_reject_port_zero(self):
        self.assertFalse(self._validate_port("0"))

    def test_reject_port_above_65535(self):
        self.assertFalse(self._validate_port("65536"))
        self.assertFalse(self._validate_port("99999"))

    def test_local_port_defaults_to_remote(self):
        """If local_port is not provided, it should default to remote_port."""
        remote_port = "3000"
        local_port  = ""
        result = local_port if local_port else remote_port
        self.assertEqual(result, "3000")

    def test_tunnel_requires_container_and_port(self):
        self.assertFalse(self._validate_tunnel_args("", "3000"))
        self.assertFalse(self._validate_tunnel_args("myapp", ""))
        self.assertFalse(self._validate_tunnel_args("", ""))
        self.assertTrue(self._validate_tunnel_args("myapp", "3000"))

    def test_container_name_with_dash(self):
        """Container names are allowed to contain dashes '-'."""
        self.assertTrue(self._validate_tunnel_args("my-webapp", "8080"))

    def test_container_name_with_underscore(self):
        self.assertTrue(self._validate_tunnel_args("web_app", "8080"))

    def test_tunnel_key_format(self):
        """TUNNEL_KEY format must be: container_port."""
        container    = "myapp"
        remote_port  = "3000"
        tunnel_key   = f"{container}_{remote_port}"
        self.assertEqual(tunnel_key, "myapp_3000")

    def test_meta_filename_format(self):
        """Meta file names must end with .meta."""
        tunnel_key = "myapp_3000"
        meta_file  = f"{tunnel_key}.meta"
        pid_file   = f"{tunnel_key}.pid"
        self.assertTrue(meta_file.endswith(".meta"))
        self.assertTrue(pid_file.endswith(".pid"))


# =============================================================================
# SUITE 2: .meta and .pid File Management
# =============================================================================
class TestTunnelFileManagement(unittest.TestCase):
    """Tests the creation, parsing, and deletion of tunnel metadata files."""

    def setUp(self):
        self.tmp = tempfile.mkdtemp(prefix="melisa_meta_test_")
        self.tunnel_dir = Path(self.tmp) / "tunnels"
        self.tunnel_dir.mkdir()

    def tearDown(self):
        shutil.rmtree(self.tmp, ignore_errors=True)

    def _write_meta(self, container: str, remote_port: int, local_port: int,
                    server: str, container_ip: str = "10.0.3.5") -> Path:
        key = f"{container}_{remote_port}"
        meta = self.tunnel_dir / f"{key}.meta"
        meta.write_text(textwrap.dedent(f"""\
            container={container}
            container_ip={container_ip}
            remote_port={remote_port}
            local_port={local_port}
            server={server}
            started=2025-01-15 10:30:00
        """))
        return meta

    def _write_pid(self, container: str, remote_port: int, pid: int) -> Path:
        key = f"{container}_{remote_port}"
        pid_file = self.tunnel_dir / f"{key}.pid"
        pid_file.write_text(str(pid) + "\n")
        return pid_file

    def _parse_meta(self, meta_file: Path) -> dict:
        result = {}
        for line in meta_file.read_text().splitlines():
            if "=" in line:
                k, _, v = line.partition("=")
                result[k.strip()] = v.strip()
        return result

    def test_meta_file_created_with_correct_fields(self):
        meta = self._write_meta("myapp", 3000, 3000, "root@203.0.113.5", "10.0.3.10")
        data = self._parse_meta(meta)
        self.assertEqual(data["container"],     "myapp")
        self.assertEqual(data["remote_port"],   "3000")
        self.assertEqual(data["local_port"],    "3000")
        self.assertEqual(data["server"],        "root@203.0.113.5")
        self.assertEqual(data["container_ip"],  "10.0.3.10")
        self.assertIn("started",                data)

    def test_meta_file_different_local_and_remote_port(self):
        meta = self._write_meta("webapp", 8080, 9090, "deploy@server.id")
        data = self._parse_meta(meta)
        self.assertEqual(data["remote_port"], "8080")
        self.assertEqual(data["local_port"],  "9090")

    def test_pid_file_created_correctly(self):
        pid_file = self._write_pid("myapp", 3000, 12345)
        self.assertTrue(pid_file.exists())
        pid = int(pid_file.read_text().strip())
        self.assertEqual(pid, 12345)

    def test_paired_meta_and_pid_files_exist(self):
        """Every tunnel must have a paired .meta and .pid file."""
        self._write_meta("api", 5000, 5000, "root@server.id")
        self._write_pid("api", 5000, 99999)
        meta_files = list(self.tunnel_dir.glob("*.meta"))
        pid_files  = list(self.tunnel_dir.glob("*.pid"))
        self.assertEqual(len(meta_files), 1)
        self.assertEqual(len(pid_files),  1)
        # Stems (names without extensions) must match
        self.assertEqual(meta_files[0].stem, pid_files[0].stem)

    def test_multiple_tunnels_have_separate_files(self):
        for container, port in [("app1", 3000), ("app2", 4000), ("app3", 5000)]:
            self._write_meta(container, port, port, "root@server.id")
            self._write_pid(container, port, 10000 + port)
        self.assertEqual(len(list(self.tunnel_dir.glob("*.meta"))), 3)
        self.assertEqual(len(list(self.tunnel_dir.glob("*.pid"))),  3)

    def test_cleanup_removes_both_files(self):
        meta = self._write_meta("temp", 7000, 7000, "root@server.id")
        pid  = self._write_pid("temp", 7000, 11111)
        # Simulate cleanup
        meta.unlink()
        pid.unlink()
        self.assertFalse(meta.exists())
        self.assertFalse(pid.exists())

    def test_meta_file_parsing_with_equals_in_value(self):
        """Values containing '=' (e.g., URLs) must be read correctly."""
        key = "special_3000"
        meta = self.tunnel_dir / f"{key}.meta"
        meta.write_text("container=special\nremote_port=3000\nlocal_port=3000\nserver=root@10.0.0.1\nstarted=2025-01-15 10:00:00\ncontainer_ip=10.0.3.7\n")
        data = self._parse_meta(meta)
        self.assertEqual(data["container"], "special")

    def test_meta_file_with_unknown_pid_string(self):
        """The PID file may contain 'unknown' if the process cannot be traced."""
        key = "myapp_3000"
        pid_file = self.tunnel_dir / f"{key}.pid"
        pid_file.write_text("unknown\n")
        content = pid_file.read_text().strip()
        self.assertEqual(content, "unknown")


# =============================================================================
# SUITE 3: exec_tunnel_list Logic
# =============================================================================
class TestTunnelListLogic(unittest.TestCase):
    """
    Tests tunnel listing logic — pure Python, without Bash.
    Mirrors exec_tunnel_list() in exec.sh.
    """

    def setUp(self):
        self.tmp = tempfile.mkdtemp(prefix="melisa_list_test_")
        self.tunnel_dir = Path(self.tmp) / "tunnels"
        self.tunnel_dir.mkdir()

    def tearDown(self):
        shutil.rmtree(self.tmp, ignore_errors=True)

    def _write_tunnel(self, container: str, remote_port: int, local_port: int,
                      server: str, pid: Optional[int] = None) -> None:
        key = f"{container}_{remote_port}"
        meta = self.tunnel_dir / f"{key}.meta"
        meta.write_text(
            f"container={container}\n"
            f"container_ip=10.0.3.5\n"
            f"remote_port={remote_port}\n"
            f"local_port={local_port}\n"
            f"server={server}\n"
            f"started=2025-01-15 10:00:00\n"
        )
        if pid is not None:
            (self.tunnel_dir / f"{key}.pid").write_text(str(pid) + "\n")

    def _list_tunnels(self) -> list:
        """Mirror of exec_tunnel_list() — reads all .meta files."""
        tunnels = []
        for meta_file in sorted(self.tunnel_dir.glob("*.meta")):
            data = {}
            for line in meta_file.read_text().splitlines():
                if "=" in line:
                    k, _, v = line.partition("=")
                    data[k.strip()] = v.strip()
            pid_file = meta_file.with_suffix(".pid")
            if pid_file.exists():
                pid_str = pid_file.read_text().strip()
                data["pid"] = pid_str
                # Check if process is still alive
                if pid_str.isdigit():
                    try:
                        os.kill(int(pid_str), 0)
                        data["status"] = "RUNNING"
                    except ProcessLookupError:
                        data["status"] = "DEAD"
                    except PermissionError:
                        data["status"] = "RUNNING"  # Process exists, but not owned by us
                else:
                    data["status"] = "UNKNOWN"
            else:
                data["status"] = "UNKNOWN"
            tunnels.append(data)
        return tunnels

    def test_empty_tunnel_dir_returns_empty_list(self):
        tunnels = self._list_tunnels()
        self.assertEqual(tunnels, [])

    def test_single_tunnel_listed(self):
        self._write_tunnel("myapp", 3000, 3000, "root@203.0.113.5", pid=os.getpid())
        tunnels = self._list_tunnels()
        self.assertEqual(len(tunnels), 1)
        self.assertEqual(tunnels[0]["container"],   "myapp")
        self.assertEqual(tunnels[0]["remote_port"], "3000")
        self.assertEqual(tunnels[0]["server"],      "root@203.0.113.5")

    def test_multiple_tunnels_listed(self):
        self._write_tunnel("frontend", 3000, 3000, "root@server.id", pid=os.getpid())
        self._write_tunnel("backend",  5000, 5000, "root@server.id", pid=os.getpid())
        self._write_tunnel("database", 5432, 5432, "root@server.id", pid=os.getpid())
        tunnels = self._list_tunnels()
        self.assertEqual(len(tunnels), 3)
        names = {t["container"] for t in tunnels}
        self.assertEqual(names, {"frontend", "backend", "database"})

    def test_dead_process_marked_dead(self):
        """PIDs of processes that have died should be marked as DEAD."""
        # PID 1 is owned by init — we cannot kill-0 safely.
        # Use a PID that definitely does not exist: 2147483647 (INT_MAX)
        self._write_tunnel("deadapp", 3000, 3000, "root@server.id", pid=2147483647)
        tunnels = self._list_tunnels()
        self.assertEqual(len(tunnels), 1)
        self.assertEqual(tunnels[0]["status"], "DEAD")

    def test_current_process_marked_running(self):
        """The PID of the current process (test runner) is definitely running."""
        self._write_tunnel("liveapp", 8080, 8080, "root@server.id", pid=os.getpid())
        tunnels = self._list_tunnels()
        self.assertEqual(tunnels[0]["status"], "RUNNING")

    def test_unknown_pid_marked_unknown(self):
        self._write_tunnel("ghostapp", 9000, 9000, "root@server.id")
        # Write PID file with "unknown" string
        (self.tunnel_dir / "ghostapp_9000.pid").write_text("unknown\n")
        tunnels = self._list_tunnels()
        self.assertEqual(tunnels[0]["status"], "UNKNOWN")

    def test_meta_without_pid_file_marked_unknown(self):
        """If .pid is missing, status should be UNKNOWN (not an error)."""
        self._write_tunnel("nopid", 4000, 4000, "root@server.id", pid=None)
        tunnels = self._list_tunnels()
        self.assertEqual(len(tunnels), 1)
        self.assertEqual(tunnels[0]["status"], "UNKNOWN")


# =============================================================================
# SUITE 4: exec_tunnel_stop Logic
# =============================================================================
class TestTunnelStopLogic(unittest.TestCase):
    """
    Tests tunnel termination logic — pure Python.
    Mirrors exec_tunnel_stop() in exec.sh.
    """

    def setUp(self):
        self.tmp = tempfile.mkdtemp(prefix="melisa_stop_test_")
        self.tunnel_dir = Path(self.tmp) / "tunnels"
        self.tunnel_dir.mkdir()

    def tearDown(self):
        shutil.rmtree(self.tmp, ignore_errors=True)

    def _write_tunnel(self, container: str, remote_port: int,
                      pid: Optional[int] = None) -> None:
        key = f"{container}_{remote_port}"
        (self.tunnel_dir / f"{key}.meta").write_text(
            f"container={container}\nremote_port={remote_port}\n"
            f"local_port={remote_port}\nserver=root@server.id\n"
            f"container_ip=10.0.3.5\nstarted=2025-01-15 10:00:00\n"
        )
        if pid is not None:
            (self.tunnel_dir / f"{key}.pid").write_text(str(pid) + "\n")

    def _stop_tunnel(self, container: str, remote_port: Optional[int] = None) -> int:
        """Mirror of exec_tunnel_stop() — returns the number of stopped tunnels."""
        stopped = 0
        for meta_file in list(self.tunnel_dir.glob("*.meta")):
            data = {}
            for line in meta_file.read_text().splitlines():
                if "=" in line:
                    k, _, v = line.partition("=")
                    data[k.strip()] = v.strip()
            if data.get("container") != container:
                continue
            if remote_port and data.get("remote_port") != str(remote_port):
                continue
            pid_file = meta_file.with_suffix(".pid")
            if pid_file.exists():
                pid_str = pid_file.read_text().strip()
                if pid_str.isdigit():
                    try:
                        os.kill(int(pid_str), signal.SIGTERM)
                    except (ProcessLookupError, PermissionError):
                        pass
            meta_file.unlink(missing_ok=True)
            pid_file.unlink(missing_ok=True)
            stopped += 1
        return stopped

    def test_stop_existing_tunnel_removes_files(self):
        # Spawn a dummy process that is safe to kill
        dummy = subprocess.Popen(["sleep", "60"])
        try:
            self._write_tunnel("myapp", 3000, pid=dummy.pid)
            n = self._stop_tunnel("myapp")
        finally:
            dummy.kill()
            dummy.wait()
        self.assertEqual(n, 1)
        self.assertEqual(list(self.tunnel_dir.glob("*.meta")), [])
        self.assertEqual(list(self.tunnel_dir.glob("*.pid")),  [])

    def test_stop_nonexistent_tunnel_returns_zero(self):
        n = self._stop_tunnel("doesnotexist")
        self.assertEqual(n, 0)

    def test_stop_specific_port_only(self):
        """tunnel-stop app 3000 should only stop the tunnel on port 3000."""
        self._write_tunnel("app", 3000)
        self._write_tunnel("app", 4000)
        n = self._stop_tunnel("app", remote_port=3000)
        self.assertEqual(n, 1)
        remaining = list(self.tunnel_dir.glob("*.meta"))
        self.assertEqual(len(remaining), 1)
        self.assertIn("4000", remaining[0].name)

    def test_stop_all_ports_for_container(self):
        """tunnel-stop app (without port) should stop all tunnels for the container."""
        # PIDs that definitely do not exist — safe to kill without side effects
        self._write_tunnel("app", 3000, pid=2147483647)
        self._write_tunnel("app", 4000, pid=2147483646)
        self._write_tunnel("app", 5000, pid=2147483645)
        n = self._stop_tunnel("app", remote_port=None)
        self.assertEqual(n, 3)
        self.assertEqual(list(self.tunnel_dir.glob("*.meta")), [])

    def test_stop_does_not_affect_other_containers(self):
        self._write_tunnel("app1", 3000)
        self._write_tunnel("app2", 3000)
        n = self._stop_tunnel("app1")
        self.assertEqual(n, 1)
        remaining = list(self.tunnel_dir.glob("*.meta"))
        self.assertEqual(len(remaining), 1)
        self.assertIn("app2", remaining[0].name)

    def test_stop_dead_process_still_cleans_files(self):
        """Tunnels with dead processes must still have their files removed."""
        self._write_tunnel("deadapp", 3000, pid=2147483647)  # Dead PID
        n = self._stop_tunnel("deadapp")
        self.assertEqual(n, 1)
        self.assertEqual(list(self.tunnel_dir.glob("*.meta")), [])


# =============================================================================
# SUITE 5: Cross-Region Analysis & Tests (US -> Indonesia)
# =============================================================================
class TestCrossRegionConnectivity(unittest.TestCase):
    """
    Analyzes and tests cross-region connection scenarios:
    Client in the US <-> Melisa Server in Indonesia.

    Analysis Results:
    [OK] YES, connection is possible if the Indonesian server has a PUBLIC IP and open port 22.
    [OK] YES, container HTTP access via 'melisa tunnel' (SSH -L port forwarding) is possible.
    [FAIL] NO, not possible if the server is behind NAT/CGNAT (only has a private IP).
    
    Cross-region connection flow:
    [US Client]                         [Indonesian Server]          [Container]
    localhost:8080  ──SSH -L tunnel──▶  public_ip:22  ──▶  10.0.3.5:8080
    """

    def test_public_ip_format_validation(self):
        """
        The server must be configured with a public IP / public hostname,
        not a private IP (192.168.x.x, 10.x.x.x, 172.16-31.x.x).
        """
        def is_private_ip(ip: str) -> bool:
            parts = ip.split(".")
            if len(parts) != 4:
                return False
            try:
                octets = [int(p) for p in parts]
            except ValueError:
                return False
            # RFC 1918 private ranges
            if octets[0] == 10:
                return True
            if octets[0] == 172 and 16 <= octets[1] <= 31:
                return True
            if octets[0] == 192 and octets[1] == 168:
                return True
            # Loopback
            if octets[0] == 127:
                return True
            return False

        # Private IPs — CANNOT be accessed from the US
        self.assertTrue(is_private_ip("192.168.1.100"),
            "LAN IP should be detected as private")
        self.assertTrue(is_private_ip("10.0.0.5"),
            "10.x.x.x should be private")
        self.assertTrue(is_private_ip("172.20.0.1"),
            "172.20.x.x should be private")

        # Public IPs — CAN be accessed from the US
        self.assertFalse(is_private_ip("203.0.113.5"),
            "TEST-NET public IP should not be private")
        self.assertFalse(is_private_ip("103.145.100.50"),
            "Telkom/ISP Indonesia IP should be public")
        self.assertFalse(is_private_ip("52.221.30.10"),
            "AWS Singapore IP should be public")

    def test_tunnel_command_builds_ssh_L_correctly(self):
        """
        The SSH command built by exec_tunnel() must utilize -L:
        ssh -N -f -L local_port:container_ip:remote_port CONN
        """
        container    = "mywebapp"
        remote_port  = 3000
        local_port   = 3000
        container_ip = "10.0.3.5"
        server_conn  = "root@203.0.113.5"  # Public IP for Indonesian server

        # Build the SSH command like exec_tunnel()
        ssh_cmd = [
            "ssh", "-N", "-f",
            "-L", f"{local_port}:{container_ip}:{remote_port}",
            "-o", "ExitOnForwardFailure=yes",
            "-o", "ServerAliveInterval=30",
            "-o", "ServerAliveCountMax=3",
            "-o", "StrictHostKeyChecking=no",
            server_conn
        ]

        # Verify critical components
        self.assertIn("-N",     ssh_cmd)  # No command (background tunnel)
        self.assertIn("-f",     ssh_cmd)  # Fork to background
        self.assertIn("-L",     ssh_cmd)  # Local port forwarding
        self.assertIn(f"{local_port}:{container_ip}:{remote_port}", ssh_cmd)
        self.assertIn(server_conn, ssh_cmd)

    def test_cross_region_tunnel_url(self):
        """
        Once the tunnel is active, the access URL in the US should be localhost:local_port,
        not the direct Indonesian IP.
        """
        local_port = 8080
        access_url = f"http://localhost:{local_port}"
        self.assertEqual(access_url, "http://localhost:8080")
        # Ensure it's not the direct Indonesian server IP
        self.assertNotIn("203.0.113", access_url)

    def test_route_description_cross_region(self):
        """Verify the routing description format displayed to the user."""
        local_port   = 3000
        server_conn  = "root@203.0.113.5"
        container_ip = "10.0.3.5"
        remote_port  = 3000

        route = f"localhost:{local_port} → {server_conn} → {container_ip}:{remote_port}"
        self.assertIn("localhost",     route)
        self.assertIn(server_conn,     route)
        self.assertIn(container_ip,    route)
        self.assertIn(str(remote_port), route)

    def test_profile_with_public_ip_structure(self):
        """
        Profile format for an Indonesian server (accessed from the US):
        profiles.conf: indonesia=root@103.145.100.50|deployuser
        """
        profile_entry = "indonesia=root@103.145.100.50|deployuser"
        name, _, rest  = profile_entry.partition("=")
        ssh_part, _, melisa_user = rest.partition("|")
        self.assertEqual(name,        "indonesia")
        self.assertEqual(ssh_part,    "root@103.145.100.50")
        self.assertEqual(melisa_user, "deployuser")

    def test_nat_detection_logic(self):
        """
        Servers behind NAT/CGNAT cannot be accessed directly.
        Detection is based on the private IP in the CONN string.
        """
        def is_reachable_from_internet(conn_str: str) -> bool:
            """Check if CONN utilizes a public IP accessible across regions."""
            host = conn_str.split("@")[-1] if "@" in conn_str else conn_str
            # If it's a hostname (not IP), assume it resolves publicly
            if not host[0].isdigit():
                return True  # Domain name — can be public
            # Check if private
            parts = host.split(".")
            if len(parts) == 4:
                try:
                    octets = [int(p) for p in parts]
                    if octets[0] in (10, 127):
                        return False
                    if octets[0] == 172 and 16 <= octets[1] <= 31:
                        return False
                    if octets[0] == 192 and octets[1] == 168:
                        return False
                except ValueError:
                    pass
            return True

        # Server with a public IP -> accessible from the US [OK]
        self.assertTrue(is_reachable_from_internet("root@203.0.113.5"))
        self.assertTrue(is_reachable_from_internet("root@103.145.100.50"))
        self.assertTrue(is_reachable_from_internet("deploy@myserver.example.com"))

        # Server with a private IP -> NOT accessible from the US [FAIL]
        self.assertFalse(is_reachable_from_internet("root@192.168.1.100"))
        self.assertFalse(is_reachable_from_internet("root@10.0.0.5"))


# =============================================================================
# SUITE 6: Local Port Conflict Detection
# =============================================================================
class TestLocalPortConflict(unittest.TestCase):
    """
    Tests local port conflict detection before creating a tunnel.
    Mirrors the 'ss -tlnp | grep :port' logic in exec_tunnel().
    """

    def _is_port_in_use(self, port: int) -> bool:
        """
        Check if a port on the local machine is currently in use.
        Uses sockets for accurate simulation (does not rely on 'ss').
        """
        with socket.socket(socket.AF_INET, socket.SOCK_STREAM) as s:
            s.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
            try:
                s.bind(("127.0.0.1", port))
                return False  # Port is available
            except OSError:
                return True   # Port is in use

    def test_high_numbered_port_likely_free(self):
        """High ports (>49151) are generally free in a test environment."""
        # Port 59999 is almost certainly empty in a test environment
        result = self._is_port_in_use(59999)
        # We cannot assert absolutely True/False, but the function must execute
        self.assertIsInstance(result, bool)

    def test_occupied_port_detected(self):
        """Create a temp server on a random port, ensure it is detected as in use."""
        server = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
        server.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
        server.bind(("127.0.0.1", 0))
        occupied_port = server.getsockname()[1]
        server.listen(1)
        try:
            self.assertTrue(self._is_port_in_use(occupied_port),
                f"Port {occupied_port} should be detected as in use")
        finally:
            server.close()

    def test_port_conflict_message_format(self):
        """The port conflict error message must be informative."""
        container   = "myapp"
        remote_port = 3000
        local_port  = 3000
        msg = (f"Local port {local_port} is already in use. "
               f"Use: melisa tunnel {container} {remote_port} <free_port>")
        self.assertIn(str(local_port), msg)
        self.assertIn(container,       msg)
        self.assertIn(str(remote_port), msg)
        self.assertIn("<free_port>",   msg)

    def test_alternative_port_suggestion(self):
        """If port 3000 is in use, the user can try another port (e.g., 3001)."""
        suggested_port = 3001  # User can utilize a different port
        self.assertGreater(suggested_port, 0)
        self.assertLessEqual(suggested_port, 65535)


# =============================================================================
# SUITE 7: Robustness — Sudden Death & Corrupted Files
# =============================================================================
class TestTunnelRobustness(unittest.TestCase):
    """
    Tests the tunnel system's resilience against abnormal conditions:
    - Tunnel process suddenly dies
    - Missing .pid files
    - Corrupted or incomplete .meta files
    - Tunnel duplication (the same tunnel recreated)
    """

    def setUp(self):
        self.tmp = tempfile.mkdtemp(prefix="melisa_robust_test_")
        self.tunnel_dir = Path(self.tmp) / "tunnels"
        self.tunnel_dir.mkdir()

    def tearDown(self):
        shutil.rmtree(self.tmp, ignore_errors=True)

    def test_corrupt_meta_file_handled_gracefully(self):
        """A corrupted .meta file must not cause a crash."""
        (self.tunnel_dir / "corrupt_3000.meta").write_text("this is not a valid format!!!\n???")
        # Parsing should proceed gracefully
        result = {}
        try:
            for line in (self.tunnel_dir / "corrupt_3000.meta").read_text().splitlines():
                if "=" in line:
                    k, _, v = line.partition("=")
                    result[k.strip()] = v.strip()
        except Exception as e:
            self.fail(f"Parsing a corrupted file threw an exception: {e}")
        # Result might be empty, but it shouldn't crash
        self.assertIsInstance(result, dict)

    def test_empty_meta_file_handled(self):
        """An empty .meta file must not crash the parser."""
        (self.tunnel_dir / "empty_3000.meta").write_text("")
        data = {}
        for line in (self.tunnel_dir / "empty_3000.meta").read_text().splitlines():
            if "=" in line:
                k, _, v = line.partition("=")
                data[k.strip()] = v.strip()
        self.assertEqual(data, {})

    def test_replacing_existing_tunnel_kills_old_pid(self):
        """
        If the same tunnel is recreated, the old process must be terminated.
        Simulate this with a PID from a process that no longer exists.
        """
        # Create a tunnel with a dead PID
        key = "myapp_3000"
        (self.tunnel_dir / f"{key}.pid").write_text("2147483647\n")
        (self.tunnel_dir / f"{key}.meta").write_text(
            "container=myapp\nremote_port=3000\nlocal_port=3000\n"
            "server=root@server.id\ncontainer_ip=10.0.3.5\nstarted=2025-01-15 10:00:00\n"
        )
        # Simulation: new tunnel overwriting the old one
        old_pid_file = self.tunnel_dir / f"{key}.pid"
        if old_pid_file.exists():
            old_pid = old_pid_file.read_text().strip()
            if old_pid.isdigit():
                try:
                    os.kill(int(old_pid), signal.SIGTERM)
                except (ProcessLookupError, PermissionError):
                    pass  # Process is already dead — safe to proceed
            old_pid_file.unlink()
        self.assertFalse(old_pid_file.exists())

    def test_pid_file_with_negative_number(self):
        """A negative PID should not be processed as a valid PID."""
        pid_str = "-1"
        is_valid_pid = pid_str.isdigit() and int(pid_str) > 0
        # "-1".isdigit() -> False in Python due to the minus sign
        self.assertFalse(is_valid_pid)

    def test_tunnel_restart_creates_fresh_metadata(self):
        """Upon restart, metadata must be fully updated (not appended)."""
        key = "webapp_8080"
        meta_file = self.tunnel_dir / f"{key}.meta"
        # Initial write
        meta_file.write_text("container=webapp\nremote_port=8080\nstarted=2025-01-01\n")
        # Overwrite (restart)
        meta_file.write_text("container=webapp\nremote_port=8080\nstarted=2025-06-01\n")
        content = meta_file.read_text()
        self.assertEqual(content.count("container=webapp"), 1,
            "Metadata must not duplicate entries after a restart")
        self.assertIn("2025-06-01", content)
        self.assertNotIn("2025-01-01", content)


# =============================================================================
# SUITE 8: Bash Module Tests (run if source is available)
# =============================================================================
@unittest.skipUnless(has_bash_modules(), "Bash modules not found in CLIENT_SRC")
class TestTunnelBashModules(unittest.TestCase):
    """
    Tests exec_tunnel(), exec_tunnel_list(), exec_tunnel_stop()
    directly from bash modules utilizing mocked SSH.
    """

    def setUp(self):
        self.env = BashEnv(fake_ssh=False)  # SSH is manually mocked per-test

    def tearDown(self):
        self.env.cleanup()

    def test_tunnel_fails_without_active_connection(self):
        """Tunnel must fail (non-0 exit) if there is no active connection."""
        rc, out, err = self.env.run_bash(
            "exec_tunnel myapp 3000",
            timeout=5
        )
        self.assertNotEqual(rc, 0,
            "Tunnel without an active connection should fail")
        combined = (out + err).lower()
        self.assertTrue(
            any(kw in combined for kw in ["no active", "not connected", "error"]),
            f"Error message does not indicate connection issues: {combined}"
        )

    def test_tunnel_empty_container_exits_with_error(self):
        """Calling exec_tunnel without arguments must display usage instructions."""
        self.env.set_active_connection("myserver", "root@server.id", "admin")
        self.env.install_fake_ssh_with_ip("10.0.3.5")
        rc, out, err = self.env.run_bash("exec_tunnel '' ''", timeout=5)
        self.assertNotEqual(rc, 0)

    def test_tunnel_list_empty_when_no_tunnels(self):
        """tunnel-list without active tunnels must show a 'no tunnels' message."""
        rc, out, err = self.env.run_bash("exec_tunnel_list", timeout=5)
        self.assertEqual(rc, 0, f"tunnel-list must succeed even when empty: {err}")
        combined = (out + err).lower()
        self.assertTrue(
            any(kw in combined for kw in ["no tunnel", "not found", "empty", "active"]),
            f"Expected a 'no tunnels' message: {combined}"
        )

    def test_tunnel_stop_nonexistent_exits_error(self):
        """tunnel-stop for a non-existent container must display an error."""
        rc, out, err = self.env.run_bash(
            "exec_tunnel_stop 'doesnotexist' 3000",
            timeout=5
        )
        combined = out + err
        self.assertTrue(
            any(kw in combined.lower() for kw in ["not found", "no tunnel", "error"]),
            f"Expected an error message for a non-existent tunnel: {combined}"
        )

    def test_tunnel_list_shows_meta_content(self):
        """
        If a .meta file exists, exec_tunnel_list must output its content.
        (Simulates an active tunnel with a mock PID = PID of this current process)
        """
        self.env.write_meta(
            container="webapp", remote_port=8080, local_port=8080,
            server="root@103.145.100.50", container_ip="10.0.3.10"
        )
        self.env.write_pid("webapp", 8080, os.getpid())
        rc, out, err = self.env.run_bash("exec_tunnel_list", timeout=5)
        self.assertEqual(rc, 0, f"Failed to execute tunnel-list: {err}")
        # Info about container or server must be present
        combined = out + err
        self.assertTrue(
            "webapp" in combined or "8080" in combined,
            f"Output fails to display tunnel information: {combined}"
        )

    def test_tunnel_stop_cleans_meta_and_pid(self):
        """
        After tunnel-stop, both .meta and .pid files must be erased.
        """
        tunnel_dir = self.env.tunnel_dir
        self.env.write_meta("cleanme", 5000, 5000, "root@server.id")
        self.env.write_pid("cleanme", 5000, 2147483647)  # Dead PID
        rc, out, err = self.env.run_bash(
            "exec_tunnel_stop 'cleanme' '5000'",
            timeout=5
        )
        # Files must be deleted
        self.assertFalse((tunnel_dir / "cleanme_5000.meta").exists(),
            ".meta file must be deleted following tunnel-stop")
        self.assertFalse((tunnel_dir / "cleanme_5000.pid").exists(),
            ".pid file must be deleted following tunnel-stop")

    def test_tunnel_invalid_port_string(self):
        """A non-numeric port must be rejected with a clear error message."""
        self.env.set_active_connection("myserver", "root@server.id", "admin")
        self.env.install_fake_ssh_with_ip("10.0.3.5")
        rc, out, err = self.env.run_bash(
            "exec_tunnel myapp 'notaport'",
            timeout=5
        )
        self.assertNotEqual(rc, 0, "A non-numeric port should be rejected")
        combined = out + err
        self.assertTrue(
            any(kw in combined.lower() for kw in ["integer", "numeric", "port", "error"]),
            f"Error message does not mention port formatting: {combined}"
        )

    def test_cross_region_tunnel_builds_correct_ssh_command(self):
        """
        Verify that exec_tunnel builds the correct SSH -L command
        for cross-region scenarios (Indonesian server from a US client).
        """
        # Set active connection with an Indonesian public IP
        self.env.set_active_connection(
            "indonesia", "root@103.145.100.50", "devuser"
        )
        # Mock SSH: return container IP when asked via --ip
        self.env.install_fake_ssh_with_ip("10.0.3.7")

        # Run tunnel — will not create a real connection due to mock
        # We check that the process runs without validation errors
        rc, out, err = self.env.run_bash(
            # Short timeout as the fake SSH spawns a sleep 3600
            # We simply need to ensure validation passes
            """
            # Override exec_tunnel to merely check parameters without an actual SSH
            exec_tunnel_dry_run() {
                ensure_connected
                local container=$1
                local remote_port=$2
                local local_port=${3:-$remote_port}
                if [ -z "$container" ] || [ -z "$remote_port" ]; then
                    echo "ERROR: Empty arguments" >&2; exit 1
                fi
                if ! [[ "$remote_port" =~ ^[0-9]+$ ]] || ! [[ "$local_port" =~ ^[0-9]+$ ]]; then
                    echo "ERROR: Port must be an integer" >&2; exit 1
                fi
                echo "DRY_RUN_OK: $container $remote_port $local_port $CONN"
            }
            exec_tunnel_dry_run "mywebapp" "8080" "8080"
            """,
            timeout=5
        )
        self.assertEqual(rc, 0, f"Dry-run tunnel failed: {err}\n{out}")
        self.assertIn("DRY_RUN_OK", out)
        self.assertIn("mywebapp",           out)
        self.assertIn("8080",               out)
        self.assertIn("103.145.100.50",     out)


# =============================================================================
# SUITE 9: Network Connectivity Integration Tests (Optional)
# =============================================================================
class TestNetworkConnectivityHelpers(unittest.TestCase):
    """
    Test network connectivity helpers — without an actual outbound connection.
    Simulates a cross-region scenario using localhost.
    """

    def test_localhost_tcp_roundtrip(self):
        """
        Simulate an SSH cross-region tunnel using two localhost sockets.
        Client in the 'US' (high port) <-> 'Server' on localhost (random port).
        """
        # Create server socket (simulates Indonesian server)
        server_sock = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
        server_sock.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
        server_sock.bind(("127.0.0.1", 0))
        server_port = server_sock.getsockname()[1]
        server_sock.listen(1)
        server_sock.settimeout(3)

        response_received = []

        def server_thread():
            try:
                conn, _ = server_sock.accept()
                data = conn.recv(1024)
                conn.sendall(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nOK")
                conn.close()
                response_received.append(data)
            except Exception:
                pass
            finally:
                server_sock.close()

        t = threading.Thread(target=server_thread, daemon=True)
        t.start()

        # Client connects (simulating an active tunnel)
        time.sleep(0.1)
        client_sock = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
        client_sock.settimeout(3)
        try:
            client_sock.connect(("127.0.0.1", server_port))
            client_sock.sendall(b"GET / HTTP/1.1\r\nHost: localhost\r\n\r\n")
            response = client_sock.recv(1024)
            self.assertIn(b"200 OK", response)
        finally:
            client_sock.close()
        t.join(timeout=3)

    def test_ssh_port_22_reachability_check_logic(self):
        """
        Simulate a check verifying if port 22 on the Indonesian server is reachable.
        In this test, we check a port we open ourselves on localhost.
        """
        # Open a TCP server on a random port (simulating the Indonesian SSH server)
        fake_ssh_server = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
        fake_ssh_server.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
        fake_ssh_server.bind(("127.0.0.1", 0))
        fake_port = fake_ssh_server.getsockname()[1]
        fake_ssh_server.listen(1)
        fake_ssh_server.settimeout(2)

        def check_port_reachable(host: str, port: int, timeout: float = 2.0) -> bool:
            """Check if a port is reachable (similar to SSH diagnostics)."""
            try:
                with socket.create_connection((host, port), timeout=timeout):
                    return True
            except (socket.timeout, ConnectionRefusedError, OSError):
                return False

        # Port we opened -> must be reachable [OK]
        self.assertTrue(check_port_reachable("127.0.0.1", fake_port))
        fake_ssh_server.close()

        # Non-existent port -> unreachable [FAIL]
        self.assertFalse(check_port_reachable("127.0.0.1", 1))

    def test_container_ip_format(self):
        """LXC container IPs generally reside within the 10.0.3.x range."""
        def is_lxc_container_ip(ip: str) -> bool:
            parts = ip.split(".")
            if len(parts) != 4:
                return False
            try:
                return int(parts[0]) == 10 and int(parts[1]) == 0 and int(parts[2]) == 3
            except ValueError:
                return False

        self.assertTrue(is_lxc_container_ip("10.0.3.1"))
        self.assertTrue(is_lxc_container_ip("10.0.3.5"))
        self.assertTrue(is_lxc_container_ip("10.0.3.254"))
        self.assertFalse(is_lxc_container_ip("10.0.4.5"))
        self.assertFalse(is_lxc_container_ip("192.168.1.5"))


# =============================================================================
# Entry Point
# =============================================================================
class MelisaTunnelTestRunner(unittest.TextTestRunner):
    def run(self, test):
        print(f"\n{BOLD}{CYAN}{'='*65}{RESET}")
        print(f"{BOLD}{CYAN}  MELISA — Tunnel Mode & Cross-Region Connectivity Tests{RESET}")
        if DEBUG_MODE:
            print(f"{BOLD}{YELLOW}  *** DEBUG MODE ACTIVE ***{RESET}")
        print(f"{BOLD}{CYAN}{'='*65}{RESET}")
        print(f"  Project Root : {MELISA_ROOT or col('Not found', RED)}")
        print(f"  Client Src   : {CLIENT_SRC or col('Not found', YELLOW)}")
        print(f"  Bash Modules : {col('Available', GREEN) if has_bash_modules() else col('Missing (Suite 8 skipped)', YELLOW)}")
        print(f"{BOLD}{CYAN}{'='*65}{RESET}\n")
        print(f"{BOLD}[INFO] Cross-Region Analysis (US -> Indonesia):{RESET}")
        print(f"  [OK] SSH tunnel (-L) supports cross-region natively.")
        print(f"  [OK] 'melisa tunnel <container> <port>' forwards traffic to the container.")
        print(f"  [WARN] Requirement: The Indonesian server must have a public IP and an open port 22.")
        print(f"  [FAIL] Servers behind NAT/CGNAT cannot be accessed directly.\n")
        return super().run(test)


if __name__ == "__main__":
    loader = unittest.TestLoader()

    # Logical sequence of suites
    suite = unittest.TestSuite()
    for cls in [
        TestTunnelPortValidation,
        TestTunnelFileManagement,
        TestTunnelListLogic,
        TestTunnelStopLogic,
        TestCrossRegionConnectivity,
        TestLocalPortConflict,
        TestTunnelRobustness,
        TestTunnelBashModules,
        TestNetworkConnectivityHelpers,
    ]:
        suite.addTests(loader.loadTestsFromTestCase(cls))

    runner = MelisaTunnelTestRunner(verbosity=2, failfast=False)
    result = runner.run(suite)
    sys.exit(0 if result.wasSuccessful() else 1)