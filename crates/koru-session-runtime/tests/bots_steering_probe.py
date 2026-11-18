# SPDX-License-Identifier: MPL-2.0
"""Opt-in native steering proof; ordinary test runs never import this file.

Only synthetic text is sent. Native session records and temporary files live
under a new disposable root. Existing Claude OAuth is passed privately in the
child environment, never copied to an evidence file or printed. The inherited
kill-on-close job owns only this probe's process tree.
"""
import argparse
import importlib.util
import json
import os
from pathlib import Path
import queue
import time


def run(args):
    source = Path(__file__).with_name("bots_protocol_probe.py")
    spec = importlib.util.spec_from_file_location("probe", source)
    probe = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(probe)
    root = Path(args.root).resolve()
    if not root.is_absolute() or root.exists():
        raise SystemExit("A new disposable root is required")
    root.mkdir(parents=True)
    environment = dict(os.environ)
    config = root / "provider-home"
    scratch = root / "temp"
    config.mkdir()
    scratch.mkdir()
    environment.update(CLAUDE_CONFIG_DIR=str(config), TEMP=str(scratch), TMP=str(scratch))
    result = {"isolated_native_state": True, "steering_advertised": False,
              "steering_acknowledged": False, "turn_completed": False,
              "replayed_initial_input": False, "replayed_steer_input": False,
              "observations": []}
    peer = None
    initial = "synthetic-bots-initial-v1\nWrite 80 short numbered lines about counting. Do not use any tools."
    steer = "synthetic-bots-steer-v1\nStop counting now and reply only STEER_RECEIVED. Do not use any tools."
    try:
        if not environment.get("CLAUDE_CODE_OAUTH_TOKEN") and not environment.get("ANTHROPIC_API_KEY"):
            credential = Path.home() / ".claude" / ".credentials.json"
            try:
                token = json.loads(credential.read_text(encoding="utf-8"))["claudeAiOauth"]["accessToken"]
            except (OSError, ValueError, KeyError):
                raise probe.ProbeFailure("isolated_auth_unavailable") from None
            if not isinstance(token, str) or not token:
                raise probe.ProbeFailure("isolated_auth_unavailable")
            environment["CLAUDE_CODE_OAUTH_TOKEN"] = token
        command = [args.program]
        peer = probe.Peer(command, root, environment, result["observations"])
        initialized = peer.call("initialize", {"protocolVersion": 1,
            "clientInfo": {"name": "bots_steering_probe", "version": "1.0.0"},
            "clientCapabilities": {"fs": {"readTextFile": False, "writeTextFile": False}, "terminal": False}})
        if not initialized:
            raise probe.ProbeFailure("initialize_refused")
        result["steering_advertised"] = initialized.get("_meta", {}).get("steering", {}).get("supported") is True
        if not result["steering_advertised"]:
            raise probe.ProbeFailure("steering_not_advertised")
        params = {"cwd": str(root), "mcpServers": [], "_meta": {"claudeCode": {"options": {
            "tools": [], "settingSources": [], "strictMcpConfig": True}}}}
        session = peer.call("session/new", params)
        if not session or "sessionId" not in session:
            raise probe.ProbeFailure("new_session_refused")
        session_id = session["sessionId"]
        peer.next_id += 1
        prompt_id = peer.next_id
        peer.send({"jsonrpc": "2.0", "id": prompt_id, "method": "session/prompt",
                   "params": {"sessionId": session_id, "prompt": [{"type": "text", "text": initial}]}})
        steer_id = None
        deadline = time.monotonic() + 90
        while time.monotonic() < deadline:
            try:
                event = peer.events.get(timeout=max(0.01, deadline-time.monotonic()))
            except queue.Empty:
                raise probe.ProbeFailure("turn_timeout") from None
            if event is None:
                raise probe.ProbeFailure("process_exit")
            if event.get("method"):
                if "id" in event:
                    peer.send({"jsonrpc": "2.0", "id": event["id"], "error": {"code": -32601, "message": "probe refuses request"}})
                update = event.get("params", {}).get("update", {})
                if steer_id is None and update.get("sessionUpdate") == "agent_message_chunk":
                    peer.next_id += 1
                    steer_id = peer.next_id
                    peer.send({"jsonrpc": "2.0", "id": steer_id, "method": "_session/steering", "params": {
                        "sessionId": session_id, "prompt": [{"type": "text", "text": steer}],
                        "_meta": {"steering": {"idleBehavior": "promptRequired"}}}})
            elif event.get("id") == steer_id:
                result["steering_acknowledged"] = event.get("result", {}).get("outcome") == "injected"
            elif event.get("id") == prompt_id:
                result["turn_completed"] = event.get("result", {}).get("stopReason") == "end_turn"
                break
        if not result["turn_completed"] or not result["steering_acknowledged"]:
            raise probe.ProbeFailure("steering_not_confirmed")
        peer.close()
        peer = probe.Peer(command, root, environment, result["observations"])
        if not probe.initialize(peer, "acp", result):
            raise probe.ProbeFailure("resume_initialize_refused")
        peer.next_id += 1
        load_id = peer.next_id
        peer.send({"jsonrpc": "2.0", "id": load_id, "method": "session/load", "params": {**params, "sessionId": session_id}})
        deadline = time.monotonic() + 40
        while time.monotonic() < deadline:
            try:
                event = peer.events.get(timeout=max(0.01, deadline-time.monotonic()))
            except queue.Empty:
                raise probe.ProbeFailure("resume_timeout") from None
            if event is None:
                raise probe.ProbeFailure("resume_process_exit")
            update = event.get("params", {}).get("update", {})
            if update.get("sessionUpdate") == "user_message_chunk":
                text = update.get("content", {}).get("text")
                result["replayed_initial_input"] |= text == initial
                result["replayed_steer_input"] |= text == steer
            if event.get("id") == load_id and "method" not in event:
                if "error" in event:
                    raise probe.ProbeFailure("resume_refused")
                break
            if "method" in event and "id" in event:
                peer.send({"jsonrpc": "2.0", "id": event["id"], "error": {"code": -32601, "message": "probe refuses request"}})
    except probe.ProbeFailure as error:
        result["failure"] = str(error)
    except (OSError, ValueError):
        result["failure"] = "probe_io_failure"
    finally:
        if peer:
            result["permission_failure_observed"] = peer.permission_failure
            result["memory_failure_observed"] = peer.memory_failure
            peer.close()
    (root / "observations.json").write_text(json.dumps(result, indent=2)+"\n", encoding="utf-8")
    print(json.dumps(result, indent=2), flush=True)
    return 1 if "failure" in result else 0


if __name__ == "__main__":
    parser = argparse.ArgumentParser()
    parser.add_argument("--run", action="store_true")
    parser.add_argument("--root")
    parser.add_argument("--program")
    args = parser.parse_args()
    if not args.run:
        print("SKIP opt-in native steering proof")
    elif not args.root or not args.program:
        parser.error("--run requires --root and --program")
    else:
        raise SystemExit(run(args))
