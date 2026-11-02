#!/bin/sh
# SPDX-License-Identifier: MPL-2.0

# Koru iOS task runner for macOS.
set -u

SCRIPT_DIR=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
REPO_ROOT=$(CDPATH= cd -- "$SCRIPT_DIR/../.." && pwd)
PROJECT="$SCRIPT_DIR/KoruMobile.xcodeproj"
SCHEME="KoruMobile"
BUNDLE_ID="do.koru.app"
# **Koru Nightly** (Config/Nightly.xcconfig): `--nightly` on any build-like
# command builds the separate app do.koru.app.nightly, "Koru Nightly", with its
# own App Group, the way Android's -PkoruNightly=true does. Without it nothing
# changes: the project's own settings are the stable app's.
NIGHTLY_ID="do.koru.app.nightly"
NIGHTLY_GROUP="group.$NIGHTLY_ID"
NIGHTLY_XCCONFIG="$SCRIPT_DIR/Config/Nightly.xcconfig"
NIGHTLY=no
DERIVED_DATA="$SCRIPT_DIR/DerivedData"
LOCAL_CONFIG="$SCRIPT_DIR/koru.local.conf"

# Xcode's own configuration when one of its build phases runs this script;
# the line after it replaces CONFIGURATION with the runner's.
XCODE_CONFIGURATION=${CONFIGURATION:-}
CONFIGURATION=${KORU_CONFIGURATION:-Debug}
DEVICE=${KORU_DEVICE:-}
DEVICE_KIND=${KORU_DEVICE_KIND:-}
DEVELOPMENT_TEAM=${KORU_DEVELOPMENT_TEAM:-}

load_config() {
    [ -f "$LOCAL_CONFIG" ] || return 0
    while IFS='=' read -r key value; do
        case "$key" in
            configuration) [ -n "${KORU_CONFIGURATION:-}" ] || CONFIGURATION=$value ;;
            device) [ -n "${KORU_DEVICE:-}" ] || DEVICE=$value ;;
            device_kind) [ -n "${KORU_DEVICE_KIND:-}" ] || DEVICE_KIND=$value ;;
            development_team) [ -n "${KORU_DEVELOPMENT_TEAM:-}" ] || DEVELOPMENT_TEAM=$value ;;
        esac
    done < "$LOCAL_CONFIG"
}

fail() {
    echo "koru: $*" >&2
    exit 1
}

ensure_macos() {
    [ "$(uname -s)" = "Darwin" ] || fail "the iOS runner requires macOS and Xcode."
    command -v xcodebuild >/dev/null 2>&1 || fail "xcodebuild was not found. Install Xcode and select it with xcode-select."
    command -v xcrun >/dev/null 2>&1 || fail "xcrun was not found. Install the Xcode command-line tools."
}

# The Rust half. The app links libkoru_mobile_ios.a — the C-ABI shim over
# koru-mobile-host — the way the Android app links libkoru_mobile.so, and the
# Xcode build phase calls back into this script so the mapping from
# PLATFORM_NAME to a Rust triple is written once.
RUST_TRIPLE_DEVICE="aarch64-apple-ios"
RUST_TRIPLE_SIM="aarch64-apple-ios-sim"

ensure_rust() {
    # Xcode build phases run with a minimal PATH; cargo and sccache (which
    # the repository's .cargo/config.toml makes a hard requirement) live in
    # places that PATH does not reach.
    PATH="$HOME/.cargo/bin:/opt/homebrew/bin:/usr/local/bin:$PATH"
    command -v cargo >/dev/null 2>&1 || fail "cargo was not found. Install Rust from https://rustup.rs."
    command -v sccache >/dev/null 2>&1 || fail "sccache is not on PATH, and this repository builds through it. brew install sccache"
    [ "$(uname -m)" = "arm64" ] || fail "the iOS toolchain here targets Apple Silicon; an Intel Mac needs the x86_64 simulator triple nobody has wired. See docs/features/mobile/README.md."
}

native_build() {
    TRIPLE=$1
    rustup target list --installed 2>/dev/null | grep -qx "$TRIPLE" \
        || fail "the Rust target $TRIPLE is missing. Run: rustup target add $TRIPLE"
    FLAG=""
    [ "$CONFIGURATION" != "Release" ] || FLAG="--release"
    echo "[koru] cargo build -p koru-mobile-ios --target $TRIPLE ${FLAG:-(debug)}..."
    (cd "$REPO_ROOT" && cargo build -p koru-mobile-ios --target "$TRIPLE" $FLAG) || exit $?
}

# **The notices the app owes, and where its source is (MPL-2.0 §3.2)** — the
# iOS half of Android's koruNotices task. The bundle's licenses/ gets Koru's
# LICENSE and NOTICE, SOURCE.txt with https://github.com/koruworks/mono/tree/
# <commit>, and THIRD-PARTY-NOTICES.txt: the staticlib's crates and the Swift
# packages, from `koru-release notices ios`. Settings -> Open-source licenses
# shows them. A Debug build labels a crate without a recorded license instead
# of refusing it; a Release build refuses. koru-release is a host tool, so the
# iOS SDK the build phase exports must not reach its C dependencies.
notices_build() {
    DIR=$1
    BUILD_CONFIGURATION=$2
    COMMIT=$(git -C "$REPO_ROOT" rev-parse HEAD 2>/dev/null || true)
    printf '%s' "$COMMIT" | grep -Eqx '[0-9a-f]{40}' \
        || fail "the notices name the commit this app is built from, and git gave none (\"$COMMIT\")."
    FLAG=""
    [ "$BUILD_CONFIGURATION" = "Release" ] || FLAG="--debug"
    echo "[koru] koru-release notices ios ($BUILD_CONFIGURATION) -> $DIR"
    (cd "$REPO_ROOT" && env -u SDKROOT -u IPHONEOS_DEPLOYMENT_TARGET \
        cargo run --quiet --locked -p koru-release -- notices ios \
        --dir "$DIR" --source "https://github.com/koruworks/mono/tree/$COMMIT" $FLAG) || exit $?
}

generate_contracts() {
    command -v buf >/dev/null 2>&1 || fail "Buf was not found on PATH. Install it from https://buf.build/docs/cli/installation/."
    echo "[koru] generating Rust, Kotlin, and Swift contract bindings..."
    (cd "$REPO_ROOT" && buf generate contracts --template contracts/buf.gen.yaml) || exit $?
}

maybe_generate_contracts() {
    if [ "${1:-}" = "--contracts" ]; then
        generate_contracts
        return 0
    fi
    return 1
}

booted_simulators() {
    xcrun simctl list devices booted | sed -nE 's/.*\(([0-9A-Fa-f-]{36})\) \(Booted\).*/\1/p'
}

# The iPhone on the newest installed runtime. `simctl list` prints runtime
# sections in ascending order, so the last iPhone row wins — which also keeps a
# machine with iOS 17 runtimes still installed off them (the app's floor is 18;
# on 17 dyld refuses it over iroh's Network.framework import).
default_simulator() {
    xcrun simctl list devices available \
        | awk '/^-- iOS/{ios=1;next} /^--/{ios=0} ios && /iPhone .*\(/{print}' \
        | tail -n 1 \
        | sed -nE 's/.*\(([0-9A-Fa-f-]{36})\).*/\1/p'
}

# `run` boots a simulator by itself when nothing is booted and no device is
# configured — the same zero-setup start the Android runner has. Explicit
# KORU_DEVICE / koru.local.conf choices are honoured untouched.
ensure_booted_simulator() {
    [ -z "$DEVICE" ] || return 0
    COUNT=$(booted_simulators | sed '/^$/d' | wc -l | tr -d ' ')
    [ "$COUNT" = "0" ] || return 0
    TARGET=$(default_simulator)
    [ -n "$TARGET" ] || fail "no iPhone simulator is available. Install one with Xcode (Settings > Components)."
    echo "[koru] booting simulator $TARGET..."
    if ! xcrun simctl boot "$TARGET" 2>/dev/null; then
        [ "$(simulator_state "$TARGET")" = "Booted" ] || fail "could not boot simulator $TARGET."
    fi
    open -a Simulator
    xcrun simctl bootstatus "$TARGET" -b
}

# The app's own log stream, the logcat of this platform. Ctrl+C stops the
# stream and leaves the app running, exactly as logcat does.
stream_logs() {
    if [ "$DEVICE_KIND" = "simulator" ]; then
        echo "[koru] streaming Koru logs from $DEVICE. Press Ctrl+C to stop."
        xcrun simctl spawn "$DEVICE" log stream --style compact --level debug --predicate 'subsystem == "do.koru.app"'
    else
        echo "[koru] launching Koru with an attached console on $DEVICE. Press Ctrl+C to stop."
        xcrun devicectl device process launch --console --terminate-existing --device "$DEVICE" "$BUNDLE_ID"
    fi
}

simulator_state() {
    xcrun simctl list devices | grep -F "$1" | sed -nE 's/.*\((Booted|Shutdown|Creating|Shutting Down)\).*/\1/p' | head -n 1
}

resolve_destination() {
    REQUIRE_CONCRETE=${1:-yes}

    if [ -n "$DEVICE" ]; then
        case "$DEVICE_KIND" in
            simulator)
                STATE=$(simulator_state "$DEVICE")
                [ "$STATE" = "Booted" ] || fail "simulator $DEVICE is not booted. Run: ./koru.sh boot $DEVICE"
                ;;
            physical|device)
                DEVICE_KIND=physical
                ;;
            "")
                STATE=$(simulator_state "$DEVICE")
                if [ "$STATE" = "Booted" ]; then
                    DEVICE_KIND=simulator
                elif [ -n "$STATE" ]; then
                    fail "simulator $DEVICE is not booted. Run: ./koru.sh boot $DEVICE"
                else
                    DEVICE_KIND=physical
                fi
                ;;
            *) fail "device_kind must be simulator or physical, got '$DEVICE_KIND'." ;;
        esac
    else
        IDS=$(booted_simulators)
        COUNT=$(printf '%s\n' "$IDS" | sed '/^$/d' | wc -l | tr -d ' ')
        if [ "$COUNT" = "1" ]; then
            DEVICE=$(printf '%s\n' "$IDS" | sed -n '1p')
            DEVICE_KIND=simulator
        elif [ "$REQUIRE_CONCRETE" = "no" ]; then
            DEVICE_KIND=generic
        elif [ "$COUNT" = "0" ]; then
            fail "no booted simulator is available. Run ./koru.sh simulators and ./koru.sh boot <name-or-udid>, or set KORU_DEVICE for a physical device."
        else
            printf '%s\n' "$IDS" >&2
            fail "more than one simulator is booted. Set KORU_DEVICE to the desired UDID."
        fi
    fi

    case "$DEVICE_KIND" in
        simulator) DESTINATION="id=$DEVICE"; PRODUCT_DIR="${CONFIGURATION}-iphonesimulator" ;;
        physical)
            [ -n "$DEVELOPMENT_TEAM" ] || fail "physical deployment needs KORU_DEVELOPMENT_TEAM or development_team in koru.local.conf."
            DESTINATION="id=$DEVICE"
            PRODUCT_DIR="${CONFIGURATION}-iphoneos"
            ;;
        generic) DESTINATION="generic/platform=iOS Simulator"; PRODUCT_DIR="${CONFIGURATION}-iphonesimulator" ;;
    esac
    APP_PATH="$DERIVED_DATA/Build/Products/$PRODUCT_DIR/KoruMobile.app"
}

# The git branch and commit this app is built from, written to
# KoruMobile/GitStamp.json (git-ignored; the synchronized group bundles it as a
# resource) and advertised to linked devices beside the bundle version, so a
# mixed-build mesh names itself — the same two fields Android's Gradle writes
# into BuildConfig. A build made straight from Xcode has no file and the device
# then advertises neither. It is a file rather than an INFOPLIST_KEY_ setting
# because Xcode only honours that prefix for the keys it knows.
git_branch() {
    branch=$(git -C "$REPO_ROOT" rev-parse --abbrev-ref HEAD 2>/dev/null || true)
    [ "$branch" = "HEAD" ] && branch=""
    printf '%s' "$branch"
}
git_commit() { git -C "$REPO_ROOT" rev-parse --short=12 HEAD 2>/dev/null || true; }
# The version is the workspace's, read from the root Cargo.toml, so a phone
# and a desktop built from one tree agree on MAJOR.MINOR.PATCH — the three
# numbers the linked-device drift rule compares. MARKETING_VERSION on the
# command line overrides the project's copy (kept in step for Xcode GUI builds).
workspace_version() {
    sed -n '/^\[workspace\.package\]/,/^\[/p' "$REPO_ROOT/Cargo.toml" | sed -n 's/^version = "\(.*\)"/\1/p' | head -1
}
write_git_stamp() {
    printf '{"branch":"%s","commit":"%s"}\n' "$(git_branch)" "$(git_commit)" > "$SCRIPT_DIR/KoruMobile/GitStamp.json"
}

run_xcodebuild() {
    ACTION=$1
    shift
    [ "$NIGHTLY" = no ] || set -- -xcconfig "$NIGHTLY_XCCONFIG" "$@"
    echo "[koru] xcodebuild $ACTION ($DESTINATION, $CONFIGURATION)..."
    write_git_stamp
    if [ "$DEVICE_KIND" = "physical" ]; then
        xcodebuild \
            -project "$PROJECT" \
            -scheme "$SCHEME" \
            -configuration "$CONFIGURATION" \
            -destination "$DESTINATION" \
            -derivedDataPath "$DERIVED_DATA" \
            "MARKETING_VERSION=$(workspace_version)" \
            DEVELOPMENT_TEAM="$DEVELOPMENT_TEAM" \
            -allowProvisioningUpdates \
            -allowProvisioningDeviceRegistration \
            "$@" "$ACTION"
    else
        # Ad-hoc signed, not unsigned: an unsigned binary has no application
        # identifier, so the simulator's Keychain refuses it (-34018) and the
        # App Group container is not real — the app then silently runs on mock
        # services and device linking looks broken. "-" needs no team and no
        # certificate; it is Xcode's own "Sign to Run Locally".
        xcodebuild \
            -project "$PROJECT" \
            -scheme "$SCHEME" \
            -configuration "$CONFIGURATION" \
            -destination "$DESTINATION" \
            -derivedDataPath "$DERIVED_DATA" \
            "MARKETING_VERSION=$(workspace_version)" \
            CODE_SIGN_IDENTITY=- \
            "$@" "$ACTION"
    fi
}

build_app() {
    run_xcodebuild build "$@" || exit $?
    [ -d "$APP_PATH" ] || fail "built application was not found at $APP_PATH"
    echo "[koru] app: $APP_PATH"
}

install_app() {
    [ -d "$APP_PATH" ] || fail "no built app at $APP_PATH. Run ./koru.sh build first."
    if [ "$DEVICE_KIND" = "simulator" ]; then
        xcrun simctl install "$DEVICE" "$APP_PATH"
    else
        xcrun devicectl device install app --device "$DEVICE" "$APP_PATH"
    fi
}

launch_app() {
    echo "[koru] launching Koru on $DEVICE..."
    if [ "$DEVICE_KIND" = "simulator" ]; then
        xcrun simctl launch --terminate-running-process "$DEVICE" "$BUNDLE_ID"
    else
        xcrun devicectl device process launch --terminate-existing --device "$DEVICE" "$BUNDLE_ID"
    fi
}

# ------------------------------------------------------------ device aliases
# `device.<name>=<udid>` lines in koru.local.conf, written by `alias` and read
# by `--device=<name>`. Names are letters, digits, "_" and "-": they become a
# config key and a flag value, nothing else.

alias_of() {
    [ -f "$LOCAL_CONFIG" ] || return 1
    # The name goes into a sed expression, so only a name `alias` would have
    # accepted may go there. `--device=a/b` is not an alias; without this it is
    # a malformed `s///` and a sed error on the way to the right answer.
    valid_name "$1" || return 1
    sed -n "s/^device\.$1=//p" "$LOCAL_CONFIG" | head -n 1 | grep .
}

alias_names() {
    [ -f "$LOCAL_CONFIG" ] || return 0
    sed -n 's/^device\.\([A-Za-z0-9_-]*\)=.*/\1/p' "$LOCAL_CONFIG"
}

# The name a UDID was given, for the device lists.
name_of_udid() {
    [ -f "$LOCAL_CONFIG" ] || return 1
    grep "^device\.[A-Za-z0-9_-]*=$1\$" "$LOCAL_CONFIG" | head -n 1 | sed 's/^device\.\([^=]*\)=.*/\1/' | grep .
}

# Letters, digits, "_" and "-": an alias is a config key and a flag value.
valid_name() {
    printf '%s' "$1" | grep -Eq '^[A-Za-z0-9_-]+$'
}

valid_alias_name() {
    valid_name "$1" \
        || fail "\"$1\" is not a usable alias. Use a short word: phone, sim, ipad."
}

write_alias() {
    touch "$LOCAL_CONFIG"
    grep -v "^device\.$1=" "$LOCAL_CONFIG" > "$LOCAL_CONFIG.tmp" || true
    echo "device.$1=$2" >> "$LOCAL_CONFIG.tmp"
    mv "$LOCAL_CONFIG.tmp" "$LOCAL_CONFIG"
}

# Every device `--device` could mean, one per line: `<udid>|<kind>|<state>|<name>`.
# Simulators first (available runtimes only), then paired phones.
#
# Phones come from devicectl's JSON, not its table. The table's columns moved
# between Xcode 16 and 27 (the hostname went blank, the identifier became the
# hardware UDID with " (UDID)" after it, simulators joined the list) and a
# column parser written against one Xcode silently lists no phone under the
# next. The JSON names its fields. jq ships with macOS 15 and later, and any
# macOS that runs a devicectl worth parsing is newer than that.
#
# The row's id is CoreDevice's `identifier`, not the hardware UDID: it is what
# `alias` has always written to koru.local.conf, and devicectl accepts either.
all_devices() {
    xcrun simctl list devices available \
        | awk '/^-- iOS/{ios=1;next} /^--/{ios=0} ios' \
        | sed -nE 's/^ *(.*[^ ]) +\(([0-9A-Fa-f-]{36})\) \(([A-Za-z ]+)\).*/\2|simulator|\3|\1/p'
    JSON=$(mktemp -t koru-devices) || return 0
    if xcrun devicectl list devices --json-output "$JSON" >/dev/null 2>&1; then
        jq -r '.result.devices[]
            | select(.hardwareProperties.reality == "physical")
            | "\(.identifier)|physical|\(.connectionProperties.tunnelState // "unknown")|\(.deviceProperties.name) (\(.hardwareProperties.marketingName // .hardwareProperties.productType))"' \
            "$JSON" 2>/dev/null
    fi
    rm -f "$JSON"
}

# Resolve what `--device` named into DEVICE / DEVICE_KIND. An alias wins;
# then an exact UDID; then a case-insensitive part of a name. One match is the
# answer; several are asked about on a terminal and an error otherwise.
resolve_device_request() {
    WANTED=$1
    if ALIASED=$(alias_of "$WANTED"); then
        DEVICE=$ALIASED
    else
        DEVICE=$WANTED
    fi
    ROWS=$(all_devices)
    MATCHES=$(printf '%s\n' "$ROWS" | awk -F'|' -v w="$DEVICE" 'tolower($1)==tolower(w)')
    if [ -z "$MATCHES" ]; then
        MATCHES=$(printf '%s\n' "$ROWS" | awk -F'|' -v w="$(printf '%s' "$DEVICE" | tr '[:upper:]' '[:lower:]')" \
            'index(tolower($1 " " $4), w)')
    fi
    COUNT=$(printf '%s\n' "$MATCHES" | sed '/^$/d' | wc -l | tr -d ' ')
    if [ "$COUNT" = "0" ]; then
        echo "koru: no simulator or device matches \"$WANTED\"." >&2
        NAMES=$(alias_names | tr '\n' ' ')
        [ -z "$NAMES" ] || echo "Known aliases: $NAMES" >&2
        exit 1
    fi
    if [ "$COUNT" != "1" ]; then
        MATCHES=$(pick_device "$MATCHES" "\"$WANTED\" matches more than one device")
    fi
    DEVICE=$(printf '%s' "$MATCHES" | cut -d'|' -f1)
    DEVICE_KIND=$(printf '%s' "$MATCHES" | cut -d'|' -f2)
    STATE=$(printf '%s' "$MATCHES" | cut -d'|' -f3)
    # A simulator named by the person is booted for them: naming it is the
    # decision, and "is not booted, run boot" would be the script asking for
    # a second command to do the first one.
    if [ "$DEVICE_KIND" = "simulator" ] && [ "$STATE" != "Booted" ]; then
        echo "[koru] booting simulator $DEVICE..."
        xcrun simctl boot "$DEVICE" 2>/dev/null || true
        open -a Simulator
        xcrun simctl bootstatus "$DEVICE" -b
    fi
}

# Ask which of several rows was meant. Prints the chosen row.
pick_device() {
    ROWS=$1
    [ -t 0 ] || fail "$2; set KORU_DEVICE or use a UDID."
    echo "$2:" >&2
    N=0
    printf '%s\n' "$ROWS" | while IFS='|' read -r UDID KIND STATE NAME; do
        N=$((N + 1))
        echo "  $N. $NAME - $KIND, $STATE" >&2
        echo "     $UDID" >&2
    done
    printf 'Which one? ' >&2
    read -r CHOICE
    printf '%s\n' "$ROWS" | sed -n "${CHOICE}p" | grep . || fail "no device $CHOICE."
}

show_devices() {
    ROWS=$(all_devices)
    [ -n "$ROWS" ] || { echo "No simulators or paired devices."; return 0; }
    echo "Devices:"
    printf '%s\n' "$ROWS" | while IFS='|' read -r UDID KIND STATE NAME; do
        LABEL=$(name_of_udid "$UDID" 2>/dev/null || echo "(unnamed)")
        echo "  $LABEL - $NAME - $KIND, $STATE"
        echo "       $UDID"
    done
    echo
    echo "Run \"./koru.sh alias\" to name the unnamed ones, then \"--device=<name>\"."
}

# `alias`: name every unnamed connected device (booted simulators and paired
# phones), one question each. `alias <name> [device]`: name one.
name_devices() {
    [ -t 0 ] || fail "alias asks questions; run it in a terminal."
    ROWS=$(all_devices | awk -F'|' '$2=="physical" || $3=="Booted"')
    [ -n "$ROWS" ] || { echo "No booted simulator or paired device to name. Boot one, or plug one in."; return 0; }
    NAMED=0
    printf '%s\n' "$ROWS" > "$LOCAL_CONFIG.rows"
    while IFS='|' read -r UDID KIND STATE NAME; do
        if EXISTING=$(name_of_udid "$UDID"); then
            echo "  $NAME is \"$EXISTING\"."
            continue
        fi
        printf '  Name for %s (%s, %s)? [skip] ' "$NAME" "$KIND" "$UDID"
        read -r ANSWER < /dev/tty
        [ -n "$ANSWER" ] || continue
        valid_alias_name "$ANSWER"
        write_alias "$ANSWER" "$UDID"
        echo "  device.$ANSWER=$UDID saved."
        NAMED=$((NAMED + 1))
    done < "$LOCAL_CONFIG.rows"
    rm -f "$LOCAL_CONFIG.rows"
    echo "[koru] $NAMED named, in $LOCAL_CONFIG. Use one with: ./koru.sh run --device=<name>"
}

show_doctor() {
    echo "Koru iOS environment"
    echo
    echo "  Project:       $PROJECT"
    echo "  Scheme:        $SCHEME"
    echo "  Configuration: $CONFIGURATION"
    echo "  Device:        ${DEVICE:-auto-select one booted simulator}"
    echo "  Device kind:   ${DEVICE_KIND:-auto-detect}"
    echo "  Team:          ${DEVELOPMENT_TEAM:-not configured (simulator only)}"
    echo "  Derived data:  $DERIVED_DATA"
    echo
    xcodebuild -version
    echo "Developer directory: $(xcode-select -p)"
    echo
    echo "Rust (the app links the koru-mobile-ios staticlib):"
    PATH="$HOME/.cargo/bin:/opt/homebrew/bin:/usr/local/bin:$PATH"
    if command -v cargo >/dev/null 2>&1; then
        echo "  $(cargo --version)"
        for TRIPLE in "$RUST_TRIPLE_DEVICE" "$RUST_TRIPLE_SIM"; do
            if rustup target list --installed 2>/dev/null | grep -qx "$TRIPLE"; then
                echo "  target $TRIPLE: installed"
            else
                echo "  target $TRIPLE: MISSING — rustup target add $TRIPLE"
            fi
        done
    else
        echo "  cargo not found — install Rust from https://rustup.rs"
    fi
    echo "  host arch: $(uname -m)"
    echo
    echo "Booted simulators:"
    xcrun simctl list devices booted
    echo
    echo "Connected physical devices:"
    xcrun devicectl list devices
}

# ------------------------------------------------------- Koru Nightly, TestFlight

# **Koru Nightly to TestFlight** (TECHNICAL.final 5.3, as amended on
# 2026-11-01): what the nightly workflow's iOS job runs, and what a Mac runs to
# check it. The iPhone is never on the release page: TestFlight hands the build
# to testers and updates it.
#
# **The archive is built unsigned.** Signing it on a fresh CI machine would
# have Xcode mint a new Apple Development certificate every night, and the
# account's are capped. It is then signed ad hoc with each target's
# entitlements, the App Group expanded, because the export keeps only the
# entitlements a signature already carries: an unsigned archive would reach
# TestFlight without its App Group, and the share sheet would stage into
# nothing. **The export is Xcode's cloud signing**: the cloud-managed Apple
# Distribution certificate and App Store profiles Xcode creates or reuses for
# both bundle ids, through the App Store Connect API key when one is given and
# the Xcode account otherwise. Nothing is installed in a keychain.
#
# Every exported .ipa is checked before anything is uploaded: the bundle ids,
# the two versions, the display name, the App Group in both signatures, and an
# Apple Distribution signature.
testflight_fail_usage() {
    fail "usage: ./koru.sh testflight --version <release version> --build <yyyymmddHHMM> [--export | --upload] [--key <AuthKey.p8> --key-id <id> --issuer <uuid>]"
}

plist_value() {
    /usr/libexec/PlistBuddy -c "Print :$2" "$1/Info.plist" 2>/dev/null
}

# <bundle> <bundle id>: the identifiers, versions and name Koru Nightly carries.
check_nightly_bundle() {
    for pair in "CFBundleIdentifier=$2" "CFBundleShortVersionString=$TF_SHORT" \
        "CFBundleVersion=$TF_BUNDLE" "KoruAppGroup=$NIGHTLY_GROUP" \
        "CFBundleDisplayName=Koru Nightly"; do
        KEY=${pair%%=*}
        WANT=${pair#*=}
        GOT=$(plist_value "$1" "$KEY")
        [ "$GOT" = "$WANT" ] || fail "$(basename "$1"): $KEY is \"$GOT\", not \"$WANT\"."
    done
}

# <bundle>: its signature asks for Koru Nightly's App Group.
check_nightly_group() {
    codesign -d --entitlements - --xml "$1" 2>/dev/null | grep -qF "<string>$NIGHTLY_GROUP</string>" \
        || fail "$(basename "$1") is not signed with the App Group $NIGHTLY_GROUP."
}

# <entitlements file> <output>: the file with KORU_APP_GROUP expanded.
expand_entitlements() {
    sed "s/[\$](KORU_APP_GROUP)/$NIGHTLY_GROUP/g" "$1" > "$2" || exit $?
    ! grep -qF "\$(" "$2" || fail "$1 names a build setting the archive step does not expand."
    plutil -lint -s "$2" || fail "$2 is not a valid property list."
}

# <export | upload> <directory> [authentication arguments]
export_archive() {
    DEST=$1
    DIR=$2
    shift 2
    OPTIONS="$TF_OUT/ExportOptions-$DEST.plist"
    cp "$SCRIPT_DIR/ExportOptions.plist" "$OPTIONS"
    /usr/libexec/PlistBuddy -c "Set :destination $DEST" "$OPTIONS" || exit $?
    echo "[koru] xcodebuild -exportArchive ($DEST)..."
    xcodebuild -exportArchive \
        -archivePath "$TF_ARCHIVE" \
        -exportOptionsPlist "$OPTIONS" \
        -exportPath "$DIR" \
        -allowProvisioningUpdates \
        "$@" || exit $?
}

testflight() {
    TF_VERSION=""
    TF_BUILD=""
    TF_MODE=archive
    TF_KEY=""
    TF_KEY_ID=""
    TF_ISSUER=""
    while [ $# -gt 0 ]; do
        case "$1" in
            --export) TF_MODE="export"; shift; continue ;;
            --upload) TF_MODE="upload"; shift; continue ;;
            --version|--build|--key|--key-id|--issuer) [ $# -ge 2 ] || testflight_fail_usage ;;
            *) testflight_fail_usage ;;
        esac
        case "$1" in
            --version) TF_VERSION=$2 ;;
            --build) TF_BUILD=$2 ;;
            --key) TF_KEY=$2 ;;
            --key-id) TF_KEY_ID=$2 ;;
            --issuer) TF_ISSUER=$2 ;;
        esac
        shift 2
    done
    [ -n "$TF_VERSION" ] && [ -n "$TF_BUILD" ] || testflight_fail_usage
    TF_AUTH=no
    if [ -n "$TF_KEY$TF_KEY_ID$TF_ISSUER" ]; then
        [ -n "$TF_KEY" ] && [ -n "$TF_KEY_ID" ] && [ -n "$TF_ISSUER" ] \
            || fail "--key, --key-id and --issuer go together."
        [ -f "$TF_KEY" ] || fail "--key $TF_KEY is not a file."
        TF_AUTH=yes
    fi

    ensure_rust
    rustup target list --installed 2>/dev/null | grep -qx "$RUST_TRIPLE_DEVICE" \
        || fail "the Rust target $RUST_TRIPLE_DEVICE is missing. Run: rustup target add $RUST_TRIPLE_DEVICE"
    # CFBundleShortVersionString and CFBundleVersion, from the release version
    # and the build time: MAJOR.MINOR.PATCH, and the APK's versionCode.
    VERSIONS=$(cd "$REPO_ROOT" && env -u SDKROOT -u IPHONEOS_DEPLOYMENT_TARGET \
        cargo run --quiet --locked -p koru-release -- ios-version --version "$TF_VERSION" --build "$TF_BUILD") || exit $?
    VERSIONS=$(printf '%s\n' "$VERSIONS" | tail -n 1)
    printf '%s\n' "$VERSIONS" | grep -Eqx '[0-9]+\.[0-9]+\.[0-9]+ [0-9]+' \
        || fail "koru-release ios-version printed \"$VERSIONS\"."
    TF_SHORT=${VERSIONS% *}
    TF_BUNDLE=${VERSIONS#* }

    TF_OUT="$DERIVED_DATA/testflight"
    TF_ARCHIVE="$TF_OUT/KoruNightly.xcarchive"
    rm -rf "$TF_OUT"
    mkdir -p "$TF_OUT"
    write_git_stamp
    echo "[koru] archiving Koru Nightly $TF_SHORT ($TF_BUNDLE), unsigned..."
    # KORU_CONFIGURATION reaches the build phase that calls `native`, so the
    # Rust staticlib is the release build the Release configuration links.
    KORU_CONFIGURATION=Release xcodebuild archive \
        -project "$PROJECT" \
        -scheme "$SCHEME" \
        -configuration Release \
        -destination "generic/platform=iOS" \
        -archivePath "$TF_ARCHIVE" \
        -derivedDataPath "$DERIVED_DATA" \
        -xcconfig "$NIGHTLY_XCCONFIG" \
        "MARKETING_VERSION=$TF_SHORT" \
        "CURRENT_PROJECT_VERSION=$TF_BUNDLE" \
        CODE_SIGNING_ALLOWED=NO \
        CODE_SIGNING_REQUIRED=NO \
        "CODE_SIGN_IDENTITY=" || exit $?

    APP="$TF_ARCHIVE/Products/Applications/KoruMobile.app"
    APPEX="$APP/PlugIns/KoruShare.appex"
    [ -d "$APPEX" ] || fail "the archive has no $APPEX."
    check_nightly_bundle "$APP" "$NIGHTLY_ID"
    check_nightly_bundle "$APPEX" "$NIGHTLY_ID.share"

    # Inside out: anything embedded first, the extension, then the app.
    ENTITLEMENTS="$TF_OUT/entitlements"
    mkdir -p "$ENTITLEMENTS"
    expand_entitlements "$SCRIPT_DIR/KoruShare/KoruShare.entitlements" "$ENTITLEMENTS/KoruShare.plist"
    expand_entitlements "$SCRIPT_DIR/KoruMobile/KoruMobile.entitlements" "$ENTITLEMENTS/KoruMobile.plist"
    for NESTED in "$APP"/Frameworks/*; do
        [ -e "$NESTED" ] || continue
        codesign --force --sign - "$NESTED" || exit $?
    done
    codesign --force --sign - --entitlements "$ENTITLEMENTS/KoruShare.plist" "$APPEX" || exit $?
    codesign --force --sign - --entitlements "$ENTITLEMENTS/KoruMobile.plist" "$APP" || exit $?
    check_nightly_group "$APPEX"
    check_nightly_group "$APP"
    echo "[koru] archive: $TF_ARCHIVE"
    [ "$TF_MODE" != archive ] || return 0

    if [ "$TF_AUTH" = yes ]; then
        set -- -authenticationKeyPath "$TF_KEY" -authenticationKeyID "$TF_KEY_ID" -authenticationKeyIssuerID "$TF_ISSUER"
    else
        set --
    fi
    export_archive export "$TF_OUT/ipa" "$@"
    IPA="$TF_OUT/ipa/KoruMobile.ipa"
    [ -f "$IPA" ] || fail "the export wrote no $IPA."
    (cd "$TF_OUT" && unzip -q "$IPA" -d payload) || exit $?
    APP="$TF_OUT/payload/Payload/KoruMobile.app"
    APPEX="$APP/PlugIns/KoruShare.appex"
    for BUNDLE in "$APPEX" "$APP"; do
        codesign --verify --strict "$BUNDLE" || fail "$(basename "$BUNDLE") does not verify."
        codesign -d --verbose=2 "$BUNDLE" 2>&1 | grep -q '^Authority=Apple Distribution' \
            || fail "$(basename "$BUNDLE") is not signed for distribution."
        check_nightly_group "$BUNDLE"
    done
    check_nightly_bundle "$APP" "$NIGHTLY_ID"
    check_nightly_bundle "$APPEX" "$NIGHTLY_ID.share"
    echo "[koru] checked: $IPA"
    [ "$TF_MODE" = upload ] || return 0

    # The checked .ipa goes to App Store Connect. TestFlight refuses a
    # CFBundleVersion it already has, so a re-run of one nightly fails here.
    # With the API key it goes through altool: xcodebuild's own upload first
    # asks App Store Connect for the app's store configuration, and with an
    # API key Xcode 26.5 gets "Cannot verify client's JWT headers" there,
    # though the same key signs the export.
    if [ "$TF_AUTH" = yes ]; then
        KEYS="$TF_OUT/private_keys"
        mkdir -p "$KEYS" && cp "$TF_KEY" "$KEYS/AuthKey_$TF_KEY_ID.p8" || exit $?
        echo "[koru] xcrun altool --upload-app..."
        API_PRIVATE_KEYS_DIR="$KEYS" xcrun altool --upload-app --type ios --file "$IPA" \
            --apiKey "$TF_KEY_ID" --apiIssuer "$TF_ISSUER"
        STATUS=$?
        rm -rf "$KEYS"
        [ "$STATUS" -eq 0 ] || fail "altool could not upload $IPA."
    else
        export_archive upload "$TF_OUT/upload"
    fi
    echo "[koru] Koru Nightly $TF_SHORT ($TF_BUNDLE) is uploaded; TestFlight processes it, then gives it to the internal group."
}

show_help() {
    cat <<'EOF'
Koru iOS

usage: ./koru.sh <command> [arguments]

  run [--contracts] [xcode args]     boot a simulator if none is, then build,
                                     install, launch, and stream the app's logs
  build [--contracts] [xcode args]   build for the selected destination
  test [--contracts] [xcode args]    run the unit tests
  deploy [--contracts] [xcode args]  build and install without launching
  install [xcode args]               alias for deploy
  launch                             launch the installed application
  clean [xcode args]                 clean Xcode build products
  packages                           resolve Swift package dependencies
  native [--platform <name>]         build the Rust staticlib (both triples, or
                                     the one PLATFORM_NAME names; the Xcode
                                     build phase calls this)
  notices [--dir <directory>]        write the open-source notices into the app
                                     bundle being built (the Xcode build phase
                                     calls this), or into <directory>
  contracts                          regenerate Rust, Kotlin, and Swift bindings
  doctor | env                       show Xcode, signing, and device state
  destinations                       list Xcode destinations for the scheme
  simulators                         list available simulator devices
  boot <name-or-udid>                boot a simulator and open Simulator.app
  shutdown [name-or-udid]            shut down a simulator
  devices                            list simulators and phones, with their names
  alias                              ask for a name for each unnamed device
  alias <name> [device]              name one device so --device=<name> finds it
  logs                               stream Koru logs; physical mode launches attached
  setup [team] [device] [kind]       save local signing/device settings
  sim                                forget the configured device; back to simulators
  xcode <args...>                    pass arguments directly to xcodebuild
  testflight --version <v> --build <yyyymmddHHMM> [--export | --upload]
             [--key <AuthKey.p8> --key-id <id> --issuer <uuid>]
                                     archive Koru Nightly; with --export also
                                     sign it for the App Store and check the
                                     .ipa; with --upload also send it to
                                     TestFlight (the nightly workflow runs this)
  open                               open the project in Xcode
  help                               show this message

Environment overrides:
  KORU_DEVICE=<udid>
  KORU_DEVICE_KIND=simulator|physical
  KORU_DEVELOPMENT_TEAM=<team-id>
  KORU_CONFIGURATION=Debug|Release

Any build-like command takes --nightly: Koru Nightly (do.koru.app.nightly),
a separate app beside Koru with its own App Group.
  ./koru.sh run --nightly --device=phone

Any command takes --device=<x> (or -d <x>): an alias, a UDID, or any part
of a simulator's or phone's name. A named simulator is booted for you.
  ./koru.sh alias                    name every device, once
  ./koru.sh run --device=phone
  ./koru.sh logs -d sim
  ./koru.sh deploy --device="iPhone 17 Pro"

Examples:
  ./koru.sh simulators
  ./koru.sh boot "iPhone 17"         the iPhone 17 on iOS 26.5 the tests run on
  ./koru.sh run
  ./koru.sh setup ABCDE12345 00008110-001234560123401E physical
  ./koru.sh deploy
  ./koru.sh logs

`--contracts` must be the first argument after build, test, deploy, or run.
Ordinary builds do not require Buf.
EOF
}

load_config

# `--device=<x>`, `--device <x>` or `-d <x>` anywhere after the command names
# the target, the way the Android runner takes it: an alias from
# koru.local.conf (`device.<name>=<udid>`), a UDID, or any part of a
# simulator's or phone's name. Taken out here so every verb sees only its own
# arguments; resolved after the config is read, because an alias wins outright.
DEVICE_WANTED=""
COMMAND=${1:-run}
[ $# -eq 0 ] || shift
# The keepers are *rotated* rather than collected into a string and `eval`-ed:
# a device name is a person's own text, and an `eval` expands a `$` in it --
# `boot "iPhone $HOME"` reaches xcrun as a path. Each argument comes off the
# front and every keeper goes on the back, so quoting survives untouched.
LEFT=$#
while [ "$LEFT" -gt 0 ]; do
    ARG=$1
    shift
    LEFT=$((LEFT - 1))
    case "$ARG" in
        --device|-d)
            [ "$LEFT" -ge 1 ] || fail "--device needs an alias, a UDID, or part of a device name."
            DEVICE_WANTED=$1
            shift
            LEFT=$((LEFT - 1))
            ;;
        --device=*) DEVICE_WANTED=${ARG#--device=} ;;
        --nightly) NIGHTLY=yes ;;
        *) set -- "$@" "$ARG" ;;
    esac
done
[ -z "$DEVICE_WANTED" ] || resolve_device_request "$DEVICE_WANTED"
[ "$NIGHTLY" = no ] || BUNDLE_ID=$NIGHTLY_ID

case "$COMMAND" in
    help|-h|--help) show_help; exit 0 ;;
    contracts) generate_contracts; exit $? ;;
esac

ensure_macos

case "$COMMAND" in
    doctor|env) show_doctor ;;
    destinations)
        xcodebuild -project "$PROJECT" -scheme "$SCHEME" -showdestinations
        ;;
    simulators)
        xcrun simctl list devices available
        ;;
    boot)
        [ $# -gt 0 ] || fail "boot needs a simulator name or UDID."
        TARGET=$1
        if ! xcrun simctl boot "$TARGET" 2>/dev/null; then
            STATE=$(simulator_state "$TARGET")
            [ "$STATE" = "Booted" ] || fail "could not boot simulator '$TARGET'."
        fi
        open -a Simulator
        xcrun simctl bootstatus "$TARGET" -b
        ;;
    shutdown)
        if [ $# -gt 0 ]; then TARGET=$1; else resolve_destination yes; TARGET=$DEVICE; fi
        [ "${DEVICE_KIND:-simulator}" != "physical" ] || fail "shutdown only applies to simulators."
        xcrun simctl shutdown "$TARGET"
        ;;
    devices)
        show_devices
        ;;
    alias)
        if [ $# -eq 0 ]; then
            name_devices
        else
            valid_alias_name "$1"
            if [ $# -ge 2 ]; then
                resolve_device_request "$2"
            else
                ROWS=$(all_devices | awk -F'|' '$2=="physical" || $3=="Booted"')
                [ -n "$ROWS" ] || fail "no booted simulator or paired device to name."
                PICK=$(pick_device "$ROWS" "Which device is \"$1\"")
                DEVICE=$(printf '%s' "$PICK" | cut -d'|' -f1)
            fi
            write_alias "$1" "$DEVICE"
            echo "[koru] device.$1=$DEVICE saved in $LOCAL_CONFIG"
            echo "[koru] use it with: ./koru.sh run --device=$1"
        fi
        ;;
    sim)
        # Forget the configured physical device and go back to simulators;
        # the team survives, so switching to the phone again is just
        # `setup <team> <udid> physical` — or nothing, if the team is saved:
        # `setup "" <udid> physical` keeps it. `run` boots a simulator by
        # itself when none is booted.
        if [ -f "$LOCAL_CONFIG" ]; then
            grep -v '^device=' "$LOCAL_CONFIG" | grep -v '^device_kind=' > "$LOCAL_CONFIG.tmp" || true
            mv "$LOCAL_CONFIG.tmp" "$LOCAL_CONFIG"
        fi
        echo "[koru] device setting cleared; run targets a simulator again (booting one if needed)."
        ;;
    setup)
        [ $# -lt 1 ] || DEVELOPMENT_TEAM=$1
        [ $# -lt 2 ] || DEVICE=$2
        [ $# -lt 3 ] || DEVICE_KIND=$3
        case "$DEVICE_KIND" in ""|simulator|physical|device) ;; *) fail "kind must be simulator or physical." ;; esac
        {
            echo "configuration=$CONFIGURATION"
            [ -z "$DEVELOPMENT_TEAM" ] || echo "development_team=$DEVELOPMENT_TEAM"
            [ -z "$DEVICE" ] || echo "device=$DEVICE"
            [ -z "$DEVICE_KIND" ] || echo "device_kind=$DEVICE_KIND"
        } > "$LOCAL_CONFIG"
        echo "[koru] wrote $LOCAL_CONFIG"
        echo "[koru] resolving Swift package dependencies..."
        xcodebuild -resolvePackageDependencies -project "$PROJECT" -scheme "$SCHEME"
        ;;
    packages)
        xcodebuild -resolvePackageDependencies -project "$PROJECT" -scheme "$SCHEME"
        ;;
    notices)
        DIR=""
        if [ "${1:-}" = "--dir" ]; then
            DIR=${2:-}
            [ -n "$DIR" ] || fail "--dir needs a directory."
            shift 2
        fi
        BUILD_CONFIGURATION=$CONFIGURATION
        if [ -z "$DIR" ]; then
            [ -n "${TARGET_BUILD_DIR:-}" ] && [ -n "${UNLOCALIZED_RESOURCES_FOLDER_PATH:-}" ] \
                || fail "notices writes into the bundle Xcode is building; outside a build phase, pass --dir <directory>."
            DIR="$TARGET_BUILD_DIR/$UNLOCALIZED_RESOURCES_FOLDER_PATH/licenses"
            BUILD_CONFIGURATION=${XCODE_CONFIGURATION:-$CONFIGURATION}
        fi
        ensure_rust
        notices_build "$DIR" "$BUILD_CONFIGURATION"
        ;;
    native)
        PLATFORM=""
        if [ "${1:-}" = "--platform" ]; then
            PLATFORM=${2:-}
            shift 2 2>/dev/null || fail "--platform needs a value (iphoneos or iphonesimulator)."
        fi
        ensure_rust
        case "$PLATFORM" in
            iphoneos) native_build "$RUST_TRIPLE_DEVICE" ;;
            iphonesimulator) native_build "$RUST_TRIPLE_SIM" ;;
            "") native_build "$RUST_TRIPLE_SIM"; native_build "$RUST_TRIPLE_DEVICE" ;;
            *) fail "unknown platform '$PLATFORM' (iphoneos or iphonesimulator)." ;;
        esac
        ;;
    build)
        if maybe_generate_contracts "${1:-}"; then shift; fi
        resolve_destination no
        build_app "$@"
        ;;
    test)
        if maybe_generate_contracts "${1:-}"; then shift; fi
        resolve_destination yes
        run_xcodebuild test "$@"
        ;;
    deploy|install)
        if maybe_generate_contracts "${1:-}"; then shift; fi
        resolve_destination yes
        build_app "$@"
        install_app
        ;;
    run)
        if maybe_generate_contracts "${1:-}"; then shift; fi
        ensure_booted_simulator
        resolve_destination yes
        build_app "$@"
        install_app
        launch_app
        # On a simulator the launch is detached, so the stream is what keeps
        # this terminal useful; on a physical device the attached launch above
        # in stream_logs *is* the run, so skip the duplicate plain launch there.
        if [ "$DEVICE_KIND" = "simulator" ]; then
            stream_logs
        fi
        ;;
    launch)
        resolve_destination yes
        launch_app
        ;;
    logs)
        resolve_destination yes
        stream_logs
        ;;
    clean)
        resolve_destination no
        run_xcodebuild clean "$@"
        ;;
    xcode)
        xcodebuild -project "$PROJECT" -scheme "$SCHEME" "$@"
        ;;
    testflight)
        testflight "$@"
        ;;
    open)
        open "$PROJECT"
        ;;
    *)
        echo "koru: unknown command '$COMMAND'" >&2
        echo >&2
        show_help >&2
        exit 2
        ;;
esac
