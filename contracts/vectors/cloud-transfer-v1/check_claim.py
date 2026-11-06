"""Independent check of claim.json against the amended transfer field table.

Read-only, standard library only, no network. Run from this directory:

    python check_claim.py

It re-derives every expected result from the field table of
CloudTransferStartV1 as amended for app connections, never from the Rust
decoder, and checks three things for each case: the amended contract's result
(`code`), the result of the decoder as it stood before the amendment
(`today`), and, where a case names one, that the bytes are identical to the
frozen case they claim to be. The Rust tests read the same file; this script
is not evidence that they pass.
"""

import base64
import hashlib
import json
import re
import sys
from pathlib import Path

HERE = Path(__file__).resolve().parent
INVALID = "sync_transfer_invalid"
MAX_CONTROL = 4096
MAX_PAYLOAD = 16_777_216
I64_MAX = 2**63 - 1
UUID_V4 = re.compile(r"^[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$")
B64URL = re.compile(r"^[A-Za-z0-9_-]*$")
RECEIVERS = {"worker_device_session", "worker_app_connection_session", "client"}
# The two refusals the amendment adds. Before it, an unknown code on a Cloud
# stream decoded as the handshake's generic malformed refusal.
NEW_REFUSALS = {"sync_unit_exists", "sync_document_claimed"}
TODAY_UNKNOWN_REFUSAL = "handshake_malformed"


class Refused(Exception):
    pass


def strict_object(pairs):
    keys = [key for key, _ in pairs]
    if len(keys) != len(set(keys)):
        raise Refused("duplicate property")
    return dict(pairs)


def parse(text):
    if len(text.encode()) > MAX_CONTROL:
        raise Refused("over the control cap")

    def no_constant(name):
        raise Refused(f"non-JSON constant {name}")

    try:
        return json.loads(text, object_pairs_hook=strict_object, parse_constant=no_constant)
    except json.JSONDecodeError as error:  # includes trailing data
        raise Refused(str(error)) from None


def integer(value, low, high):
    if type(value) is not int or not low <= value <= high:
        raise Refused("integer out of range")
    return value


def uuid_v4(value):
    if type(value) is not str or not UUID_V4.match(value):
        raise Refused("not a canonical lowercase v4 UUID")


def bytes32(value):
    """Unpadded, canonical base64url of exactly 32 bytes."""
    if type(value) is not str or len(value) != 43 or not B64URL.match(value):
        raise Refused("not 43 base64url characters")
    decoded = base64.urlsafe_b64decode(value + "=")
    if len(decoded) != 32 or base64.urlsafe_b64encode(decoded).decode().rstrip("=") != value:
        raise Refused("noncanonical base64url")
    return decoded


def exact(obj, required, optional=()):
    if type(obj) is not dict:
        raise Refused("not an object")
    keys = set(obj)
    if not set(required) <= keys or not keys <= set(required) | set(optional):
        raise Refused("missing or unknown property")
    if any(value is None for value in obj.values()):
        raise Refused("null property")


UNIT_ARMS = {
    "document": ("workspace_id", "document_id"),
    "identity": ("profile_id",),
    "private": ("profile_id",),
    "personal": ("profile_id", "workspace_id"),
}


def unit(value):
    if type(value) is not dict or value.get("kind") not in UNIT_ARMS:
        raise Refused("unknown unit arm")
    fields = UNIT_ARMS[value["kind"]]
    exact(value, ("kind",) + fields)
    for field in fields:
        uuid_v4(value[field])


def batch(value):
    if type(value) is not list or len(value) != 8:
        raise Refused("batch_id is not 8 integers")
    for item in value:
        integer(item, 0, 255)


def control(obj, receiver, amended):
    """The tag-1 control table. `amended` adds `claim` to the start only."""
    kind = obj.get("kind") if type(obj) is dict else None
    if kind == "transfer_policy":
        exact(obj, ("kind", "version", "policy_generation", "automatic_limit_bytes"))
        integer(obj["version"], 1, 1)
        integer(obj["policy_generation"], 1, I64_MAX)
        if obj["automatic_limit_bytes"] not in (0, 200_000, MAX_PAYLOAD) or type(
            obj["automatic_limit_bytes"]
        ) is not int:
            raise Refused("policy ceiling")
    elif kind == "transfer_decision":
        exact(obj, ("kind", "version", "policy_generation", "unit", "batch_id", "sha256", "decision"))
        integer(obj["version"], 1, 1)
        integer(obj["policy_generation"], 1, I64_MAX)
        unit(obj["unit"])
        batch(obj["batch_id"])
        bytes32(obj["sha256"])
        if obj["decision"] not in ("accept", "defer"):
            raise Refused("decision")
    elif kind == "transfer_start":
        required = ("kind", "version", "policy_generation", "unit", "batch_id",
                    "total_length", "fragment_count", "sha256")
        exact(obj, required, ("claim",) if amended else ())
        integer(obj["version"], 1, 1)
        integer(obj["policy_generation"], 1, I64_MAX)
        unit(obj["unit"])
        batch(obj["batch_id"])
        length = integer(obj["total_length"], 1, MAX_PAYLOAD)
        fragments = integer(obj["fragment_count"], 1, 16_384)
        if fragments > length:
            raise Refused("more fragments than bytes")
        bytes32(obj["sha256"])
        if "claim" in obj:
            bytes32(obj["claim"])
            # Accepted only from an app connection's session. A device session
            # and a client receiving a worker's start refuse it.
            if receiver != "worker_app_connection_session":
                raise Refused("claim outside an app connection session")
    else:
        raise Refused("unknown control kind")


def expected_control(text, receiver, amended):
    try:
        control(parse(text), receiver, amended)
        return "ok"
    except Refused:
        return INVALID


def compact(text):
    """The canonical encoder spelling of a parsed value, keys in input order."""
    return json.dumps(parse(text), separators=(",", ":"), ensure_ascii=False)


def record(text, tag):
    payload = text.encode()
    return ((len(payload) + 1).to_bytes(4, "big") + bytes([tag]) + payload).hex()


def main():
    vectors = json.loads((HERE / "claim.json").read_text(encoding="utf-8"))
    frozen = {
        case["id"]: case
        for name in ("cases.json", "supplement.json")
        for case in json.loads((HERE / name).read_text(encoding="utf-8"))
    }
    handle = vectors["claim_handle_fixture"].encode()
    fixture = base64.urlsafe_b64encode(hashlib.sha256(handle).digest()).decode().rstrip("=")
    assert fixture == vectors["claim_sha256_base64url"], "claim fixture is SHA-256 of the handle"

    seen, checked = set(), 0
    for case in vectors["cases"]:
        name = case["id"]
        assert name not in seen, f"{name}: duplicate id"
        seen.add(name)
        assert case["receiver"] in RECEIVERS, f"{name}: receiver"
        text = case["json"]
        if case["kind"] == "control":
            assert case["code"] == expected_control(text, case["receiver"], True), f"{name}: amended"
            assert case["today"] == expected_control(text, case["receiver"], False), f"{name}: today"
            if case["code"] == "ok":
                # Every accepted value is compact, with no whitespace to drift.
                assert compact(text) == text, f"{name}: not compact"
                if '"claim"' in text:
                    assert parse(text)["claim"] == vectors["claim_sha256_base64url"], name
            if "record_hex" in case:
                assert case["record_hex"] == record(text, 1), f"{name}: record bytes"
        elif case["kind"] == "peer":
            frame = parse(text)
            exact(frame, ("kind", "code"))
            assert frame["kind"] == "error" and frame["code"] in NEW_REFUSALS, name
            assert case["code"] == frame["code"], f"{name}: a refusal decodes as itself"
            assert case["today"] == TODAY_UNKNOWN_REFUSAL, f"{name}: today"
            assert case["receiver"] == "client", f"{name}: only a client receives it"
            assert case["record_hex"] == record(text, 0), f"{name}: record bytes"
        else:
            raise AssertionError(f"{name}: unknown kind")
        if "byte_identical_to" in case:
            original = frozen[case["byte_identical_to"]]
            assert original["code"] == "ok", name
            assert text == original["json"], f"{name}: device bytes changed"
        checked += 1

    # The cases the plan names must all be present.
    by = {case["id"]: case for case in vectors["cases"]}
    assert any(c["receiver"] == "worker_device_session" and '"claim"' not in c["json"]
               and c["code"] == "ok" for c in by.values()), "a device start without claim"
    assert any(c["receiver"] == "worker_app_connection_session" and '"claim"' in c["json"]
               and c["code"] == "ok" for c in by.values()), "a connection start with claim"
    assert any(c["receiver"] == "worker_device_session" and '"claim"' in c["json"]
               and c["code"] == INVALID for c in by.values()), "a device session carrying one"
    assert {c["code"] for c in by.values() if c["kind"] == "peer"} == NEW_REFUSALS
    print(f"claim.json: {checked} cases match the amended field table")


if __name__ == "__main__":
    try:
        main()
    except AssertionError as error:
        print(f"claim.json: FAILED {error}", file=sys.stderr)
        sys.exit(1)
