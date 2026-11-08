#!/usr/bin/env python3
# SPDX-License-Identifier: MPL-2.0
"""Map the existing public API settings onto an Android release build.

Official nightlies used to pass no account settings (NIGHTLY N5), so the
installed app left sign-in unavailable and Cloud sync detached. Desktop
re-enablement bakes three values from the GitHub environment:

  KORU_PUBLIC_API_ORIGIN
  KORU_PUBLIC_API_ISSUER
  KORU_PUBLIC_API_CREDENTIAL_ROLLOUT

Android already reads those as the Gradle properties koruAccountOrigin,
koruAccountIssuer and koruAccountCredentialRollout. Sign-in also needs the
web client id the local launcher already documents, KORU_PUBLIC_API_GOOGLE_CLIENT_ID,
and the credential channel the desktop uses for the same release (nightly or
stable). KORU_GOOGLE_DOCS_CLIENT_ID is the Docs importer's desktop client and
is never read here.

A build that does not ask for Cloud prints no properties. Gradle then leaves
the account fields empty, which is the offline app: sign-in stays unavailable
and Cloud sync stays detached. This does not create or change a Google client.
"""

from __future__ import annotations

import argparse
import json
import os
import sys
import urllib.parse

# koru-account-session REFRESH_ABSOLUTE_MILLIS. A legacy cutover that cannot
# add this span is rejected by RegionalCredentialPolicy::validate.
REFRESH_ABSOLUTE_MILLIS = 365 * 24 * 60 * 60 * 1000
ROLLOUT_KEYS = {
    "region",
    "regional_issuer",
    "legacy_issuer",
    "legacy_cutover_millis",
    "issuance",
    "legacy_access_kids",
    "legacy_attestation_kids",
}
I64_MAX = 2**63 - 1


class ConfigError(Exception):
    """The cloud Android build's public configuration is not usable."""


def release_args(channel: str, with_cloud: bool, env: dict[str, str] | None = None) -> list[str]:
    """Gradle `-P` arguments for this Android release, or none when Cloud is off."""
    if not with_cloud:
        return []
    if channel not in ("nightly", "stable"):
        raise ConfigError(
            f"Android Cloud builds use the nightly or stable channel, not {channel!r}."
        )
    values = env if env is not None else os.environ
    origin = _required(values, "KORU_PUBLIC_API_ORIGIN")
    issuer = _required(values, "KORU_PUBLIC_API_ISSUER")
    client = _required_client(values)
    rollout = _required(values, "KORU_PUBLIC_API_CREDENTIAL_ROLLOUT")
    _hosted_origin("KORU_PUBLIC_API_ORIGIN", origin)
    _hosted_origin("KORU_PUBLIC_API_ISSUER", issuer)
    _rollout(rollout, issuer)
    return [
        f"-PkoruAccountOrigin={origin}",
        f"-PkoruAccountIssuer={issuer}",
        f"-PkoruGoogleServerClientId={client}",
        f"-PkoruAccountChannel={channel}",
        f"-PkoruAccountCredentialRollout={rollout}",
    ]


def _required(env: dict[str, str], name: str) -> str:
    value = env.get(name, "")
    if value.strip() == "" or any(ord(ch) < 32 or ch == "\x7f" for ch in value):
        raise ConfigError(
            f"{name} is empty in this environment. "
            "A Cloud Android build uses the same public value the desktop build bakes in. "
            "Nothing here creates a new client or registration."
        )
    if "\n" in value or len(value) > 8192:
        raise ConfigError(f"{name} is not a single public configuration value.")
    return value.strip()


def _required_client(env: dict[str, str]) -> str:
    name = "KORU_PUBLIC_API_GOOGLE_CLIENT_ID"
    raw = env.get(name, "")
    if raw.strip() == "":
        docs = ""
        if env.get("KORU_GOOGLE_DOCS_CLIENT_ID", "").strip():
            docs = (
                " KORU_GOOGLE_DOCS_CLIENT_ID is the Docs importer's desktop client "
                "and is not the account web client."
            )
        raise ConfigError(
            f"{name} is empty. Android sign-in uses that existing public web client id, "
            "the value a local build already copies into koruGoogleServerClientId. "
            f"It is not a new client.{docs}"
        )
    value = _required(env, name)
    if len(value) > 256 or not _web_client(value):
        raise ConfigError(
            f"{name} must be the existing public web OAuth client id "
            "(….apps.googleusercontent.com). It is not a new client."
        )
    return value


def _web_client(value: str) -> bool:
    prefix, dot, suffix = value.rpartition(".")
    if not dot or suffix != "com":
        return False
    # apps.googleusercontent.com, and a non-empty client prefix.
    if not prefix.endswith(".apps.googleusercontent"):
        return False
    head = prefix[: -len(".apps.googleusercontent")]
    if not head:
        return False
    return all(ch.isalnum() or ch in "._-" for ch in head)


def _hosted_origin(name: str, value: str) -> None:
    parsed = urllib.parse.urlparse(value)
    host = (parsed.hostname or "").lower()
    loopback = host in {"localhost", "127.0.0.1", "::1"}
    if (
        parsed.scheme != "https"
        or not host
        or loopback
        or parsed.username
        or parsed.password
        or parsed.query
        or parsed.fragment
        or parsed.path not in ("", "/")
        or parsed.port == 0
    ):
        raise ConfigError(
            f"{name} must be the hosted HTTPS origin already used for Koru Cloud, "
            "with no path, query, or credentials."
        )


def _rollout(value: str, account_issuer: str) -> None:
    if len(value) > 8192:
        raise ConfigError("KORU_PUBLIC_API_CREDENTIAL_ROLLOUT is larger than the account policy accepts.")
    try:
        policy = json.loads(value)
    except json.JSONDecodeError as error:
        raise ConfigError(
            "KORU_PUBLIC_API_CREDENTIAL_ROLLOUT is not the public regional credential policy."
        ) from error
    if not isinstance(policy, dict) or set(policy) != ROLLOUT_KEYS:
        raise ConfigError(
            "KORU_PUBLIC_API_CREDENTIAL_ROLLOUT does not match the regional credential policy."
        )
    if policy["region"] != "US":
        raise ConfigError("KORU_PUBLIC_API_CREDENTIAL_ROLLOUT must name the US region.")
    issuance = policy["issuance"]
    if issuance not in ("legacy", "regional"):
        raise ConfigError("KORU_PUBLIC_API_CREDENTIAL_ROLLOUT has an unknown issuance stage.")
    regional = _issuer(policy["regional_issuer"], "regional_issuer")
    legacy = _issuer(policy["legacy_issuer"], "legacy_issuer")
    if regional == legacy:
        raise ConfigError("KORU_PUBLIC_API_CREDENTIAL_ROLLOUT repeats one issuer for both stages.")
    cutover = policy["legacy_cutover_millis"]
    if isinstance(cutover, bool) or not isinstance(cutover, int):
        raise ConfigError("KORU_PUBLIC_API_CREDENTIAL_ROLLOUT has no integer legacy cutover.")
    if cutover < 0 or (issuance == "legacy" and cutover == 0) or cutover > I64_MAX - REFRESH_ABSOLUTE_MILLIS:
        raise ConfigError("KORU_PUBLIC_API_CREDENTIAL_ROLLOUT has a legacy cutover the phone will refuse.")
    access = _kids(policy["legacy_access_kids"], "legacy_access_kids")
    attestation = _kids(policy["legacy_attestation_kids"], "legacy_attestation_kids")
    if set(access) & set(attestation):
        raise ConfigError("KORU_PUBLIC_API_CREDENTIAL_ROLLOUT reuses a key id for two purposes.")
    expected = legacy if issuance == "legacy" else regional
    if account_issuer != expected:
        raise ConfigError(
            "KORU_PUBLIC_API_ISSUER does not match the issuer named by "
            "KORU_PUBLIC_API_CREDENTIAL_ROLLOUT, so Cloud sync would refuse every credential."
        )


def _issuer(value: object, field: str) -> str:
    if not isinstance(value, str) or not value or len(value) > 512 or any(ch.isspace() or ord(ch) < 32 for ch in value):
        raise ConfigError(f"KORU_PUBLIC_API_CREDENTIAL_ROLLOUT has an unusable {field}.")
    return value


def _kids(value: object, field: str) -> list[str]:
    if not isinstance(value, list) or len(value) > 16:
        raise ConfigError(f"KORU_PUBLIC_API_CREDENTIAL_ROLLOUT has an unusable {field}.")
    kids: list[str] = []
    for kid in value:
        if (
            not isinstance(kid, str)
            or not kid
            or len(kid) > 128
            or any(not (ch.isalnum() or ch in "-_.") for ch in kid)
        ):
            raise ConfigError(f"KORU_PUBLIC_API_CREDENTIAL_ROLLOUT has an unusable {field} entry.")
        kids.append(kid)
    return kids


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    sub = parser.add_subparsers(dest="command", required=True)
    release = sub.add_parser("release-args")
    release.add_argument("--channel", required=True)
    release.add_argument("--with-cloud", required=True, choices=("true", "false"))
    args = parser.parse_args(argv)
    try:
        lines = release_args(args.channel, args.with_cloud == "true")
    except ConfigError as error:
        print(f"::error::{error}", file=sys.stderr)
        return 1
    for line in lines:
        print(line)
    if lines:
        print(
            f"Android account configuration: channel {args.channel}, sign-in and Cloud sync attached.",
            file=sys.stderr,
        )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
