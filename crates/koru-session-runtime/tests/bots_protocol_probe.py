# SPDX-License-Identifier: MPL-2.0
"""Opt-in Bots protocol spike. Never loaded by cargo test.

Uses synthetic sessions in an explicitly supplied disposable directory. Output
is a whitelist of protocol observations: no native IDs, paths, tokens, provider
messages, prompts or content. A model turn additionally requires --prompt.
On Windows a bootstrap child waits until assigned to a kill-on-close job before
starting the provider; closing that job cleans up only this probe's descendants.
"""

import argparse
import ctypes
from ctypes import wintypes
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
import json
import os
from pathlib import Path
import queue
import re
import subprocess
import sys
import threading
import time

MARKER = "bots-probe-delivery-00000000-0000-4000-8000-000000000001"


class LocalEndpoint:
    """Empty MCP service: observes synthetic authentication, serves no tools."""
    def __init__(self):
        self.counts = {"initial": 0, "replacement": 0, "rejected": 0}
        self.replacement_seen = threading.Event()
        owner = self

        class Handler(BaseHTTPRequestHandler):
            def log_message(self, *args):
                pass

            def do_POST(self):
                header = self.headers.get("Authorization")
                stage = next((name for name in ("initial", "replacement")
                              if header == "Bearer synthetic-probe-" + name), None)
                if stage is None:
                    owner.counts["rejected"] += 1
                    self.send_error(403)
                    return
                owner.counts[stage] += 1
                if stage == "replacement":
                    owner.replacement_seen.set()
                length = int(self.headers.get("Content-Length", "0"))
                if not 0 < length <= 65536:
                    self.send_error(413)
                    return
                request = json.loads(self.rfile.read(length))
                if "id" not in request:
                    self.send_response(202)
                    self.end_headers()
                    return
                method = request.get("method")
                if method == "initialize":
                    result = {"protocolVersion": request.get("params", {}).get("protocolVersion"),
                              "capabilities": {"tools": {}},
                              "serverInfo": {"name": "probe", "version": "1.0.0"}}
                elif method == "tools/list":
                    result = {"tools": []}
                elif method == "ping":
                    result = {}
                else:
                    self.send_error(405)
                    return
                payload = json.dumps({"jsonrpc": "2.0", "id": request["id"], "result": result}).encode()
                self.send_response(200)
                self.send_header("Content-Type", "application/json")
                self.send_header("Content-Length", str(len(payload)))
                self.end_headers()
                self.wfile.write(payload)

            def do_GET(self):
                self.send_error(405)

            def do_DELETE(self):
                self.send_error(405)

        self.server = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
        threading.Thread(target=self.server.serve_forever, daemon=True).start()

    def descriptor(self, stage):
        return {"type": "http", "name": "probe", "url":
                "http://127.0.0.1:" + str(self.server.server_port) + "/mcp",
                "headers": [{"name": "Authorization", "value": "Bearer synthetic-probe-" + stage}]}

    def close(self):
        self.server.shutdown()
        self.server.server_close()


class ProbeFailure(Exception):
    pass


class OwnedJob:
    def __init__(self, process):
        class Limits(ctypes.Structure):
            _fields_ = [("process_time", ctypes.c_longlong),
                        ("job_time", ctypes.c_longlong), ("flags", wintypes.DWORD),
                        ("minimum", ctypes.c_size_t), ("maximum", ctypes.c_size_t),
                        ("active", wintypes.DWORD), ("affinity", ctypes.c_size_t),
                        ("priority", wintypes.DWORD), ("scheduling", wintypes.DWORD)]

        class Extended(ctypes.Structure):
            _fields_ = [("basic", Limits), ("io", ctypes.c_ulonglong * 6),
                        ("process_memory", ctypes.c_size_t),
                        ("job_memory", ctypes.c_size_t),
                        ("peak_process", ctypes.c_size_t),
                        ("peak_job", ctypes.c_size_t)]

        self.api = ctypes.WinDLL("kernel32", use_last_error=True)
        self.api.CreateJobObjectW.argtypes = [ctypes.c_void_p, wintypes.LPCWSTR]
        self.api.CreateJobObjectW.restype = wintypes.HANDLE
        self.api.SetInformationJobObject.argtypes = [wintypes.HANDLE, ctypes.c_int,
                                                    ctypes.c_void_p, wintypes.DWORD]
        self.api.AssignProcessToJobObject.argtypes = [wintypes.HANDLE, wintypes.HANDLE]
        self.api.CloseHandle.argtypes = [wintypes.HANDLE]
        self.handle = self.api.CreateJobObjectW(None, None)
        info = Extended()
        info.basic.flags = 0x2000  # JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE
        if not self.handle or not self.api.SetInformationJobObject(
                self.handle, 9, ctypes.byref(info), ctypes.sizeof(info)):
            self.close()
            raise ProbeFailure("job_setup_failed")
        if not self.api.AssignProcessToJobObject(self.handle, int(process._handle)):
            self.close()
            raise ProbeFailure("job_assignment_failed")

    def close(self):
        if self.handle:
            self.api.CloseHandle(self.handle)
            self.handle = None


class Peer:
    def __init__(self, command, root, environment, observations):
        self.events = queue.Queue(maxsize=1024)
        self.observations = observations
        self.next_id = 0
        self.job = None
        self.permission_failure = False
        self.memory_failure = False
        # The child reads exactly the bootstrap line, so no buffered RPC bytes
        # can be swallowed before the provider inherits the input pipe.
        launch = [sys.executable, str(Path(__file__).resolve()), "--child",
                  json.dumps(command)]
        self.process = subprocess.Popen(launch, cwd=root, env=environment,
                                        stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                                        stderr=subprocess.PIPE,
                                        creationflags=subprocess.CREATE_NO_WINDOW)
        try:
            self.job = OwnedJob(self.process)
            self.process.stdin.write(b"start\n")
            self.process.stdin.flush()
        except BaseException:
            self.process.kill()
            self.process.wait(timeout=5)
            raise
        threading.Thread(target=self.read, daemon=True).start()
        threading.Thread(target=self.read_errors, daemon=True).start()

    def read_errors(self):
        while True:
            line = self.process.stderr.readline(65536)
            if not line:
                return
            lower = line.lower()
            self.permission_failure |= any(token in lower for token in
                (b"permission denied", b"access is denied", b"eacces", b"eperm"))
            self.memory_failure |= any(token in lower for token in
                (b"out of memory", b"heap limit", b"allocation failed"))

    def read(self):
        try:
            while True:
                line = self.process.stdout.readline(1024 * 1024 + 1)
                if not line:
                    self.events.put_nowait(None)
                    return
                if len(line) > 1024 * 1024:
                    self.events.put_nowait({"probe_failure": "oversize_frame"})
                    return
                try:
                    value = json.loads(line)
                except (ValueError, UnicodeError):
                    continue  # Never retain non-protocol output.
                self.events.put_nowait(value)
        except (OSError, queue.Full):
            return

    def send(self, value):
        self.process.stdin.write(json.dumps(value).encode() + b"\n")
        self.process.stdin.flush()

    def call(self, method, params, timeout=25):
        self.next_id += 1
        request_id = self.next_id
        self.send({"jsonrpc": "2.0", "id": request_id,
                   "method": method, "params": params})
        deadline = time.monotonic() + timeout
        row = {"method": method}
        self.observations.append(row)
        while True:
            try:
                value = self.events.get(timeout=max(0, deadline - time.monotonic()))
            except queue.Empty:
                row["outcome"] = "timeout"
                raise ProbeFailure("request_timeout") from None
            if value is None:
                row["outcome"] = "process_exit"
                raise ProbeFailure("process_exit")
            if not isinstance(value, dict):
                continue
            if "probe_failure" in value:
                raise ProbeFailure(value["probe_failure"])
            if "method" in value:
                # No permission, filesystem, terminal or elicitation operation
                # is authorized by this no-prompt probe.
                if "id" in value:
                    self.send({"jsonrpc": "2.0", "id": value["id"],
                               "error": {"code": -32601, "message": "probe refuses request"}})
                elif value["method"] == "session/update":
                    row["replayed_updates"] = row.get("replayed_updates", 0) + 1
                    update = value.get("params", {}).get("update", {})
                    if (update.get("sessionUpdate") == "user_message_chunk"
                            and MARKER in json.dumps(update)):
                        row["synthetic_input_marker_seen"] = True
                continue
            if value.get("id") != request_id:
                continue
            if "error" in value:
                row["outcome"] = "refused"
                code = value["error"].get("code")
                if isinstance(code, int):
                    row["code"] = code
                return None
            row["outcome"] = "answered"
            return value.get("result", {})

    def close(self):
        if self.job:
            self.job.close()
        self.process.wait(timeout=10)
        self.process.stdin.close()
        self.process.stdout.close()
        self.process.stderr.close()


def initialize(peer, protocol, result):
    if protocol == "app-server":
        reply = peer.call("initialize", {
            "clientInfo": {"name": "bots_protocol_probe", "version": "1.0.0"},
            "capabilities": {"experimentalApi": False}})
        if reply is not None:
            peer.send({"jsonrpc": "2.0", "method": "initialized", "params": {}})
        return reply is not None
    reply = peer.call("initialize", {"protocolVersion": 1,
        "clientInfo": {"name": "bots_protocol_probe", "version": "1.0.0"},
        "clientCapabilities": {"fs": {"readTextFile": False, "writeTextFile": False},
                               "terminal": False}})
    if reply is None:
        return False
    caps = reply.get("agentCapabilities", {})
    result["load_session_advertised"] = caps.get("loadSession") is True
    result["http_mcp_advertised"] = caps.get("mcpCapabilities", {}).get("http") is True
    version = reply.get("agentInfo", {}).get("version", "")
    if re.fullmatch(r"[0-9][0-9A-Za-z.+-]{0,60}", version):
        result["protocol_reported_version"] = version
    return True


def run(args):
    root = Path(args.root).resolve()
    root.mkdir(parents=True, exist_ok=False)
    environment = dict(os.environ)
    (root / "temp").mkdir()
    environment["TEMP"] = str(root / "temp")
    environment["TMP"] = str(root / "temp")
    environment["GROK_SUBAGENTS"] = "0"
    environment["GROK_DISABLE_AUTOUPDATER"] = "1"
    # A wholly disposable app-server home, with no copied credentials/config.
    if args.protocol == "app-server":
        environment["CODEX_HOME"] = str(root / "provider-home")
        (root / "provider-home").mkdir()
    command = [args.program, *args.arg]
    result = {"protocol": args.protocol, "no_prompt": not args.prompt, "observations": [],
              "input_replay_proven": False, "credential_replacement_proven": False,
              "call_correlation_proven": False, "steering_proven": False}
    peer = None
    endpoint = LocalEndpoint() if args.mcp_rebind else None
    try:
        peer = Peer(command, root, environment, result["observations"])
        if not initialize(peer, args.protocol, result):
            raise ProbeFailure("initialize_refused")
        if args.protocol == "acp":
            if args.cached_auth:
                peer.call("authenticate", {"methodId": "cached_token", "_meta": {"headless": True}})
            params = {"cwd": str(root), "mcpServers": []}
            if endpoint:
                params["mcpServers"] = [endpoint.descriptor("initial")]
            if args.restricted_session:
                params["_meta"] = {"claudeCode": {"options": {
                    "tools": [], "settingSources": [], "strictMcpConfig": True}}}
            reply = peer.call("session/new", params)
            session = reply.get("sessionId") if reply else None
        else:
            reply = peer.call("thread/start", {"cwd": str(root), "ephemeral": False,
                "approvalPolicy": "on-request", "sandbox": "read-only"})
            session = reply.get("thread", {}).get("id") if reply else None
        result["session_created"] = isinstance(session, str) and bool(session)
        if result["session_created"]:
            if args.prompt:
                if args.protocol != "acp" or not args.restricted_session:
                    raise ProbeFailure("prompt_requires_restricted_acp_session")
                reply = peer.call("session/prompt", {"sessionId": session,
                    "prompt": [{"type": "text", "text": MARKER +
                        "\nReply with only OK. Do not call any tools."}]}, timeout=55)
                result["synthetic_turn_completed"] = bool(reply and reply.get("stopReason") == "end_turn")
            peer.close()
            peer = Peer(command, root, environment, result["observations"])
            if not initialize(peer, args.protocol, result):
                raise ProbeFailure("restart_initialize_refused")
            if args.protocol == "acp":
                if args.cached_auth:
                    peer.call("authenticate", {"methodId": "cached_token", "_meta": {"headless": True}})
                params["sessionId"] = session
                if endpoint:
                    params["mcpServers"] = [endpoint.descriptor("replacement")]
                reply = peer.call("session/load", params)
                if endpoint and reply is not None:
                    endpoint.replacement_seen.wait(timeout=5)
                result["input_replay_proven"] = bool(reply is not None and
                    result["observations"][-1].get("synthetic_input_marker_seen"))
            else:
                reply = peer.call("thread/resume", {"threadId": session,
                    "cwd": str(root), "approvalPolicy": "on-request", "sandbox": "read-only"})
            result["session_reload_answered"] = reply is not None
    except ProbeFailure as error:
        result["failure"] = str(error)
    except (OSError, subprocess.TimeoutExpired):
        result["failure"] = "process_io_failure"
    finally:
        if peer:
            result["permission_failure_observed"] = peer.permission_failure
            result["memory_failure_observed"] = peer.memory_failure
            peer.close()
        if endpoint:
            endpoint.close()
            result["synthetic_mcp_requests"] = endpoint.counts
            result["credential_replacement_proven"] = all(
                endpoint.counts[stage] > 0 for stage in ("initial", "replacement"))
    output = root / "protocol-observations.json"
    output.write_text(json.dumps(result, indent=2) + "\n", encoding="utf-8")
    print(json.dumps(result, indent=2))


if __name__ == "__main__":
    if len(sys.argv) > 1 and sys.argv[1] == "--child":
        bootstrap = bytearray()
        while not bootstrap.endswith(b"\n"):
            token = os.read(sys.stdin.fileno(), 1)
            if not token:
                sys.exit(2)
            bootstrap.extend(token)
        sys.exit(subprocess.call(json.loads(sys.argv[2])))
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--run", action="store_true", help="explicitly opt into real processes")
    parser.add_argument("--protocol", choices=["acp", "app-server"], required=True)
    parser.add_argument("--program", required=True)
    parser.add_argument("--arg", action="append", default=[])
    parser.add_argument("--root", required=True, help="new disposable directory; must not exist")
    parser.add_argument("--cached-auth", action="store_true", help="use the route's cached_token method; never opens login")
    parser.add_argument("--restricted-session", action="store_true", help="wrapper options: no tools, settings or inherited MCP")
    parser.add_argument("--prompt", action="store_true", help="opt into one synthetic model turn in a restricted session")
    parser.add_argument("--mcp-rebind", action="store_true", help="observe fresh synthetic bearer at a tool-free local MCP endpoint")
    args = parser.parse_args()
    if not args.run or os.name != "nt":
        parser.error("This probe requires Windows and explicit --run")
    run(args)
