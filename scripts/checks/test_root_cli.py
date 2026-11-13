# SPDX-License-Identifier: MPL-2.0

"""Exercise cmd -> PowerShell -> npm-style shims without real builds or clients."""

import json
import os
import shutil
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]


@unittest.skipUnless(os.name == "nt", "Windows launcher")
class RootArguments(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix="koru root ")
        self.addCleanup(self.temp.cleanup)
        self.repo = Path(self.temp.name)
        self.dev = self.repo / "scripts/dev"
        self.dev.mkdir(parents=True)
        shutil.copyfile(ROOT / "koru.bat", self.repo / "koru.bat")
        shutil.copyfile(ROOT / "scripts/dev/mcp-registration.ps1", self.dev / "mcp-registration.ps1")
        script = (ROOT / "scripts/dev/root-stack.ps1").read_text()
        # All production parsing, environment, build and registration run. Stop
        # at the native UI boundary; fixtures never contain a Koru executable.
        script = script.replace(
            "    # Start-Process joins ArgumentList into a Windows command line.",
            '    [Console]::WriteLine("TEST_LAUNCH_ROOT=" + $Root)\n'
            '    exit 0\n'
            "    # Start-Process joins ArgumentList into a Windows command line.",
        )
        script = script.replace(
            '    $agent = Join-Path $bin "koru-agent.exe"',
            '    if ($Action -ne "Start") {\n'
            '        [Console]::WriteLine("TEST_ACTION=" + $Action + ";ROOT=" + $Root)\n'
            '        exit 0\n    }\n'
            '    $agent = Join-Path $bin "koru-agent.exe"',
        )
        (self.dev / "root-stack.ps1").write_text(script)
        (self.dev / "stop-exact-exe.ps1").write_text(
            'param([string[]]$ExecutablePath)\n'
            '[Console]::WriteLine("TEST_STOP=" + ($ExecutablePath -join ";"))\n'
        )
        setup = self.repo / "scripts/setup"
        setup.mkdir()
        for name in ["fetch-fonts", "fetch-third-party"]:
            (setup / (name + ".ps1")).write_text('Write-Host "TEST_SETUP"\n')
        self.stub = self.repo / "stubs"
        self.stub.mkdir()
        self.log = self.repo / "calls.jsonl"
        collector = self.stub / "collect.py"
        collector.write_text(
            'import json, os, sys\n'
            'with open(os.environ["TEST_CALLS"], "a") as f:\n'
            '    f.write(json.dumps(sys.argv[1:]) + "\\n")\n'
            'if sys.argv[1] == "git":\n'
            '    print(os.environ.get("TEST_BRANCH", "sliechti/a2ui") '
            'if "branch" in sys.argv else "abc1234")\n'
            'sys.exit(int(os.environ.get("TEST_FAIL_CODE", "7")) '
            'if sys.argv[1] == os.environ.get("TEST_FAIL") else 0)\n'
        )
        for command in ["sccache", "cargo", "git", "claude", "codex", "gemini"]:
            (self.stub / (command + ".cmd")).write_text(
                f'@"{sys.executable}" "{collector}" {command} %*\n'
            )
        self.env = os.environ.copy()
        for key in list(self.env):
            if key.startswith(("KORU_", "ROOT_COMMAND_")) or key == "CARGO_TARGET_DIR":
                del self.env[key]
        system = Path(os.environ["SystemRoot"]) / "System32"
        self.env.update({
            "PATH": os.pathsep.join(map(str, [self.stub, system, system / "WindowsPowerShell/v1.0"])),
            "TEST_CALLS": str(self.log),
        })

    def run_launcher(self, *args):
        # cmd.exe does not interpret backslash as an escape before a quote.
        # Quote every argument ourselves; list2cmdline uses native CRT rules.
        env = self.env.copy()
        env["TEST_LAUNCHER"] = str(self.repo / "koru.bat")
        command = '"%TEST_LAUNCHER%"'
        for index, arg in enumerate(args):
            env[f"TEST_INPUT_{index}"] = arg
            command += f' "%TEST_INPUT_{index}%"'
        return subprocess.run(
            f'"{os.environ["ComSpec"]}" /d /v:off /s /c "{command}"',
            cwd=self.repo, env=env, capture_output=True, text=True, timeout=30,
        )

    def calls(self, name):
        if not self.log.exists():
            return []
        return [call[1:] for line in self.log.read_text().splitlines()
                if (call := json.loads(line))[0] == name]

    def assert_ok(self, result):
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)

    def test_flag_default_root_and_full_branch_registration(self):
        result = self.run_launcher("root", "--mcp")
        self.assert_ok(result)
        expected = str(self.repo / ".dev-data/root-mode")
        self.assertIn("TEST_LAUNCH_ROOT=" + expected, result.stdout)
        self.assertFalse((self.repo / "--mcp").exists())
        self.assertEqual(self.calls("cargo"), [[
            "build", "--target-dir", str(self.repo / "target"),
            "-p", "koru-agent", "-p", "koru", "-p", "koru-mcp",
        ]])
        for cli in ["claude", "codex", "gemini"]:
            call = self.calls(cli)[0]
            self.assertIn("koru_sliechti_a2ui", call)
            self.assertEqual(call[-2:], ["--root", expected])
            self.assertIn(str(self.repo / "target/debug/koru-mcp.exe"), call)

    def test_spaces_metacharacters_and_trailing_slash_in_either_order(self):
        path = str(self.repo / "data !&%TEST_BRANCH%^ space") + "\\"
        self.env["CARGO_TARGET_DIR"] = "custom target"
        for args in [(path, "--mcp"), ("--mcp", path)]:
            with self.subTest(args=args):
                self.log.unlink(missing_ok=True)
                result = self.run_launcher("root", *args)
                self.assert_ok(result)
                self.assertIn("TEST_LAUNCH_ROOT=" + path, result.stdout)
                for cli in ["claude", "codex", "gemini"]:
                    self.assertEqual(self.calls(cli)[0][-2:], ["--root", path])
                    self.assertIn(str(self.repo / "custom target/debug/koru-mcp.exe"), self.calls(cli)[0])

    def test_plain_root_builds_bridge_without_registration(self):
        self.assert_ok(self.run_launcher("root", ".\\--mcp"))
        self.assertIn("koru-mcp", self.calls("cargo")[0])
        self.assertEqual(self.calls("claude"), [])

    def test_unknown_duplicate_extra_and_empty_arguments_have_no_effects(self):
        for args in [
            ("root", "--invalid"), ("root", "--mcp", "--invalid"),
            ("root", "", "--invalid"), ("root", "one", "two"),
            ("root", "--mcp", "--mcp"), ("root-stop", "--mcp"),
            ("root-open", "one", "two"), ("mcp-add", "unsafe&name"),
            ("mcp-add", "one", "two"),
        ]:
            with self.subTest(args=args):
                result = self.run_launcher(*args)
                self.assertEqual(result.returncode, 2, result.stdout + result.stderr)
                self.assertNotIn("TEST_SETUP", result.stdout)
                self.assertNotIn("TEST_STOP", result.stdout)
                self.assertFalse(self.log.exists())

    def test_open_and_stop_keep_the_supplied_path(self):
        path = str(self.repo / "another root") + "\\"
        for verb, action in [("root-open", "Open"), ("root-stop", "Stop")]:
            result = self.run_launcher(verb, path)
            self.assert_ok(result)
            self.assertIn(f"TEST_ACTION={action};ROOT={path}", result.stdout)

    def test_client_failure_is_named_and_prevents_launch(self):
        self.env["TEST_FAIL"] = "claude"
        result = self.run_launcher("root", "--mcp")
        self.assertNotEqual(result.returncode, 0)
        error = " ".join(result.stderr.split())
        self.assertIn("claude (exit 7)", error)
        self.assertIn("mcp-remove koru_sliechti_a2ui", error)
        self.assertNotIn("TEST_LAUNCH_ROOT", result.stdout)
        self.assertEqual(len(self.calls("gemini")), 1)

    def test_build_failure_does_not_register(self):
        self.env["TEST_FAIL"] = "cargo"
        result = self.run_launcher("root", "--mcp")
        self.assertNotEqual(result.returncode, 0)
        self.assertEqual(self.calls("claude"), [])
        self.assertNotIn("TEST_LAUNCH_ROOT", result.stdout)

    def test_mcp_add_keeps_default_name_and_explicit_root(self):
        self.env["KORU_ROOT"] = str(self.repo / "chosen !&%TEST_BRANCH%^ root") + "\\"
        result = self.run_launcher("mcp-add")
        self.assert_ok(result)
        self.assertEqual(self.calls("codex")[0][2], "koru")
        self.assertEqual(self.calls("codex")[0][-2:], ["--root", self.env["KORU_ROOT"]])
        self.assertEqual(self.calls("cargo")[0][-2:], ["-p", "koru-mcp"])

    def test_root_inherits_exact_environment_root(self):
        path = str(self.repo / "inherited !&%TEST_BRANCH%^ root") + "\\"
        self.env["KORU_ROOT"] = path
        result = self.run_launcher("root", "--mcp")
        self.assert_ok(result)
        self.assertIn("TEST_LAUNCH_ROOT=" + path, result.stdout)
        for cli in ["claude", "codex", "gemini"]:
            self.assertEqual(self.calls(cli)[0][-2:], ["--root", path])

    def test_missing_clients_fail_without_launching(self):
        for cli in ["claude", "codex", "gemini"]:
            (self.stub / (cli + ".cmd")).unlink()
        result = self.run_launcher("root", "--mcp")
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("nothing was registered", " ".join(result.stderr.split()))
        self.assertNotIn("TEST_LAUNCH_ROOT", result.stdout)

    def test_detached_head_has_checkout_and_commit_name(self):
        self.env["TEST_BRANCH"] = ""
        self.assert_ok(self.run_launcher("root", "--mcp"))
        name = self.calls("codex")[0][2]
        self.assertTrue(name.startswith("koru_koru_root_"), name)
        self.assertTrue(name.endswith("_abc1234"), name)


if __name__ == "__main__":
    unittest.main()
