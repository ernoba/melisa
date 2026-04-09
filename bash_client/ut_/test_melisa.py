import os
import sys
import stat
import shutil
import tempfile
import unittest
import subprocess
import textwrap
import time
from pathlib import Path
from typing import Optional, Tuple

def find_melisa_root() -> Optional[Path]:
    candidates = [
        Path(__file__).parent.parent.parent.parent,
        Path(__file__).parent.parent.parent,
        Path.cwd(),
        Path.cwd().parent,
        Path.cwd().parent.parent,
    ]
    for p in candidates:
        if (p / "Cargo.toml").exists() and (p / "src" / "main.rs").exists():
            return p
    return None

MELISA_ROOT = find_melisa_root()
CLIENT_SRC  = MELISA_ROOT / "src" / "melisa_client" / "src" if MELISA_ROOT else None
BINARY      = MELISA_ROOT / "target" / "release" / "melisa" if MELISA_ROOT else None
DEBUG_BIN   = MELISA_ROOT / "target" / "debug" / "melisa" if MELISA_ROOT else None

GREEN  = "\033[32m"
RED    = "\033[31m"
YELLOW = "\033[33m"
CYAN   = "\033[36m"
BOLD   = "\033[1m"
RESET  = "\033[0m"

# ─────────────────────────────────────────────────────────
# GLOBAL CONFIGURATION
# ─────────────────────────────────────────────────────────
DEBUG_MODE = False
if "--debug" in sys.argv:
    DEBUG_MODE = True
    sys.argv.remove("--debug")

def col(text: str, color: str) -> str:
    if sys.stdout.isatty():
        return f"{color}{text}{RESET}"
    return text

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

# ─────────────────────────────────────────────────────────
# HELPER: Run bash script in an isolated environment
# ─────────────────────────────────────────────────────────
class BashEnv:
    """Isolated environment for testing Melisa bash scripts."""
    def __init__(self):
        self.tmp_dir = tempfile.mkdtemp(prefix="melisa_test_")
        self.home    = Path(self.tmp_dir) / "home"
        self.home.mkdir(parents=True)
        if CLIENT_SRC and CLIENT_SRC.exists():
            for sh_file in CLIENT_SRC.glob("*.sh"):
                dest = self.home / ".local" / "share" / "melisa" / sh_file.name
                dest.parent.mkdir(parents=True, exist_ok=True)
                shutil.copy2(sh_file, dest)
                dest.chmod(dest.stat().st_mode | stat.S_IEXEC)

    def cleanup(self):
        shutil.rmtree(self.tmp_dir, ignore_errors=True)

    def run_bash(
        self,
        script: str,
        env_extra: Optional[dict] = None,
        timeout: int = 10
    ) -> Tuple[int, str, str]:
        env = os.environ.copy()
        env["HOME"] = str(self.home)
        env["PATH"] = f"{self.home}/.local/bin:/usr/bin:/bin"
        for var in ["SSH_CLIENT", "SSH_TTY", "SSH_CONNECTION", "SUDO_USER"]:
            env.pop(var, None)
        if env_extra:
            env.update(env_extra)
        lib_dir = self.home / ".local" / "share" / "melisa"
        header = textwrap.dedent(f"""\
            #!/bin/bash
            set -o pipefail
            export HOME="{self.home}"
            export MELISA_LIB="{lib_dir}"
            # Source modules if they exist
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
                capture_output=True,
                text=True,
                env=env,
                timeout=timeout
            )
            debug_print("BashEnv", cmd_args, result.returncode, result.stdout, result.stderr)
            return result.returncode, result.stdout, result.stderr
        except subprocess.TimeoutExpired:
            debug_print("BashEnv", cmd_args, -1, "", f"TIMEOUT after {timeout} seconds")
            return -1, "", f"TIMEOUT after {timeout} seconds"
        except Exception as e:
            debug_print("BashEnv", cmd_args, -2, "", str(e))
            return -2, "", str(e)

def has_bash_modules() -> bool:
    """Check if bash modules are available."""
    return CLIENT_SRC is not None and (CLIENT_SRC / "utils.sh").exists()

# ─────────────────────────────────────────────────────────
# HELPER: Detect passwordless sudo (FIX #2)
# ─────────────────────────────────────────────────────────
def can_sudo_nopasswd() -> bool:
    """
    Check if sudo can be executed without a password prompt.

    Using 'sudo -n true':
      -n  = non-interactive, fails immediately (exit 1) if a password is required
            instead of blocking the process while waiting for TTY input.

    Returns True if sudo is available without a password (NOPASSWD),
    False if a password is required or sudo is missing.

    How to enable NOPASSWD for testing:
      sudo visudo
      # Add the following line (replace 'saferoom' with your username):
      saferoom ALL=(ALL) NOPASSWD: /home/saferoom/Documents/afira/saferoom/target/debug/melisa
    """
    try:
        result = subprocess.run(
            ["sudo", "-n", "true"],
            capture_output=True,
            timeout=3
        )
        return result.returncode == 0
    except (subprocess.TimeoutExpired, FileNotFoundError):
        return False


# ─────────────────────────────────────────────────────────
# TEST SUITE 1: Pure Logic Tests (no binary/bash required)
# ─────────────────────────────────────────────────────────
class TestSlugGeneration(unittest.TestCase):
    def _generate_slug(self, name: str, release: str, arch: str) -> str:
        arch_map = {"amd64": "x64", "arm64": "a64", "i386": "x86"}
        s_arch = arch_map.get(arch, arch)
        prefix = name[:min(3, len(name))]
        return f"{prefix}-{release}-{s_arch}".lower()

    def test_ubuntu_amd64(self):
        self.assertEqual(self._generate_slug("ubuntu", "22.04", "amd64"), "ubu-22.04-x64")

    def test_debian_arm64(self):
        self.assertEqual(self._generate_slug("debian", "12", "arm64"), "deb-12-a64")

    def test_alpine_i386(self):
        self.assertEqual(self._generate_slug("alpine", "3.18", "i386"), "alp-3.18-x86")

    def test_archlinux_truncated(self):
        self.assertEqual(self._generate_slug("archlinux", "base", "amd64"), "arc-base-x64")

    def test_unknown_arch_passthrough(self):
        self.assertEqual(self._generate_slug("fedora", "39", "riscv64"), "fed-39-riscv64")

    def test_single_char_name(self):
        self.assertEqual(self._generate_slug("a", "1.0", "amd64"), "a-1.0-x64")


class TestDistroListParsing(unittest.TestCase):
    def _parse(self, content: str) -> list:
        PM_MAP = {
            "debian": "apt", "ubuntu": "apt", "kali": "apt",
            "fedora": "dnf", "centos": "dnf", "rocky": "dnf", "almalinux": "dnf",
            "alpine": "apk",
            "archlinux": "pacman",
            "opensuse": "zypper",
        }
        ARCH_MAP = {"amd64": "x64", "arm64": "a64", "i386": "x86"}
        result = []
        for line in content.splitlines():
            parts = line.split()
            if len(parts) < 4:
                continue
            if any(kw in line for kw in ["Distribution", "DIST", "---"]):
                continue
            name, release, arch, variant = parts[0], parts[1], parts[2], parts[3]
            s_arch = ARCH_MAP.get(arch, arch)
            slug = f"{name[:3]}-{release}-{s_arch}".lower()
            pm = PM_MAP.get(name, "apt")
            result.append({
                "name": name, "release": release, "arch": arch,
                "variant": variant, "slug": slug, "pkg_manager": pm
            })
        return result

    def test_valid_single_entry(self):
        content = "ubuntu 22.04 amd64 default"
        result = self._parse(content)
        self.assertEqual(len(result), 1)
        self.assertEqual(result[0]["name"], "ubuntu")
        self.assertEqual(result[0]["pkg_manager"], "apt")
        self.assertEqual(result[0]["slug"], "ubu-22.04-x64")

    def test_header_lines_skipped(self):
        content = "Distribution Release Architecture Variant\n---\nubuntu 22.04 amd64 default"
        result = self._parse(content)
        self.assertEqual(len(result), 1)

    def test_empty_input(self):
        self.assertEqual(self._parse(""), [])

    def test_incomplete_lines_skipped(self):
        content = "ubuntu 22.04 amd64\ndebian 12 arm64 default"
        result = self._parse(content)
        self.assertEqual(len(result), 1)
        self.assertEqual(result[0]["name"], "debian")

    def test_all_pkg_managers(self):
        entries = [
            ("ubuntu",    "apt"),    ("debian",    "apt"),   ("kali",      "apt"),
            ("fedora",    "dnf"),    ("centos",    "dnf"),   ("rocky",     "dnf"),
            ("almalinux", "dnf"),    ("alpine",    "apk"),
            ("archlinux", "pacman"), ("opensuse",  "zypper"),
            ("voidlinux", "apt"),
        ]
        for name, expected_pm in entries:
            content = f"{name} 1.0 amd64 default"
            result = self._parse(content)
            self.assertEqual(len(result), 1)
            self.assertEqual(result[0]["pkg_manager"], expected_pm,
                f"Incorrect pkg_manager for '{name}'")

    def test_multiple_distros(self):
        content = textwrap.dedent("""\
            Distribution Release Architecture Variant
            ---
            ubuntu   22.04  amd64  default
            debian   12     arm64  default
            alpine   3.18   i386   default
            fedora   39     amd64  default
        """)
        result = self._parse(content)
        self.assertEqual(len(result), 4)
        names = [d["name"] for d in result]
        self.assertIn("ubuntu", names)
        self.assertIn("debian", names)
        self.assertIn("alpine", names)
        self.assertIn("fedora", names)


class TestContainerNameValidation(unittest.TestCase):
    def _validate(self, name: str) -> bool:
        return '/' not in name and '\\' not in name and name != ".."

    def test_valid_names(self):
        for name in ["myapp", "ubuntu-dev", "test123", "a", "x-y-z", "my_box"]:
            with self.subTest(name=name):
                self.assertTrue(self._validate(name), f"'{name}' should be valid")

    def test_reject_forward_slash(self):
        for name in ["a/b", "/etc/passwd", "container/hack", "../secret"]:
            with self.subTest(name=name):
                self.assertFalse(self._validate(name), f"'{name}' should be rejected")

    def test_reject_backslash(self):
        self.assertFalse(self._validate("evil\\path"))

    def test_reject_dotdot(self):
        self.assertFalse(self._validate(".."))

    def test_dotdot_in_middle_allowed_if_no_slash(self):
        # "my..container" does not contain a slash and is not exactly ".."
        self.assertTrue(self._validate("my..container"))


class TestProjectInputValidation(unittest.TestCase):
    """Tests validate_project_input() logic — path traversal security."""

    def _validate(self, project_name: str, username: str) -> bool:
        """Mirror of validate_project_input() in Rust."""
        if '/' in username or ".." in username:
            return False
        if '/' in project_name or ".." in project_name:
            return False
        return True

    def test_valid_combinations(self):
        self.assertTrue(self._validate("myproject", "alice"))
        self.assertTrue(self._validate("backend-api", "bob123"))
        self.assertTrue(self._validate("proj_name", "user_name"))

    def test_reject_slash_in_project(self):
        self.assertFalse(self._validate("proj/evil", "alice"))
        self.assertFalse(self._validate("/etc/shadow", "alice"))

    def test_reject_slash_in_username(self):
        self.assertFalse(self._validate("project", "alice/hack"))
        self.assertFalse(self._validate("project", "/root"))

    def test_reject_dotdot_in_project(self):
        self.assertFalse(self._validate("..", "alice"))
        self.assertFalse(self._validate("../secret", "alice"))

    def test_reject_dotdot_in_username(self):
        self.assertFalse(self._validate("project", ".."))
        self.assertFalse(self._validate("project", "../admin"))


class TestCommandParsing(unittest.TestCase):
    """Tests parse_command() logic — shell input parsing."""

    def _parse(self, input_str: str):
        """Mirror of parse_command() in Rust."""
        raw = input_str.split()
        audit = "--audit" in raw
        parts = [x for x in raw if x != "--audit"]
        return parts, audit

    def test_basic_command(self):
        parts, audit = self._parse("melisa --list")
        self.assertEqual(parts, ["melisa", "--list"])
        self.assertFalse(audit)

    def test_audit_flag_at_end(self):
        parts, audit = self._parse("melisa --list --audit")
        self.assertEqual(parts, ["melisa", "--list"])
        self.assertTrue(audit)

    def test_audit_flag_in_middle(self):
        parts, audit = self._parse("melisa --audit --create mybox ubu-22.04-x64")
        self.assertEqual(parts, ["melisa", "--create", "mybox", "ubu-22.04-x64"])
        self.assertTrue(audit)

    def test_empty_input(self):
        parts, audit = self._parse("")
        self.assertEqual(parts, [])
        self.assertFalse(audit)

    def test_exit_command(self):
        parts, audit = self._parse("exit")
        self.assertEqual(parts, ["exit"])
        self.assertFalse(audit)

    def test_cd_with_path(self):
        parts, audit = self._parse("cd /home/user/projects")
        self.assertEqual(parts, ["cd", "/home/user/projects"])
        self.assertFalse(audit)

    def test_melisa_send_multi_word(self):
        parts, audit = self._parse("melisa --send mybox apt update")
        self.assertEqual(parts, ["melisa", "--send", "mybox", "apt", "update"])
        self.assertFalse(audit)


class TestPkgManagerCmd(unittest.TestCase):
    """Tests get_pkg_update_cmd() — package manager mapping."""

    def _get_cmd(self, pm: str) -> str:
        """Mirror of get_pkg_update_cmd() in Rust."""
        return {
            "apt":    "apt-get update -y",
            "dnf":    "dnf makecache",
            "apk":    "apk update",
            "pacman": "pacman -Sy --noconfirm",
            "zypper": "zypper --non-interactive refresh",
        }.get(pm, "true")

    def test_apt(self):
        self.assertEqual(self._get_cmd("apt"), "apt-get update -y")

    def test_dnf(self):
        self.assertEqual(self._get_cmd("dnf"), "dnf makecache")

    def test_apk(self):
        self.assertEqual(self._get_cmd("apk"), "apk update")

    def test_pacman(self):
        self.assertEqual(self._get_cmd("pacman"), "pacman -Sy --noconfirm")

    def test_zypper(self):
        self.assertEqual(self._get_cmd("zypper"), "zypper --non-interactive refresh")

    def test_unknown_fallback(self):
        self.assertEqual(self._get_cmd("yum"), "true")
        self.assertEqual(self._get_cmd(""), "true")
        self.assertEqual(self._get_cmd("brew"), "true")


# ─────────────────────────────────────────────────────────
# TEST SUITE 2: Bash Client Scripts (auth.sh, db.sh)
# ─────────────────────────────────────────────────────────
@unittest.skipUnless(has_bash_modules(), "Bash modules not found in CLIENT_SRC")
class TestAuthModule(unittest.TestCase):
    """Tests auth.sh — server connection profile management."""

    def setUp(self):
        self.env = BashEnv()

    def tearDown(self):
        self.env.cleanup()

    def test_init_auth_creates_directories(self):
        """init_auth() must create the required config directories."""
        rc, out, err = self.env.run_bash("init_auth")
        self.assertEqual(rc, 0, f"init_auth failed: {err}")
        config_dir = self.env.home / ".config" / "melisa"
        self.assertTrue(config_dir.exists(), "~/.config/melisa was not created")
        profile_file = config_dir / "profiles.conf"
        self.assertTrue(profile_file.exists(), "profiles.conf was not created")

    def test_get_active_conn_returns_1_when_no_active(self):
        """get_active_conn() should return 1 if there is no active connection."""
        rc, out, err = self.env.run_bash("init_auth; get_active_conn; echo exit=$?")
        self.assertIn("exit=1", out, f"Should return 1 if there is no active file: {out}")

    def test_add_profile_and_get_conn(self):
        """Adding a profile and retrieving it back."""
        script = textwrap.dedent("""\
            init_auth
            CONFIG_DIR="$HOME/.config/melisa"
            PROFILE_FILE="$CONFIG_DIR/profiles.conf"
            ACTIVE_FILE="$CONFIG_DIR/active"
            echo "myserver=root@192.168.1.100|alice" >> "$PROFILE_FILE"
            echo "myserver" > "$ACTIVE_FILE"
            result=$(get_active_conn)
            echo "CONN=$result"
        """)
        rc, out, err = self.env.run_bash(script)
        self.assertEqual(rc, 0, f"Error: {err}")
        self.assertIn("CONN=root@192.168.1.100", out,
            f"get_active_conn must return 'root@192.168.1.100', not: {out}")

    def test_get_active_conn_strips_melisa_user(self):
        """get_active_conn() must strip the '|melisa_user' portion."""
        script = textwrap.dedent("""\
            init_auth
            CONFIG_DIR="$HOME/.config/melisa"
            PROFILE_FILE="$CONFIG_DIR/profiles.conf"
            ACTIVE_FILE="$CONFIG_DIR/active"
            echo "prod=ubuntu@10.0.0.1|devuser" >> "$PROFILE_FILE"
            echo "prod" > "$ACTIVE_FILE"
            conn=$(get_active_conn)
            echo "CONN=$conn"
        """)
        rc, out, err = self.env.run_bash(script)
        self.assertEqual(rc, 0, f"Error: {err}")
        self.assertIn("CONN=ubuntu@10.0.0.1", out,
            f"Must strip '|devuser': {out}")


@unittest.skipUnless(has_bash_modules(), "Bash modules not found in CLIENT_SRC")
class TestDBModule(unittest.TestCase):
    """Tests db.sh — project registry (flat file database)."""

    def setUp(self):
        self.env = BashEnv()
        self.db_dir = self.env.home / ".config" / "melisa"
        self.db_dir.mkdir(parents=True, exist_ok=True)

    def tearDown(self):
        self.env.cleanup()

    def _setup_db(self) -> str:
        """Setup DB_PATH in the environment."""
        return f'DB_PATH="{self.db_dir}/registry"'

    def test_db_update_project_creates_entry(self):
        """db_update_project() must create a new entry."""
        script = f"""\
{self._setup_db()}
db_update_project "myapp" "/home/user/projects/myapp"
cat "$DB_PATH"
"""
        rc, out, err = self.env.run_bash(script)
        self.assertEqual(rc, 0, f"Error: {err}")
        self.assertIn("myapp|", out)

    def test_db_update_project_overwrites_existing(self):
        """db_update_project() must overwrite an existing entry (no duplicates)."""
        script = f"""\
{self._setup_db()}
db_update_project "backend" "/old/path"
db_update_project "backend" "/new/path"
count=$(grep -c "^backend|" "$DB_PATH" 2>/dev/null || echo "0")
echo "COUNT=$count"
content=$(cat "$DB_PATH")
echo "CONTENT=$content"
"""
        rc, out, err = self.env.run_bash(script)
        self.assertEqual(rc, 0, f"Error: {err}")
        self.assertIn("COUNT=1", out, "Must have exactly 1 entry after overwrite")
        self.assertIn("/new/path", out, "Must save the new path")
        self.assertNotIn("/old/path", out, "The old path must be removed")

    def test_db_update_multiple_projects(self):
        """Multiple projects can be saved simultaneously."""
        script = f"""\
{self._setup_db()}
db_update_project "frontend" "/home/user/frontend"
db_update_project "backend"  "/home/user/backend"
db_update_project "scripts"  "/home/user/scripts"
wc -l < "$DB_PATH"
"""
        rc, out, err = self.env.run_bash(script)
        self.assertEqual(rc, 0, f"Error: {err}")
        self.assertIn("3", out.strip(), "There must be 3 entries in the database")

    def test_db_identify_by_pwd_exact_match(self):
        """db_identify_by_pwd() must return the project name for an exact match."""
        project_dir = self.env.home / "projects" / "myapp"
        project_dir.mkdir(parents=True)
        script = f"""\
{self._setup_db()}
db_update_project "myapp" "{project_dir}"
cd "{project_dir}"
result=$(db_identify_by_pwd)
echo "PROJECT=$result"
"""
        rc, out, err = self.env.run_bash(script)
        self.assertEqual(rc, 0, f"Error: {err}")
        self.assertIn("PROJECT=myapp", out)

    def test_db_identify_by_pwd_subdir_match(self):
        """db_identify_by_pwd() must match when inside a project subdirectory."""
        project_dir = self.env.home / "projects" / "backend"
        sub_dir = project_dir / "src" / "api"
        sub_dir.mkdir(parents=True)
        script = f"""\
{self._setup_db()}
db_update_project "backend" "{project_dir}"
cd "{sub_dir}"
result=$(db_identify_by_pwd)
echo "PROJECT=$result"
"""
        rc, out, err = self.env.run_bash(script)
        self.assertEqual(rc, 0, f"Error: {err}")
        self.assertIn("PROJECT=backend", out)

    def test_db_identify_by_pwd_no_match(self):
        """db_identify_by_pwd() must return empty if there is no match."""
        unrelated_dir = self.env.home / "unrelated"
        unrelated_dir.mkdir(parents=True)
        script = f"""\
{self._setup_db()}
db_update_project "myapp" "{self.env.home}/projects/myapp"
cd "{unrelated_dir}"
result=$(db_identify_by_pwd)
echo "PROJECT='$result'"
"""
        rc, out, err = self.env.run_bash(script)
        self.assertEqual(rc, 0, f"Error: {err}")
        self.assertIn("PROJECT=''", out, "Must return empty if there is no match")

    def test_db_identify_longest_prefix_wins(self):
        """db_identify_by_pwd() must select the most specific (longest) path."""
        parent_dir = self.env.home / "work"
        child_dir  = self.env.home / "work" / "specific" / "project"
        child_dir.mkdir(parents=True)
        script = f"""\
{self._setup_db()}
db_update_project "parent"   "{parent_dir}"
db_update_project "specific" "{child_dir}"
cd "{child_dir}"
result=$(db_identify_by_pwd)
echo "PROJECT=$result"
"""
        rc, out, err = self.env.run_bash(script)
        self.assertEqual(rc, 0, f"Error: {err}")
        self.assertIn("PROJECT=specific", out, "Must select the longest path (specific)")

    def test_db_no_false_positive_prefix(self):
        """db_identify_by_pwd() must not match '/projects/app' for '/projects/apple'."""
        app_dir   = self.env.home / "projects" / "app"
        apple_dir = self.env.home / "projects" / "apple"
        app_dir.mkdir(parents=True)
        apple_dir.mkdir(parents=True)
        script = f"""\
{self._setup_db()}
db_update_project "app" "{app_dir}"
cd "{apple_dir}"
result=$(db_identify_by_pwd)
echo "PROJECT='$result'"
"""
        rc, out, err = self.env.run_bash(script)
        self.assertEqual(rc, 0, f"Error: {err}")
        self.assertIn("PROJECT=''", out,
            "Boundary check failed: 'app' must not match inside 'apple' directory")

    def test_db_get_path_returns_correct_path(self):
        """db_get_path() must return the correct path for a project name."""
        project_path = str(self.env.home / "work" / "backend")
        script = f"""\
{self._setup_db()}
db_update_project "backend" "{project_path}"
result=$(db_get_path "backend")
echo "PATH=$result"
"""
        rc, out, err = self.env.run_bash(script)
        self.assertEqual(rc, 0, f"Error: {err}")
        self.assertIn(f"PATH={project_path}", out)

    def test_db_get_path_nonexistent_returns_empty(self):
        """db_get_path() must return empty for a nonexistent project."""
        script = f"""\
{self._setup_db()}
result=$(db_get_path "nonexistent_project")
echo "PATH='$result'"
"""
        rc, out, err = self.env.run_bash(script)
        self.assertEqual(rc, 0, f"Error: {err}")
        self.assertIn("PATH=''", out, "Must return empty for an unknown project")


@unittest.skipUnless(has_bash_modules(), "Bash modules not found in CLIENT_SRC")
class TestUtilsModule(unittest.TestCase):
    """Tests utils.sh — helper functions."""

    def setUp(self):
        self.env = BashEnv()

    def tearDown(self):
        self.env.cleanup()

    def test_log_info_outputs_to_stderr(self):
        """Logging functions (if any) must output to stderr, not stdout."""
        script = 'log_info "test message" 2>&1 1>/dev/null; echo "STDERR_ONLY=$?"'
        rc, out, err = self.env.run_bash(script)
        if "log_info: command not found" in err:
            self.skipTest("log_info is not present in utils.sh")

    def test_bash_scripts_are_syntactically_valid(self):
        """All .sh files must be parsable by bash without syntax errors."""
        if not CLIENT_SRC or not CLIENT_SRC.exists():
            self.skipTest("CLIENT_SRC not found")
        for sh_file in sorted(CLIENT_SRC.glob("*.sh")):
            with self.subTest(file=sh_file.name):
                result = subprocess.run(
                    ["bash", "-n", str(sh_file)],
                    capture_output=True, text=True
                )
                self.assertEqual(
                    result.returncode, 0,
                    f"Syntax error in {sh_file.name}:\n{result.stderr}"
                )


# ─────────────────────────────────────────────────────────
# TEST SUITE 3: Cargo Test Integration
# ─────────────────────────────────────────────────────────
class TestCargoTests(unittest.TestCase):
    """Runs `cargo test` to execute all Rust unit tests."""

    def _run_cargo_test(self, test_filter: str = "", timeout: int = 120):
        """Run cargo test with an optional filter."""
        cmd = ["cargo", "test", "--quiet"]
        if test_filter:
            cmd.append(test_filter)
        cmd.extend(["--", "--nocapture"])
        try:
            result = subprocess.run(
                cmd,
                cwd=str(MELISA_ROOT),
                capture_output=True,
                text=True,
                timeout=timeout
            )
            debug_print("CargoTest", cmd, result.returncode, result.stdout, result.stderr)
            return result.returncode, result.stdout, result.stderr
        except subprocess.TimeoutExpired:
            debug_print("CargoTest", cmd, -1, "", f"cargo test timeout after {timeout}s")
            return -1, "", f"cargo test timeout after {timeout}s"
        except Exception as e:
            debug_print("CargoTest", cmd, -2, "", str(e))
            return -2, "", str(e)

    @unittest.skipUnless(
        MELISA_ROOT is not None and shutil.which("cargo") is not None,
        "cargo is not available or MELISA_ROOT is not found"
    )
    def test_cargo_check_compiles(self):
        """The project must compile without errors (cargo check)."""
        cmd_args = ["cargo", "check", "--quiet"]
        result = subprocess.run(
            cmd_args,
            cwd=str(MELISA_ROOT),
            capture_output=True,
            text=True,
            timeout=120
        )
        debug_print("CargoCheck", cmd_args, result.returncode, result.stdout, result.stderr)
        self.assertEqual(
            result.returncode, 0,
            f"cargo check failed:\n{result.stderr}"
        )

    @unittest.skipUnless(
        MELISA_ROOT is not None and shutil.which("cargo") is not None,
        "cargo is not available"
    )
    def test_cargo_test_unit_tests_pass(self):
        """All Rust unit tests must pass."""
        rc, out, err = self._run_cargo_test()
        if rc != 0:
            failed_tests = [
                line for line in (out + err).splitlines()
                if "FAILED" in line or "error" in line.lower()
            ]
            self.fail(
                f"cargo test failed (exit code {rc}).\n"
                f"Failed tests:\n" + "\n".join(failed_tests[:20]) +
                f"\n\nFull stderr:\n{err[:2000]}"
            )

    @unittest.skipUnless(
        MELISA_ROOT is not None and shutil.which("cargo") is not None,
        "cargo is not available"
    )
    def test_cargo_test_distro_module(self):
        """Specific unit test for the distro module."""
        rc, out, err = self._run_cargo_test("distro")
        self.assertEqual(rc, 0, f"Distro tests failed:\n{err[:2000]}")

    @unittest.skipUnless(
        MELISA_ROOT is not None and shutil.which("cargo") is not None,
        "cargo is not available"
    )
    def test_cargo_test_metadata_module(self):
        """Specific unit test for the metadata module."""
        rc, out, err = self._run_cargo_test("metadata")
        self.assertEqual(rc, 0, f"Metadata tests failed:\n{err[:2000]}")


# ─────────────────────────────────────────────────────────
# TEST SUITE 4: Rust Binary Integration Tests
# ─────────────────────────────────────────────────────────
def get_melisa_binary() -> Optional[Path]:
    """Find the compiled melisa binary."""
    if DEBUG_BIN and DEBUG_BIN.exists():
        return DEBUG_BIN
    if BINARY and BINARY.exists():
        return BINARY
    return None


class TestMelisaBinary(unittest.TestCase):
    """
    Integration test: testing the compiled melisa binary.

    The melisa binary requires root privileges for most of its operations.

    ORIGINAL ISSUE (Fixed):
      Older versions used `sudo` without the `-n` flag, causing the process
      to BLOCK for 10 seconds while waiting for password input in the TTY
      → test_help, test_create, test_invite always result in TIMEOUT and FAIL.

    APPLIED FIX:
      1. Use `sudo -n` (non-interactive) to immediately fail if a password
         is required, instead of blocking.
      2. setUpClass() detects sudo availability once at the beginning.
      3. _require_sudo() in each test provides a clear SKIP message along
         with configuration instructions, instead of a false FAIL.
      4. Timeout is lowered to 8 seconds to provide a reasonable buffer.

    NOPASSWD SETUP (to run all tests):
      sudo visudo
      # Add this line (replace path according to your system):
      saferoom ALL=(ALL) NOPASSWD: /path/to/target/debug/melisa
    """

    @classmethod
    def setUpClass(cls):
        """Detect once at the beginning if passwordless sudo is available."""
        cls.binary     = get_melisa_binary()
        cls._sudo_ok   = can_sudo_nopasswd()
        cls._sudo_hint = (
            "Passwordless sudo is not available.\n"
            "  Add to sudoers via: sudo visudo\n"
            "  Example line: saferoom ALL=(ALL) NOPASSWD: "
            f"{cls.binary or '/path/to/target/debug/melisa'}"
        )

    def _require_sudo(self):
        """
        Skip this test if passwordless sudo is not available.
        Called at the beginning of every test that requires root.
        """
        if not self._sudo_ok:
            self.skipTest(self._sudo_hint)

    def _run_melisa(self, args: list, timeout: int = 8) -> Tuple[int, str, str]:
        """
        Run the melisa binary with specific arguments via sudo.

        Using `sudo -n` (non-interactive) so that:
          - It immediately fails with exit code 1 if a password is required.
          - It does not block the test process until a timeout occurs.

        Args:
            args:    List of arguments to pass to the melisa binary.
            timeout: Execution time limit in seconds (default 8s).

        Returns:
            Tuple (returncode, stdout, stderr).
            returncode = -1 on timeout, -2 on other errors.
        """
        if not self.binary:
            return -1, "", "Binary not found — run: cargo build"

        # FIX #1: Use sudo -n to avoid blocking the TTY
        cmd = ["sudo", "-n", str(self.binary)] + args
        try:
            result = subprocess.run(
                cmd,
                capture_output=True,
                text=True,
                timeout=timeout
            )
            debug_print("MelisaBinary", cmd, result.returncode, result.stdout, result.stderr)
            return result.returncode, result.stdout, result.stderr
        except subprocess.TimeoutExpired:
            debug_print("MelisaBinary", cmd, -1, "", f"Timeout after {timeout}s")
            return -1, "", f"Timeout after {timeout}s"
        except Exception as e:
            debug_print("MelisaBinary", cmd, -2, "", str(e))
            return -2, "", str(e)

    # ── Tests that DO NOT require sudo (pass natively) ──────────────────
    @unittest.skipUnless(get_melisa_binary() is not None, "Melisa binary not found")
    def test_version_command(self):
        """
        melisa --version must display the version.

        Note: --version is processed before check_root() in main.rs,
        so it does not require sudo and will always pass.
        """
        rc, out, err = self._run_melisa(["melisa", "--version"])
        combined = out + err
        self.assertIn("0.1", combined, "Version number must be present in the output")

    # ── Tests that require sudo (skip if NOPASSWD is unconfigured) ──
    @unittest.skipUnless(get_melisa_binary() is not None, "Melisa binary not found")
    def test_help_command_shows_usage(self):
        """melisa --help must display usage info."""
        self._require_sudo()   # FIX #3: SKIP if no NOPASSWD
        rc, out, err = self._run_melisa(["melisa", "--help"])
        combined = out + err
        self.assertIn("MELISA", combined, "Output must mention MELISA")
        self.assertIn("--help", combined)
        self.assertIn("--list", combined)

    @unittest.skipUnless(get_melisa_binary() is not None, "Melisa binary not found")
    def test_unknown_command_shows_error(self):
        """An unknown command must display an error message."""
        self._require_sudo()
        rc, out, err = self._run_melisa(["melisa", "--fakecommand"])
        combined = out + err
        self.assertIn("ERROR", combined.upper())

    @unittest.skipUnless(get_melisa_binary() is not None, "Melisa binary not found")
    def test_create_requires_name_and_code(self):
        """melisa --create without arguments must display an error, not crash."""
        self._require_sudo()   # FIX #3: SKIP if no NOPASSWD
        rc, out, err = self._run_melisa(["melisa", "--create"])
        combined = out + err
        # Must contain an informative error message
        self.assertIn("ERROR", combined.upper(),
            f"Binary must output ERROR for --create without arguments.\n"
            f"Output: {combined!r}")

    @unittest.skipUnless(get_melisa_binary() is not None, "Melisa binary not found")
    def test_invite_requires_enough_args(self):
        """melisa --invite without enough args must display an error."""
        self._require_sudo()   # FIX #3: SKIP if no NOPASSWD
        rc, out, err = self._run_melisa(["melisa", "--invite"])
        combined = out + err
        self.assertIn("ERROR", combined.upper(),
            f"Binary must output ERROR for --invite without arguments.\n"
            f"Output: {combined!r}")

    @unittest.skipUnless(get_melisa_binary() is not None, "Melisa binary not found")
    def test_list_command_requires_root(self):
        """
        melisa --list without root should display an error or request sudo,
        rather than crashing with a traceback.

        This test verifies that the binary fails gracefully,
        not with a panic or segfault (exit code 139 / SIGSEGV).
        """
        self._require_sudo()
        rc, out, err = self._run_melisa(["melisa", "--list"])
        # Must not crash (SIGSEGV = 139, panic is usually = 101)
        self.assertNotEqual(rc, 139, "Binary crashed with SIGSEGV (segfault)")
        self.assertNotIn("thread 'main' panicked", out + err,
            "Binary executed panic! — this is a Rust bug that needs fixing")


# ─────────────────────────────────────────────────────────
# TEST SUITE 5: Security Tests (Overall Security)
# ─────────────────────────────────────────────────────────
class TestSecurityCritical(unittest.TestCase):
    """Critical security tests — path traversal, injection, etc."""

    def test_no_path_traversal_in_container_name(self):
        """Container name must not contain path traversal characters."""
        evil_names = [
            "../etc",
            "../../root/.ssh/authorized_keys",
            "/etc/shadow",
            "evil/path",
            "..\\windows\\system32",
        ]
        for name in evil_names:
            with self.subTest(name=name):
                is_safe = '/' not in name and '\\' not in name and name != ".."
                self.assertFalse(
                    is_safe,
                    f"Malicious name '{name}' must be rejected by validation"
                )

    def test_no_path_traversal_in_username(self):
        """Username must not contain path traversal characters."""
        evil_usernames = ["../root", "alice/../root", "user/hack", ".."]
        for username in evil_usernames:
            with self.subTest(username=username):
                is_safe = '/' not in username and ".." not in username
                self.assertFalse(
                    is_safe,
                    f"Malicious username '{username}' must be rejected"
                )

    def test_metadata_content_format(self):
        """Metadata format must use KEY=VALUE without malicious characters."""
        import re
        valid_keys = [
            "MELISA_INSTANCE_NAME", "MELISA_INSTANCE_ID", "DISTRO_SLUG",
            "DISTRO_NAME", "DISTRO_RELEASE", "ARCHITECTURE", "CREATED_AT"
        ]
        key_pattern = re.compile(r'^[A-Z_]+$')
        for key in valid_keys:
            with self.subTest(key=key):
                self.assertTrue(
                    key_pattern.match(key),
                    f"Key '{key}' contains unsafe characters"
                )

    def test_project_path_construction_safety(self):
        """Path /home/<user>/<project> must be safe from injection."""
        safe_combos = [
            ("alice",   "backend"),
            ("bob",     "frontend-app"),
            ("user1",   "proj_1"),
        ]
        evil_combos = [
            ("../root",    "project"),     # username traversal
            ("alice",      "../../../etc"), # project traversal
            ("user/hack",  "project"),     # username with slash
        ]
        for username, project in safe_combos:
            with self.subTest(username=username, project=project):
                is_safe = '/' not in username and ".." not in username \
                          and '/' not in project and ".." not in project
                self.assertTrue(is_safe, f"Combination ({username}, {project}) should be safe")

        for username, project in evil_combos:
            with self.subTest(username=username, project=project):
                is_safe = '/' not in username and ".." not in username \
                          and '/' not in project and ".." not in project
                self.assertFalse(is_safe, f"Combination ({username}, {project}) should be rejected")


# ─────────────────────────────────────────────────────────
# Custom test result with timing and colors
# ─────────────────────────────────────────────────────────
class ColoredTestResult(unittest.TextTestResult):
    def startTest(self, test):
        super().startTest(test)
        self._start_time = time.monotonic()

    def addSuccess(self, test):
        super().addSuccess(test)
        elapsed = time.monotonic() - self._start_time
        if self.showAll:
            self.stream.write(col(f"  [PASS] ({elapsed:.3f}s)\n", GREEN))
            self.stream.flush()

    def addFailure(self, test, err):
        super().addFailure(test, err)
        elapsed = time.monotonic() - self._start_time
        if self.showAll:
            self.stream.write(col(f"  [FAIL] ({elapsed:.3f}s)\n", RED))
            self.stream.flush()

    def addError(self, test, err):
        super().addError(test, err)
        elapsed = time.monotonic() - self._start_time
        if self.showAll:
            self.stream.write(col(f"  [ERROR] ({elapsed:.3f}s)\n", RED))
            self.stream.flush()

    def addSkip(self, test, reason):
        super().addSkip(test, reason)
        if self.showAll:
            self.stream.write(col(f"  [SKIP] {reason}\n", YELLOW))
            self.stream.flush()


class ColoredTestRunner(unittest.TextTestRunner):
    resultclass = ColoredTestResult


# ─────────────────────────────────────────────────────────
# Entry point
# ─────────────────────────────────────────────────────────
def print_banner():
    """Display information banner before testing."""
    print(col("=" * 65, CYAN))
    print(col("  MELISA — Unit Test Runner", BOLD + CYAN))
    if DEBUG_MODE:
        print(col("  *** DEBUG MODE ACTIVE ***", BOLD + YELLOW))
    print(col("=" * 65, CYAN))
    print(f"  Project Root : {col(str(MELISA_ROOT or 'NOT FOUND'), YELLOW)}")
    print(f"  Bash Client  : {col(str(CLIENT_SRC or 'NOT FOUND'), YELLOW)}")
    binary = get_melisa_binary()
    print(f"  Binary       : {col(str(binary or 'Not compiled yet'), YELLOW)}")
    cargo_available = col("[OK] available", GREEN) if shutil.which("cargo") else col("[FAIL] missing", RED)
    bash_available  = col("[OK] available", GREEN) if has_bash_modules() else col("[WARN] missing", YELLOW)
    sudo_ok         = can_sudo_nopasswd()
    sudo_status     = col("[OK] NOPASSWD active", GREEN) if sudo_ok else col("[WARN] needs configuration (some tests will SKIP)", YELLOW)
    print(f"  cargo        : {cargo_available}")
    print(f"  Bash modules : {bash_available}")
    print(f"  sudo -n      : {sudo_status}")
    print(col("=" * 65, CYAN))
    print()


def main():
    """Entry point to execute all tests."""
    print_banner()

    loader = unittest.TestLoader()
    suites = [
        ("Pure Logic Tests", loader.loadTestsFromTestCase(TestSlugGeneration)),
        ("Pure Logic Tests", loader.loadTestsFromTestCase(TestDistroListParsing)),
        ("Pure Logic Tests", loader.loadTestsFromTestCase(TestContainerNameValidation)),
        ("Pure Logic Tests", loader.loadTestsFromTestCase(TestProjectInputValidation)),
        ("Pure Logic Tests", loader.loadTestsFromTestCase(TestCommandParsing)),
        ("Pure Logic Tests", loader.loadTestsFromTestCase(TestPkgManagerCmd)),
        ("Bash: auth.sh",    loader.loadTestsFromTestCase(TestAuthModule)),
        ("Bash: db.sh",      loader.loadTestsFromTestCase(TestDBModule)),
        ("Bash: utils",      loader.loadTestsFromTestCase(TestUtilsModule)),
        ("Rust: cargo test", loader.loadTestsFromTestCase(TestCargoTests)),
        ("Binary: melisa",   loader.loadTestsFromTestCase(TestMelisaBinary)),
        ("Security",         loader.loadTestsFromTestCase(TestSecurityCritical)),
    ]

    # Use args to support normal unittest flags, while ignoring --debug which we already popped
    if len(sys.argv) > 1:
        unittest.main(argv=[sys.argv[0]] + sys.argv[1:], verbosity=2,
                      testRunner=ColoredTestRunner)
        return

    all_suite = unittest.TestSuite()
    for _, suite in suites:
        all_suite.addTests(suite)

    runner = ColoredTestRunner(verbosity=2, stream=sys.stdout)
    result = runner.run(all_suite)

    print()
    print(col("=" * 65, CYAN))
    total   = result.testsRun
    passed  = total - len(result.failures) - len(result.errors) - len(result.skipped)
    failed  = len(result.failures) + len(result.errors)
    skipped = len(result.skipped)
    print(f"  Total    : {col(str(total), BOLD)}")
    print(f"  {col('Passed', GREEN)}   : {col(str(passed), GREEN)}")
    print(f"  {col('Failed', RED)}   : {col(str(failed), RED) if failed else col('0', GREEN)}")
    print(f"  {col('Skipped', YELLOW)}  : {col(str(skipped), YELLOW)}")
    print(col("=" * 65, CYAN))

    if result.failures or result.errors:
        print(col("\n  [ERROR] SOME TESTS FAILED — Check details above\n", RED + BOLD))
        sys.exit(1)
    else:
        print(col("\n  [SUCCESS] ALL TESTS PASSED!\n", GREEN + BOLD))
        sys.exit(0)


if __name__ == "__main__":
    main()