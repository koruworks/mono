// SPDX-License-Identifier: MPL-2.0

import CoreImage.CIFilterBuiltins
import SwiftUI
import VisionKit

/// Link this device, and the devices it is linked to — the iOS mirror of
/// Android's `LinkDeviceScreen` + `LinkedDevicesList`. Three ways in, same as
/// there: scan the desktop's QR, type the code, or show a QR of our own.
struct DevicesScreen: View {
    @EnvironmentObject private var model: AppModel
    /// The device-local relay preference (iroh-relay roadmap, decision №6):
    /// UserDefaults owns it — the same key `KoruNative` reads at host open —
    /// and the toggle also applies it to the live host, so no restart.
    @AppStorage(KoruNative.relayEnabledKey) private var relayEnabled = true
    @State private var code = ""
    @State private var offer: PairingOffer?
    @State private var scanning = false
    @State private var renaming = false
    @State private var confirmingReset = false
    @State private var selectedDevice: LinkedDevice?
    @State private var newName = ""

    var body: some View {
        List {
            if let mine = model.thisDevice {
                Section("this_device") {
                    HStack {
                        VStack(alignment: .leading, spacing: KoruSpace.xSmall) {
                            Text(mine.displayName).font(KoruType.body)
                            Text("this_device_hint").font(.caption).foregroundStyle(KoruColor.subtle)
                            if let build = mine.buildLabel {
                                Text(build).font(.caption).foregroundStyle(KoruColor.subtle)
                            }
                        }
                        Spacer()
                        Button("rename") { newName = mine.displayName; renaming = true }
                            .buttonStyle(.borderless)
                            .font(.caption)
                    }
                }
            }
            Section {
                Toggle("use_relay", isOn: $relayEnabled)
                    .onChange(of: relayEnabled) { _, enabled in
                        Task { await model.setRelayEnabled(enabled) }
                    }
                Text("use_relay_explainer").font(.caption).foregroundStyle(KoruColor.subtle)
                // The connection as the host reads it now, refreshed by the
                // same 1 s devices poll as the list below. Absent (an old
                // host, or none yet) shows nothing rather than a guess.
                if let status = model.relayStatus {
                    Text(relayStatusLine(status)).font(.caption).foregroundStyle(KoruColor.subtle)
                }
            }
            Section("link_title") {
                stateRow
                Text("link_explainer").font(.caption).foregroundStyle(KoruColor.subtle)
                if DataScannerViewController.isSupported {
                    Button("scan_qr", systemImage: "qrcode.viewfinder") { scanning = true }
                }
                TextField("enter_link_code", text: $code)
                    .textInputAutocapitalization(.never)
                    .autocorrectionDisabled()
                    .font(.system(.body, design: .monospaced))
                Button("link_device") {
                    let entered = code
                    Task { await model.link(code: entered) }
                }
                .disabled(code.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty || isLinking)
            }
            Section("pairing_offer") {
                if let offer {
                    PairingOfferView(offer: offer) {
                        Task { await model.cancelPairing(); self.offer = nil }
                    }
                } else {
                    Button("start_pairing", systemImage: "qrcode") {
                        Task { offer = await model.startPairing() }
                    }
                }
            }
            Section("linked_devices") {
                if model.linkedDevices.isEmpty {
                    Text("no_linked_devices").font(.caption).foregroundStyle(KoruColor.subtle)
                } else {
                    ForEach(model.linkedDevices) { device in
                        // The row is the way into the device's history — as a
                        // sheet, the way Android opens it. Not a NavigationLink:
                        // the root stack binds a typed `[Route]` path, and a
                        // link whose value is not a Route is drawn disabled.
                        // The Unlink button inside stays its own tap target.
                        LinkedDeviceRow(device: device) {
                            Task { await model.unlink(deviceID: device.deviceID) }
                        }
                        .contentShape(Rectangle())
                        .onTapGesture { selectedDevice = device }
                    }
                }
            }
        }
        .navigationTitle("devices")
        .sheet(item: $selectedDevice) { device in
            NavigationStack { DeviceHistoryScreen(device: device) }
        }
        .confirmationDialog("start_over_as_new_device", isPresented: $confirmingReset, titleVisibility: .visible) {
            Button("start_over_confirm", role: .destructive) {
                Task { await model.resetDeviceIdentity() }
            }
            Button("cancel", role: .cancel) {}
        } message: {
            Text("start_over_explainer")
        }
        .alert("rename_device", isPresented: $renaming) {
            TextField("name_or_title", text: $newName)
            Button("cancel", role: .cancel) { renaming = false }
            Button("rename") {
                let name = newName
                Task { await model.renameThisDevice(name) }
                renaming = false
            }
        }
        .sheet(isPresented: $scanning) {
            QRScanSheet { payload in
                scanning = false
                code = payload
                Task { await model.link(code: payload) }
            }
        }
    }

    private var isLinking: Bool {
        if case .linking = model.linkState { return true }
        return false
    }

    /// The relay status row's text, the desktop tray's rendering word for
    /// word: `<host> — <state>`, the host never translated, the state word
    /// from the same translation table and the separator living here. Off
    /// carries no host worth naming — the toggle above already says which
    /// switch — so it is the localized whole-row text alone.
    private func relayStatusLine(_ status: RelayStatus) -> String {
        switch status.state {
        case .off: String(localized: "relay_state_off")
        case .connecting: "\(status.host) — " + String(localized: "relay_state_connecting")
        case .connected: "\(status.host) — " + String(localized: "relay_state_connected")
        case .disconnected: "\(status.host) — " + String(localized: "relay_state_disconnected")
        }
    }

    @ViewBuilder private var stateRow: some View {
        switch model.linkState {
        case .unlinked: Label("state_unlinked", systemImage: "link.badge.plus")
        case .linking: HStack(spacing: KoruSpace.small) { ProgressView(); Text("linking_device") }
        case let .linked(peer): Label(String(format: String(localized: "state_linked"), locale: .current, peer ?? "-"), systemImage: "checkmark.seal")
        case let .failed(code): Label(String(format: String(localized: "state_link_failed"), locale: .current, code), systemImage: "exclamationmark.triangle").foregroundStyle(.red)
        case .identityMismatch:
            VStack(alignment: .leading, spacing: KoruSpace.small) {
                Label("identity_mismatch", systemImage: "key.slash").foregroundStyle(KoruColor.danger)
                Text("identity_mismatch_explainer").font(.caption).foregroundStyle(KoruColor.subtle)
                Button("start_over_as_new_device", role: .destructive) { confirmingReset = true }
                    .buttonStyle(.borderless)
                    .font(.caption)
            }
        case .unavailable: Label("native_unavailable", systemImage: "bolt.slash").foregroundStyle(KoruColor.subtle)
        }
    }
}

private struct LinkedDeviceRow: View {
    let device: LinkedDevice
    let unlink: () -> Void

    var body: some View {
        HStack {
            VStack(alignment: .leading, spacing: KoruSpace.xSmall) {
                Text(device.displayName).font(KoruType.body)
                // Two devices on different peer protocol numbers do not sync:
                // that leads, and names the side the update is on. Then the
                // presence with how it is reached, then what it runs — a
                // device that never said shows nothing rather than a guess.
                if let notSyncing {
                    Text(notSyncing).font(.caption).foregroundStyle(KoruColor.danger)
                }
                Text(presence)
                    .font(.caption)
                    .foregroundStyle(device.connected ? Color.green : KoruColor.subtle)
                if let via = device.introducedBy {
                    // A mesh peer: linked through a device, not paired here.
                    Text(String(format: String(localized: "introduced_by"), locale: .current, via))
                        .font(.caption)
                        .foregroundStyle(KoruColor.subtle)
                }
                if let build = device.buildLabel {
                    Text(build).font(.caption).foregroundStyle(buildColor)
                }
                if let millis = device.lastSyncMillis {
                    Text(String(format: String(localized: "last_synced"), locale: .current, relativeTime(millis)))
                        .font(.caption)
                        .foregroundStyle(KoruColor.subtle)
                }
            }
            Spacer()
            // A transitive device arrived through someone else's pairing, so
            // it offers no Unlink — it disappears when that relationship is
            // revoked.
            if device.introducedBy == nil {
                Button("unlink_device", role: .destructive, action: unlink)
                    .buttonStyle(.borderless)
                    .font(.caption)
            }
        }
    }

    private var presence: String {
        switch (device.connected, device.connection) {
        case (true, "direct"): String(localized: "device_connected_direct")
        case (true, "relay"): String(localized: "device_connected_relay")
        case (true, _): String(localized: "device_connected")
        case (false, _): String(localized: "device_disconnected")
        }
    }

    private var notSyncing: String? {
        switch device.updateNeededOn {
        case nil: nil
        case "this_device": String(localized: "not_syncing_update_this_device")
        default: String(format: String(localized: "not_syncing_update_peer"), locale: .current, device.displayName)
        }
    }

    /// A minor version apart in amber, a major apart or incompatible in the
    /// destructive red; the host decides, the view only colors.
    private var buildColor: Color {
        switch device.buildSeverity {
        case "danger": KoruColor.danger
        case "warning": KoruColor.notice
        default: KoruColor.subtle
        }
    }
}

private struct PairingOfferView: View {
    let offer: PairingOffer
    let cancel: () -> Void

    var body: some View {
        VStack(spacing: KoruSpace.medium) {
            if let image = qrImage(from: offer.payload) {
                Image(uiImage: image)
                    .interpolation(.none)
                    .resizable()
                    .scaledToFit()
                    .frame(maxWidth: 240)
                    .accessibilityLabel("pairing_offer")
            } else {
                // The payload still links by hand if the QR cannot draw.
                Text(offer.payload).font(.system(.caption2, design: .monospaced)).textSelection(.enabled)
            }
            Text(String(format: String(localized: "pairing_expires"), locale: .current, offer.expiresInSeconds))
                .font(.caption)
                .foregroundStyle(KoruColor.subtle)
            Button("cancel", role: .cancel, action: cancel)
        }
        .frame(maxWidth: .infinity)
    }

    private func qrImage(from payload: String) -> UIImage? {
        let filter = CIFilter.qrCodeGenerator()
        filter.message = Data(payload.utf8)
        guard let output = filter.outputImage else { return nil }
        let scaled = output.transformed(by: CGAffineTransform(scaleX: 8, y: 8))
        guard let cg = CIContext().createCGImage(scaled, from: scaled.extent) else { return nil }
        return UIImage(cgImage: cg)
    }
}

/// VisionKit's live scanner, narrowed to QR codes. iOS 16+, and this app is
/// 18+ (iroh's `netdev` strong-imports an iOS 18 Network.framework symbol,
/// and on iOS 17 dyld refuses the app); a simulator has no camera, which is
/// why the manual field stays the
/// path a test can walk.
private struct QRScanSheet: UIViewControllerRepresentable {
    let found: (String) -> Void

    func makeCoordinator() -> Coordinator { Coordinator(found: found) }

    func makeUIViewController(context: Context) -> DataScannerViewController {
        let scanner = DataScannerViewController(
            recognizedDataTypes: [.barcode(symbologies: [.qr])],
            qualityLevel: .balanced,
            isHighlightingEnabled: true)
        scanner.delegate = context.coordinator
        try? scanner.startScanning()
        return scanner
    }

    func updateUIViewController(_ controller: DataScannerViewController, context: Context) {}

    final class Coordinator: NSObject, DataScannerViewControllerDelegate {
        let found: (String) -> Void
        private var delivered = false
        init(found: @escaping (String) -> Void) { self.found = found }

        func dataScanner(_ scanner: DataScannerViewController, didAdd added: [RecognizedItem], allItems: [RecognizedItem]) {
            guard !delivered else { return }
            for item in added {
                if case let .barcode(barcode) = item, let payload = barcode.payloadStringValue {
                    delivered = true
                    scanner.stopScanning()
                    found(payload)
                    return
                }
            }
        }
    }
}

/// "3 minutes ago", in the person's language — Android's
/// `DateUtils.getRelativeTimeSpanString`.
func relativeTime(_ millis: Int64) -> String {
    let formatter = RelativeDateTimeFormatter()
    formatter.unitsStyle = .full
    return formatter.localizedString(for: Date(timeIntervalSince1970: Double(millis) / 1000), relativeTo: Date())
}

/// What one linked device changed here over the last seven days, newest
/// first, fifty to a page, fetched when the screen opens and not before —
/// the routine status refresh stays small. Mirrors Android's device sheet.
struct DeviceHistoryScreen: View {
    @EnvironmentObject private var model: AppModel
    @Environment(\.dismiss) private var dismiss
    let device: LinkedDevice
    @State private var events: [DeviceStateEvent] = []
    @State private var nextBefore: Int64?
    @State private var loading = false
    @State private var failed = false
    @State private var openedDocument: DocumentID?

    var body: some View {
        List {
            Section {
                VStack(alignment: .leading, spacing: KoruSpace.xSmall) {
                    Text(device.displayName).font(KoruType.title)
                    if let millis = device.lastSyncMillis {
                        Text(String(format: String(localized: "last_synced"), locale: .current, relativeTime(millis)))
                            .font(.caption).foregroundStyle(KoruColor.subtle)
                    }
                }
            }
            Section("device_history") {
                if loading && events.isEmpty {
                    HStack(spacing: KoruSpace.small) { ProgressView(); Text("history_loading") }
                } else if failed && events.isEmpty {
                    Text("history_unavailable").foregroundStyle(KoruColor.danger)
                } else if events.isEmpty {
                    Text("history_empty").font(.caption).foregroundStyle(KoruColor.subtle)
                } else {
                    ForEach(events) { event in
                        DeviceEventRow(event: event) { id in
                            model.openDocument(id)
                            openedDocument = id
                        }
                        .onAppear {
                            // Page in when the last rows show — the same
                            // two-from-the-end trigger Android uses.
                            if let last = events.last, last.id == event.id { Task { await loadMore(initial: false) } }
                        }
                    }
                    if loading { ProgressView().frame(maxWidth: .infinity) }
                }
            }
        }
        .navigationTitle("device_history")
        .navigationBarTitleDisplayMode(.inline)
        .toolbar { ToolbarItem(placement: .confirmationAction) { Button("done") { dismiss() } } }
        .navigationDestination(item: $openedDocument) { id in DocumentView(documentID: id) }
        .task { await loadMore(initial: true) }
    }

    private func loadMore(initial: Bool) async {
        guard !loading else { return }
        if initial { guard events.isEmpty else { return } } else { guard nextBefore != nil else { return } }
        loading = true
        defer { loading = false }
        do {
            let page = try await model.deviceHistory(deviceID: device.deviceID, before: events.isEmpty ? nil : nextBefore)
            let known = Set(events.compactMap(\.eventId))
            events += page.events.filter { $0.eventId.map { !known.contains($0) } ?? true }
            nextBefore = page.nextBefore
            failed = false
        } catch {
            failed = true
            nextBefore = nil
        }
    }
}

private struct DeviceEventRow: View {
    let event: DeviceStateEvent
    let open: (DocumentID) -> Void

    var body: some View {
        let linked = event.kind == "document_updated" ? event.documentId : nil
        Button {
            if let linked { open(DocumentID(linked)) }
        } label: {
            VStack(alignment: .leading, spacing: KoruSpace.xSmall) {
                Text(label).font(KoruType.body).foregroundStyle(linked != nil ? KoruColor.accent : Color.primary)
                Text(relativeTime(event.atMillis)).font(.caption).foregroundStyle(KoruColor.subtle)
            }
        }
        .buttonStyle(.plain)
        .disabled(linked == nil)
    }

    /// Android's `deviceEventLabel`, word for word: a document by its name and
    /// op count, a reconciliation by what it touched, everything else by
    /// kind and a short id.
    private var label: String {
        if event.kind == "document_updated" {
            let name = event.documentName.flatMap { $0.isEmpty ? nil : $0 } ?? "Untitled"
            let operations = event.items.map { " · \($0) \($0 == 1 ? "op" : "ops")" } ?? ""
            return "Updated “\(name)”\(operations)"
        }
        if event.kind == "catalog_reconciled", let detail = event.detail {
            return "Reconciled \(detail)"
        }
        var target: String?
        if let id = event.documentId { target = String(id.prefix(8)) }
        else if let id = event.workspaceId { target = String(id.prefix(8)) }
        else if let id = event.profileId { target = String(id.prefix(8)) }
        else if let hash = event.blobHash { target = String(hash.prefix(12)) }
        let name: String = switch event.kind {
        case "catalog_reconciled": "Catalog reconciled"
        case "profile_created": "Profile added"
        case "profile_updated": "Profile updated"
        case "workspace_created": "Workspace added"
        case "workspace_updated": "Workspace updated"
        case "document_created": "Document added"
        case "document_updated": "Document updated"
        case "blob_stored": "File downloaded"
        default: Self.capitalized(event.kind.replacingOccurrences(of: "_", with: " "))
        }
        return target.map { "\(name) · \($0)" } ?? name
    }

    private static func capitalized(_ text: String) -> String {
        guard let first = text.first else { return text }
        return first.uppercased() + text.dropFirst()
    }
}

