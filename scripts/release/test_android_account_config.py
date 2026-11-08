#!/usr/bin/env python3
# SPDX-License-Identifier: MPL-2.0
"""The Android Cloud build maps existing public settings and stays offline without them.

    python3 scripts/release/test_android_account_config.py
"""

import importlib.util
import os
import subprocess
import sys
import unittest
from pathlib import Path

SCRIPT = Path(__file__).resolve().parent / "android-account-config.py"
spec = importlib.util.spec_from_file_location("android_account_config", SCRIPT)
assert spec and spec.loader
config = importlib.util.module_from_spec(spec)
spec.loader.exec_module(config)

ORIGIN = "https://api.example.test"
ISSUER = "https://issuer.example.test"
CLIENT = "fixture-web.apps.googleusercontent.com"
ROLLOUT = (
    '{"region":"US","regional_issuer":"https://regional.example.test",'
    '"legacy_issuer":"https://issuer.example.test","legacy_cutover_millis":1700000000000,'
    '"issuance":"legacy","legacy_access_kids":["access-1"],'
    '"legacy_attestation_kids":["attest-1"]}'
)


def cloud_env(**overrides: str) -> dict[str, str]:
    values = {
        "KORU_PUBLIC_API_ORIGIN": ORIGIN,
        "KORU_PUBLIC_API_ISSUER": ISSUER,
        "KORU_PUBLIC_API_GOOGLE_CLIENT_ID": CLIENT,
        "KORU_PUBLIC_API_CREDENTIAL_ROLLOUT": ROLLOUT,
    }
    values.update(overrides)
    return values


class ReleaseArgsTest(unittest.TestCase):
    def test_cloud_off_passes_no_account_settings(self):
        self.assertEqual(config.release_args("nightly", False, cloud_env()), [])

    def test_nightly_cloud_maps_the_existing_public_values(self):
        args = config.release_args("nightly", True, cloud_env())
        self.assertEqual(
            args,
            [
                f"-PkoruAccountOrigin={ORIGIN}",
                f"-PkoruAccountIssuer={ISSUER}",
                f"-PkoruGoogleServerClientId={CLIENT}",
                "-PkoruAccountChannel=nightly",
                f"-PkoruAccountCredentialRollout={ROLLOUT}",
            ],
        )

    def test_stable_channel_is_the_desktop_stable_segment(self):
        args = config.release_args("stable", True, cloud_env())
        self.assertIn("-PkoruAccountChannel=stable", args)

    def test_docs_desktop_client_is_not_the_account_client(self):
        env = cloud_env()
        del env["KORU_PUBLIC_API_GOOGLE_CLIENT_ID"]
        env["KORU_GOOGLE_DOCS_CLIENT_ID"] = "desktop-docs.apps.googleusercontent.com"
        with self.assertRaises(config.ConfigError) as raised:
            config.release_args("nightly", True, env)
        message = str(raised.exception)
        self.assertIn("KORU_PUBLIC_API_GOOGLE_CLIENT_ID", message)
        self.assertIn("KORU_GOOGLE_DOCS_CLIENT_ID", message)
        self.assertNotIn("desktop-docs.apps.googleusercontent.com", message)

    def test_missing_rollout_refuses_a_sign_in_without_cloud_sync(self):
        with self.assertRaises(config.ConfigError) as raised:
            config.release_args("nightly", True, cloud_env(KORU_PUBLIC_API_CREDENTIAL_ROLLOUT=""))
        self.assertIn("KORU_PUBLIC_API_CREDENTIAL_ROLLOUT", str(raised.exception))

    def test_loopback_and_partial_policy_are_refused(self):
        with self.assertRaises(config.ConfigError):
            config.release_args("nightly", True, cloud_env(KORU_PUBLIC_API_ORIGIN="http://localhost:8080"))
        with self.assertRaises(config.ConfigError):
            config.release_args(
                "nightly",
                True,
                cloud_env(KORU_PUBLIC_API_CREDENTIAL_ROLLOUT='{"region":"US"}'),
            )

    def test_issuer_must_match_the_policy_stage(self):
        with self.assertRaises(config.ConfigError) as raised:
            config.release_args(
                "nightly",
                True,
                cloud_env(KORU_PUBLIC_API_ISSUER="https://other.example.test"),
            )
        self.assertIn("Cloud sync", str(raised.exception))

    def test_legacy_zero_cutover_is_the_policy_the_phone_refuses(self):
        rollout = ROLLOUT.replace("1700000000000", "0")
        with self.assertRaises(config.ConfigError):
            config.release_args("nightly", True, cloud_env(KORU_PUBLIC_API_CREDENTIAL_ROLLOUT=rollout))

    def test_cli_cloud_off_is_silent_and_a_missing_value_is_named_not_printed(self):
        off_env = os.environ.copy()
        off_env.update(cloud_env())
        off = subprocess.run(
            [sys.executable, str(SCRIPT), "release-args", "--channel", "nightly", "--with-cloud", "false"],
            check=False,
            capture_output=True,
            text=True,
            env=off_env,
        )
        self.assertEqual(off.returncode, 0)
        self.assertEqual(off.stdout, "")
        self.assertNotIn(CLIENT, off.stderr)

        missing_env = os.environ.copy()
        missing_env["KORU_PUBLIC_API_ORIGIN"] = ORIGIN
        missing_env.pop("KORU_PUBLIC_API_ISSUER", None)
        missing = subprocess.run(
            [sys.executable, str(SCRIPT), "release-args", "--channel", "nightly", "--with-cloud", "true"],
            check=False,
            capture_output=True,
            text=True,
            env=missing_env,
        )
        self.assertEqual(missing.returncode, 1)
        self.assertIn("KORU_PUBLIC_API_ISSUER", missing.stderr)
        self.assertNotIn(ORIGIN, missing.stderr)


if __name__ == "__main__":
    unittest.main()
