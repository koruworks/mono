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
