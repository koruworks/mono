# Koru iOS

This is the native Swift and SwiftUI application. It currently uses in-memory
mock services, so demonstration data resets when the process ends.

## Requirements

- macOS with Xcode 26.5 or newer selected by `xcode-select`. Xcode 26.5 is the
  first release whose simulator runs the iOS 26.5 runtime the tests need, and
  it needs macOS Tahoe 26.2 or newer.
- An iOS 18 or newer simulator runtime or physical device. The deployment
  target is iOS 18.0 because iroh's `netdev` strong-imports an iOS 18
  Network.framework symbol, and on iOS 17 dyld refuses to launch the app. The
  tests run on the iOS 26.5 runtime (an iPhone 17 simulator).
- An Apple development team and device provisioning for physical deployment.

The project resolves SwiftProtobuf through Swift Package Manager and compiles
the committed generated sources from `../../contracts/gen/swift`.

## Run in a simulator

Use `koru.sh` as the main entry point from this directory:

```sh
cd apps/ios
chmod +x koru.sh                 # needed once if the executable bit was lost

./koru.sh doctor
./koru.sh simulators
./koru.sh boot "iPhone 17"
./koru.sh run
```

`run` builds, installs, and launches Koru. It automatically selects the one
booted simulator. If several simulators are booted, select one explicitly:

```sh
export KORU_DEVICE=<simulator-udid>   # from `xcrun simctl list devices booted`
export KORU_DEVICE_KIND=simulator
./koru.sh run
```

Use `./koru.sh devices` to see simulator and physical-device identifiers.

## Deploy to a physical device

First sign into your Apple account in Xcode, enable Developer Mode on the
device, connect or pair it, and use `./koru.sh devices` to find its identifier.
Save the Apple development team, device identifier, and destination kind for
this checkout:

```sh
./koru.sh setup ABCDE12345 00008110-001234560123401E physical
./koru.sh run
```

`setup` also resolves Swift package dependencies. It writes the ignored
`koru.local.conf` file beside the script. Environment variables override that
file when you need a temporary selection:

```sh
export KORU_DEVELOPMENT_TEAM=ABCDE12345
export KORU_DEVICE=00008110-001234560123401E
export KORU_DEVICE_KIND=physical
./koru.sh deploy              # build and install without launching
./koru.sh launch              # launch an already installed build
```

Automatic signing and provisioning updates are enabled for physical builds.
Xcode may still ask you to accept a license, sign into the selected team, trust
the Mac, or resolve a bundle-ID/provisioning conflict.

## Commands

| Command | Purpose |
| --- | --- |
| `./koru.sh run` | Build, install, and launch on the selected destination. |
| `./koru.sh build` | Build for a selected destination or a generic simulator. |
| `./koru.sh test` | Run unit tests on the selected simulator/device. |
| `./koru.sh deploy` | Build and install without launching. |
| `./koru.sh launch` | Launch an already installed app. |
| `./koru.sh doctor` | Show Xcode, signing, and connected-device state. |
| `./koru.sh destinations` | List Xcode destinations for the shared scheme. |
| `./koru.sh simulators` | List available simulator devices. |
| `./koru.sh boot <name-or-udid>` | Boot a simulator and open Simulator.app. |
| `./koru.sh shutdown [name-or-udid]` | Shut down a simulator. |
| `./koru.sh devices` | List simulators and physical devices. |
| `./koru.sh logs` | Stream Koru's OSLog messages. |
| `./koru.sh packages` | Resolve Swift package dependencies. |
| `./koru.sh contracts` | Regenerate Rust, Kotlin, and Swift bindings. |
| `./koru.sh clean` | Clean Xcode build products. |
| `./koru.sh open` | Open the project in Xcode. |
| `./koru.sh xcode <args...>` | Pass arguments directly to `xcodebuild`. |

Run `./koru.sh help` for environment variables and the complete command list.
Extra arguments after build-like commands are forwarded to `xcodebuild`:

```sh
./koru.sh build -quiet
./koru.sh test -only-testing:KoruMobileTests/ContractFixtureTests
```

Add `--contracts` as the first argument to generate contract bindings before a
build-like command. Ordinary builds do not require Buf:

```sh
./koru.sh build --contracts
./koru.sh run --contracts
```

## Koru Nightly and TestFlight

Koru Nightly is a separate app, `do.koru.app.nightly`, that installs beside
Koru with its own share extension (`do.koru.app.nightly.share`) and its own
App Group (`group.do.koru.app.nightly`), so it never touches the stable app's
data. It is the iOS twin of Android's `-PkoruNightly=true`. The switch is
[`Config/Nightly.xcconfig`](Config/Nightly.xcconfig), which overrides the
project's `KORU_APP_ID` and `KORU_APP_DISPLAY_NAME`; the bundle ids, the
display name, both entitlements files and the `KoruAppGroup` key the code
reads all follow from those two settings. Without it the build is Koru, as
before. To run the nightly variant yourself:

```sh
./koru.sh run --nightly --device=phone
```

**Every nightly reaches TestFlight, not the release page.** When the
`nightly` tag moves, the nightly workflow's iOS job runs
`./koru.sh testflight --version <v> --build <n> --upload`: it archives Koru
Nightly unsigned, signs it ad hoc with the expanded App Group entitlements,
exports it with Xcode's cloud-managed Apple Distribution certificate through
the App Store Connect API key (`APPSTORE_API_KEY_P8`, `APPSTORE_API_KEY_ID`,
`APPSTORE_API_ISSUER_ID` in the `nightly` environment, the key that notarizes
the Mac build), checks the `.ipa`, and uploads it. Without the key the job
only archives, to show the tree builds. A failed iOS build never holds back
the other platforms. The version is the release's `MAJOR.MINOR.PATCH`, and
the build number is the minutes since 2026-01-01 UTC, the Android
`versionCode` of the same nightly (`koru-release ios-version`). TestFlight
updates installed builds itself, so the iOS app has no update notice.

To check the same build on a Mac without uploading it (signed through your
Xcode account, or the API key with `--key`, `--key-id` and `--issuer`):

```sh
rustup target add aarch64-apple-ios
./koru.sh testflight --version 0.1.5-nightly.20261101+abc1234 --build 202611010900 --export
```

**Adding testers.** In App Store Connect, open *Koru Nightly*, then
*TestFlight*. Internal testers are members of the App Store Connect team
(*Users and Access*); add them to the internal group, with automatic
distribution on, and each new build reaches them once Apple has processed it,
usually within minutes. They install the TestFlight app and accept the
invitation. External testers, and the public link, need a group of their own
and Beta App Review for the first build of each version. Each build asks for
its export compliance answer unless the app declares it in its Info.plist.

If cloud signing is ever refused on CI (the API key needs the Admin role, or
*Access to Cloud Managed Distribution Certificate*), the fallback is manual
signing: an Apple Distribution certificate as a `.p12` and App Store
provisioning profiles for both nightly bundle ids, stored in the `nightly`
environment and installed into a temporary keychain the way the macOS job
installs its Developer ID certificate, with `signingStyle` `manual` in
[`ExportOptions.plist`](ExportOptions.plist). That path is not wired today:
it would be the job's own change, with three new secrets.

## Logs

The simulator log command filters the unified log to subsystem `do.koru.app`:

```sh
./koru.sh logs
```

On a physical device, the command launches Koru with `devicectl --console` and
attaches to its output. Press `Ctrl+C` to stop either stream. Logs contain event
names, identifiers, and timings—not document contents.

## Use Xcode directly

Open `KoruMobile.xcodeproj`, choose the `KoruMobile` scheme and destination,
then press Run. The equivalent test command is:

```sh
xcodebuild test -project KoruMobile.xcodeproj -scheme KoruMobile \
  -destination 'platform=iOS Simulator,name=iPhone 17,OS=26.5'
```

Do not hand-edit generated contract sources. Regenerate them with
`./koru.sh contracts` and commit `contracts/gen/`.
