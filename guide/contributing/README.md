# Contributor guides

Start with [CONTRIBUTING.md](../../CONTRIBUTING.md) for contribution policy,
sign-off, and the checks to run before a pull request. Read
[AGENTS.md](../../AGENTS.md) for the repository's orientation and invariants.

## Building and running

The root [README](../../README.md#build-and-run-from-a-fresh-clone) describes prerequisites,
setup, and launching a development build. Rust is pinned in
[`rust-toolchain.toml`](../../rust-toolchain.toml); the workspace's minimum Rust
version is declared in [`Cargo.toml`](../../Cargo.toml).

On Windows, `koru.bat root` builds and starts an isolated development root at
`.dev-data\root-mode`. Rebuilding stops this checkout's debug windows and services
across all roots so Windows can replace the shared executables. To run a second
root alongside it, use `koru.bat root-open .dev-data\root-b`: this uses the built
binaries and leaves other roots running. An explicit path may be supplied to
either command. `koru.bat root-stop <path>` stops only that root's services.

`koru.bat font-test` launches the standalone font comparison editor, using the
monorepo's fonts and rendering dependencies. Its [controls and implementation](../../apps/koru-font-test/README.md)
include exact pixel magnification and live rendering experiments.

Setup's `--agent-skills` option installs coding agent skills.
Windows `--defender-exclusions` enables the documented
antivirus exclusions for a trusted development checkout. Neither is needed to
read these guides or contribute a change.

For platform-specific work, see the [Android README](../../apps/android/README.md)
and [iOS README](../../apps/ios/README.md). For an unsigned local package, use
`koru.bat dist-local beta` or `./koru.sh dist-local beta`; see the
[release notes guide](../../releases/README.md) and
[release build requirements](../../CONTRIBUTING.md#release-builds).

## Testing and implementation

Run the workspace suite from the repository root.

```sh
cargo test --workspace --locked
cargo fmt --all --check
python -m unittest discover -s scripts/checks -p "test_*.py"
python scripts/checks/oss-links.py .
```

Android tests are not part of CI yet; their local commands remain in the Android
README. A passing automated suite does not replace verifying the changed user
interaction on its supported platforms.

| Topic | Read |
| --- | --- |
| Test placement, isolation, process state, and native verification | [Testing](TESTING.md) |
| Design and implementation principles | [Working principles](PRINCIPLES.md) |
| Library behavior and platform traps | [Library primers](deps/README.md) |
| Switching, regenerating and extending desktop icon packs | [Compiled icons](ICONS.md) |
| Rust concepts through examples from Koru | [Rust guide](RUST.md) |
| Adding a block kind across the model, editor, and MCP | [Block checklist](BLOCKS.md) |
| Choosing which surfaces a build shows | [Product feature flags](FEATURE-FLAGS.md) |
| Public API and wire schemas | [Generated references](../../reference/README.md) |
| Environment variables for diagnostics and configuration | [Environment reference](../../reference/ENV.md) |

### Security advisories in dependencies

CI runs `cargo-deny` on every push and pull request, and a separate workflow
(`.github/workflows/advisories.yml`) checks `main` against the RustSec advisory
database once a day, so an advisory is noticed the day it is published. Either
one going red on advisories means a locked crate has a known problem.

Dependabot updates the GitHub Actions pins and the Android build, but **not
Rust**: it cannot read the patched crates this workspace fetches instead of
committing ([third-party/](../../third-party/README.md)). So the fix is made by
hand: change that one crate's `version` and `checksum` in `Cargo.lock` to the
fixed release, then confirm with `cargo metadata --locked` and
`cargo deny check`. Prefer that to `cargo update -p <crate>`, which also
re-resolves unrelated entries the lockfile holds on purpose. An advisory that
does not apply gets an `ignore` entry in `deny.toml` with the reason.

Some primers retain ADR and plan identifiers as provenance for past experiments.
Maintainers keep those original records in the private documentation repository;
the implementation guidance is included in the public pages.

### Diagnosing unavailable mini-app tools

The Store's rotating `logs/koru-api-gateway.log` records
`ai.mini_apps.bridge_probe` and `ai.mini_apps.startup`: whether the desktop MCP
bridge is installed beside the worker executable, whether a session host was
supplied, and whether managed mini-app tools passed that startup gate.
`logs/koru-session-host.log` records `chat.bridge.ready` or
`chat.bridge.missing` for the bridge launch the session host configures.
These checks are cached at process startup; restart the gateway and session
host after installing or rebuilding a missing sibling bridge.

Enable `RUST_LOG=warn,koru=info,koru_ai_server=debug,koru_ai_provider_agent=info,koru_session_host=info`
before starting the processes for per-attempt `ai.mini_apps.tools` decisions.
They report provider registration, host admission or its categorical rejection
reason, configured and admitted tool counts, and refinement availability.
`ai.mini_apps.withheld` warns when a requested host is rejected;
`ai.mini_apps.provider_unavailable` warns when the provider later reports that
native tools are unavailable. Correlate them by attempt ID. These diagnostics
contain no prompts, tool payloads, capability tokens, or filesystem paths.

## Documentation changes

Public instructions live in [guide/](../README.md). Update the
[user guide](../user/USERGUIDE.md) when a gesture or visible behavior changes,
and the relevant contributor page when an implementation rule changes. Internal
plans and hosted-service operations remain in the private documentation
repository; a public contribution should explain any corresponding internal
update in its pull request.
