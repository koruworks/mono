# SPDX-License-Identifier: MPL-2.0
"""Opt-in driver for the real-provider proof and its empty synthetic MCP."""
import argparse
import importlib.util
import json
from pathlib import Path
import subprocess
import sys


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--run", action="store_true", required=True)
    parser.add_argument("--root", required=True)
    parser.add_argument("--program", required=True)
    args = parser.parse_args()
    checkout = Path(__file__).resolve().parents[3]
    source = checkout / "crates/koru-session-runtime/tests/bots_protocol_probe.py"
    spec = importlib.util.spec_from_file_location("probe", source)
    probe = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(probe)
    endpoint = probe.LocalEndpoint()
    try:
        command = ["cargo", "test", "-p", "koru-bots-engine", "--features", "provider-runtime", "--test", "session_process",
                   "--locked", "--offline", "-j", "1", "--", "--run", "--root", args.root,
                   "--program", args.program, "--endpoint", endpoint.descriptor("initial")["url"]]
        process = subprocess.Popen([sys.executable, str(source), "--child", json.dumps(command)],
                                   cwd=checkout, stdin=subprocess.PIPE,
                                   creationflags=subprocess.CREATE_NO_WINDOW)
        job = probe.OwnedJob(process)
        try:
            process.stdin.write(b"start\n")
            process.stdin.flush()
            code = process.wait(timeout=240)
        finally:
            job.close()
            process.wait(timeout=10)
            process.stdin.close()
        if code:
            return code
        assert endpoint.counts["initial"] > 0 and endpoint.counts["replacement"] > 0
        assert endpoint.counts["rejected"] == 0
        print("PASS synthetic MCP credentials: " + str(endpoint.counts), flush=True)
        return 0
    finally:
        endpoint.close()


if __name__ == "__main__":
    sys.exit(main())
