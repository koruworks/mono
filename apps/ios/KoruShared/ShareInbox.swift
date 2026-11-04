// SPDX-License-Identifier: MPL-2.0

import Foundation

/// The staging inbox between the share extension and the app — ADR-137.
///
/// The extension writes an entry (payload files first, `manifest.json` **last,
/// as the commit marker**) and never opens the Store; the app drains committed
/// entries on activation and imports them through the ordinary
/// `WorkspaceFileService.save` path. An entry without a manifest is a crash
/// mid-copy: swept on the next drain, never imported.
///
/// This file is compiled into **both** targets (the `KoruShared` synchronized
/// group), which is what keeps the format from existing twice. The app's unit
/// tests are where it is pinned.
struct ShareInbox: Sendable {
    /// One shared payload, as the manifest records it.
    struct File: Codable, Equatable, Sendable {
        /// The payload's filename inside the entry directory (never a path).
        let file: String
        /// The display name the import will give the document.
        let name: String
        let mimeType: String
        let declaredSize: Int64?
        /// The note the import attaches to the file, when the sharer wrote
        /// one. Optional on purpose: entries from before the field decode
        /// fine and import without a note — the same versioning story as the
        /// manifest's `workspaceId`.
        let note: String?

        init(file: String, name: String, mimeType: String, declaredSize: Int64?, note: String? = nil) {
            self.file = file
            self.name = name
            self.mimeType = mimeType
            self.declaredSize = declaredSize
            self.note = note
        }
    }

    struct Manifest: Codable, Equatable, Sendable {
        var files: [File]
        /// The workspace the sharer chose in the extension's picker, when the
        /// destinations snapshot let them. Optional on purpose: entries from
        /// before the picker (and from a fresh install with no snapshot)
        /// decode fine and take the ask-in-app path — this field is the
        /// format's whole versioning story.
        var workspaceId: String?

        init(files: [File], workspaceId: String? = nil) {
            self.files = files
            self.workspaceId = workspaceId
        }
    }

    /// A committed entry, ready to import.
    struct Entry: Identifiable, Equatable, Sendable {
        let id: String
        let directory: URL
        let manifest: Manifest
    }

    /// The App Group both targets share, as the build names it: the
    /// `KoruAppGroup` key each target's Info.plist takes from the build
    /// setting `KORU_APP_GROUP`, the same setting both entitlements files
    /// read. `group.do.koru.app` for Koru, `group.do.koru.app.nightly` for
    /// Koru Nightly (`Config/Nightly.xcconfig`); never written here, so the
    /// code cannot name a group the build did not sign for.
    static let appGroup: String? = Bundle.main.object(forInfoDictionaryKey: "KoruAppGroup") as? String

    /// The App Group container, or `nil` when the build names no group or
    /// the entitlement is absent - a build problem, not a runtime state.
    static func groupContainer() -> URL? {
        guard let group = appGroup, !group.isEmpty else { return nil }
        return FileManager.default.containerURL(forSecurityApplicationGroupIdentifier: group)
    }

    let root: URL

    /// The real inbox, in the App Group container both targets share. `nil`
    /// when the entitlement is absent — a build problem, not a runtime state,
    /// but the extension must degrade to "nothing shared" rather than crash.
    static func shared() -> ShareInbox? {
        groupContainer()
            .map { ShareInbox(root: $0.appendingPathComponent("inbox", isDirectory: true)) }
    }

    // ---------------------------------------------------------- the writer

    /// A directory to stage one share's payloads into. Nothing in it is
    /// visible to a drain until [`commit`] writes the manifest.
    func begin() throws -> URL {
        let entry = root.appendingPathComponent(UUID().uuidString, isDirectory: true)
        try FileManager.default.createDirectory(at: entry, withIntermediateDirectories: true)
        return entry
    }

    /// The commit marker. Written atomically, and last — the order is the
    /// whole crash-safety story.
    func commit(_ entry: URL, manifest: Manifest) throws {
        let encoder = JSONEncoder()
        encoder.outputFormatting = [.prettyPrinted, .sortedKeys]
        try encoder.encode(manifest)
            .write(to: entry.appendingPathComponent("manifest.json"), options: .atomic)
    }

    // ---------------------------------------------------------- the reader

    /// Every committed entry, oldest first — and, as a side effect, the sweep:
    /// an uncommitted entry older than an hour is a crash's debris and is
    /// removed (young ones may be an extension mid-copy right now).
    func drain() -> [Entry] {
        let contents = (try? FileManager.default.contentsOfDirectory(
            at: root, includingPropertiesForKeys: [.creationDateKey], options: [])) ?? []
        var entries: [(Date, Entry)] = []
        for directory in contents {
            let manifestURL = directory.appendingPathComponent("manifest.json")
            let created = (try? directory.resourceValues(forKeys: [.creationDateKey]))?
                .creationDate ?? .distantPast
            guard
                let data = try? Data(contentsOf: manifestURL),
                let manifest = try? JSONDecoder().decode(Manifest.self, from: data)
            else {
                if created < Date(timeIntervalSinceNow: -3600) {
                    try? FileManager.default.removeItem(at: directory)
                }
                continue
            }
            entries.append(
                (created, Entry(id: directory.lastPathComponent, directory: directory, manifest: manifest)))
        }
        return entries.sorted { $0.0 < $1.0 }.map(\.1)
    }

    /// Remove one entry — after a successful import, or an explicit dismissal.
    func remove(_ entry: Entry) {
        try? FileManager.default.removeItem(at: entry.directory)
    }
}

// ------------------------------------------------------------ destinations

/// What the extension's picker shows: the app's profiles and their live
/// workspaces, exported as **display hints** into the App Group whenever the
/// inventory moves. Staleness is expected and safe — the import validates the
/// chosen workspace still exists and falls back to asking in the app.
///
/// One struct, compiled into both targets (the `KoruShared` group), pinned by
/// the app's unit tests — the same discipline as the inbox manifest.
struct ShareDestinations: Codable, Equatable, Sendable {
    struct Profile: Codable, Equatable, Sendable {
        let id: String
        let label: String
        var leadingEmoji: String?
        var color: String?
    }

    struct Workspace: Codable, Equatable, Sendable {
        let id: String
        let profileId: String
        let label: String
        var leadingEmoji: String?
        var color: String?
    }

    var profiles: [Profile]
    var workspaces: [Workspace]
    /// When the app wrote this, epoch millis — display/debugging only.
    var generation: Int64

    static func file(in groupRoot: URL) -> URL {
        groupRoot.appendingPathComponent("destinations.json")
    }

    /// The snapshot the app last exported, or `nil` — a fresh install, or an
    /// app that never ran. The extension degrades to destination-less staging.
    static func read() -> ShareDestinations? {
        guard
            let group = ShareInbox.groupContainer(),
            let data = try? Data(contentsOf: file(in: group))
        else { return nil }
        return try? JSONDecoder().decode(ShareDestinations.self, from: data)
    }

    /// Atomic, like the manifest: a torn snapshot must never be readable.
    func write() throws {
        guard let group = ShareInbox.groupContainer() else { return }
        let encoder = JSONEncoder()
        encoder.outputFormatting = [.prettyPrinted, .sortedKeys]
        try encoder.encode(self).write(to: Self.file(in: group), options: .atomic)
    }
}

// ------------------------------------------------------- recent destinations

/// The workspaces content was last saved into — up to three, most recent
/// first — recorded by whichever side did the saving (the extension's picker
/// or the app's fallback sheet) and read by both to lead with them: the
/// workspace someone shared into is the one they will most likely share into
/// next.
///
/// The successor of the single-slot `last-destination.json`: a read that
/// finds no file of its own but a legacy one seeds from it, so an install
/// that pre-dates the list keeps its one memory as the most recent entry.
enum ShareRecentDestinations {
    struct Entry: Codable, Equatable, Sendable {
        let workspaceId: String
    }

    static let limit = 3

    static func file(in groupRoot: URL) -> URL {
        groupRoot.appendingPathComponent("recent-destinations.json")
    }

    static func legacyFile(in groupRoot: URL) -> URL {
        groupRoot.appendingPathComponent("last-destination.json")
    }

    static func read() -> [Entry] {
        guard let group = ShareInbox.groupContainer() else { return [] }
        return read(in: group)
    }

    static func record(workspaceId: String) {
        guard let group = ShareInbox.groupContainer() else { return }
        record(workspaceId: workspaceId, in: group)
    }

    /// The recorded list, most recent first — the order is insertion order.
    /// Seeded from the legacy single-slot file when it is the only one there —
    /// that entry becomes the most recent.
    static func read(in groupRoot: URL) -> [Entry] {
        if let data = try? Data(contentsOf: file(in: groupRoot)),
            let entries = try? JSONDecoder().decode([Entry].self, from: data)
        {
            return entries
        }
        let legacyURL = legacyFile(in: groupRoot)
        guard let data = try? Data(contentsOf: legacyURL),
            let legacy = try? JSONDecoder().decode([String: String].self, from: data),
            let workspaceId = legacy["workspaceId"]
        else { return [] }
        return [Entry(workspaceId: workspaceId)]
    }

    static func record(workspaceId: String, in groupRoot: URL) {
        let next = recorded(read(in: groupRoot), workspaceId: workspaceId)
        let encoder = JSONEncoder()
        encoder.outputFormatting = [.prettyPrinted, .sortedKeys]
        guard let data = try? encoder.encode(next) else { return }
        try? data.write(to: file(in: groupRoot), options: .atomic)
    }

    /// The merge, pure so the cap/dedup/order rules are pinnable in tests:
    /// the new entry leads, recording an existing workspace moves it to the
    /// front, and the list never exceeds the cap.
    static func recorded(_ entries: [Entry], workspaceId: String) -> [Entry] {
        Array(
            ([Entry(workspaceId: workspaceId)]
                + entries.filter { $0.workspaceId != workspaceId })
                .prefix(limit))
    }
}

/// One section of the picker's table: a profile's workspaces — or the leading
/// "Recent" section, whose `profile` is `nil` so the caller titles it.
struct ShareDestinationSection: Equatable, Sendable {
    let profile: ShareDestinations.Profile?
    let workspaces: [ShareDestinations.Workspace]
}

/// The picker's sections: the recent destinations first, most recent first —
/// then workspaces grouped under their profiles in the snapshot's order with
/// the recent ones filtered out, so no row appears twice. A recent the
/// snapshot no longer knows (the snapshot is hints; the app validates for
/// real at import) is dropped, and profiles with no live workspace left are
/// not offered. Pure, so the app's unit tests can pin the order without an
/// extension host.
func destinationSections(
    _ destinations: ShareDestinations, recents: [String]
) -> [ShareDestinationSection] {
    var seen = Set<String>()
    let recentRows = recents.compactMap { id -> ShareDestinations.Workspace? in
        guard seen.insert(id).inserted else { return nil }
        return destinations.workspaces.first { $0.id == id }
    }
    var sections: [ShareDestinationSection] = []
    if !recentRows.isEmpty {
        sections.append(ShareDestinationSection(profile: nil, workspaces: recentRows))
    }
    let recentIds = Set(recentRows.map(\.id))
    sections += destinations.profiles.compactMap { profile in
        let workspaces = destinations.workspaces.filter {
            $0.profileId == profile.id && !recentIds.contains($0.id)
        }
        return workspaces.isEmpty
            ? nil
            : ShareDestinationSection(profile: profile, workspaces: workspaces)
    }
    return sections
}

// ----------------------------------------------------------- best provider

/// Which of one `NSExtensionItem`'s providers to stage, judged from their
/// registered type identifiers.
///
/// Providers on one item are *representations of one thing*: Safari offers a
/// shared page as `public.url` **and** `public.plain-text`, and staging both
/// saved the same link twice. The rule: every file-shaped provider stages
/// (a multi-photo share is many real payloads on one item), but at most one
/// text-shaped provider does, preferring url over html over plain text. Pure,
/// so the app's unit tests can hold the table without an extension host.
func stagedProviderIndices(_ providersTypes: [[String]]) -> [Int] {
    // UTType identifiers, spelled as strings so this stays hostable in tests
    // without UniformTypeIdentifiers value lookups.
    let fileURL = "public.file-url"
    let url = "public.url"
    let html = "public.html"
    let text = "public.plain-text"
    let data = "public.data"

    func isFileShaped(_ types: [String]) -> Bool {
        if types.contains(fileURL) { return true }
        // A url-only or text-only provider is text-shaped even though url and
        // text conform to data; anything else offering data is a payload.
        if types.contains(url) || types.contains(html) || types.contains(text) {
            return false
        }
        return types.contains(data)
    }

    let files = providersTypes.indices.filter { isFileShaped(providersTypes[$0]) }
    if !files.isEmpty {
        return files
    }
    for preferred in [url, html, text] {
        if let best = providersTypes.firstIndex(where: { $0.contains(preferred) }) {
            return [best]
        }
    }
    return []
}

// ------------------------------------------------------------- normalization

/// What one piece of shared *text* becomes — the exact rules of Android's
/// `SharedText.kt`, so a link shared on either phone lands as the same kind of
/// document: a single web URL is a `.url` file, HTML is `.html`, anything else
/// is `.txt`.
enum SharedText {
    struct TextFile: Equatable {
        let name: String
        let mimeType: String
        let content: String
    }

    static func normalize(_ value: String, subject: String?, html: Bool = false) -> TextFile {
        let trimmed = value.trimmingCharacters(in: .whitespacesAndNewlines)
        let isURL =
            !html
            && trimmed.range(
                of: "^https?://\\S+$", options: [.regularExpression, .caseInsensitive]) != nil
        let (fallback, suffix, mimeType, content): (String, String, String, String) =
            if html {
                ("Shared page", ".html", "text/html", value)
            } else if isURL {
                ("Shared link", ".url", "text/uri-list", trimmed + "\n")
            } else {
                ("Shared text", ".txt", "text/plain", value)
            }
        let cleanedSubject = safeDisplayName(subject ?? "")
        let base = cleanedSubject == "Shared file" ? fallback : cleanedSubject
        let named = base.lowercased().hasSuffix(suffix) ? base : base + suffix
        return TextFile(name: safeDisplayName(named), mimeType: mimeType, content: content)
    }
}

/// A display name that cannot escape, control a terminal, or overflow a row —
/// the mirror of Android's `safeDisplayName`, fallback included.
func safeDisplayName(_ value: String) -> String {
    let stripped = value.unicodeScalars.map { scalar -> Character in
        let category = scalar.properties.generalCategory
        if category == .control || category == .format || scalar == "/" || scalar == "\\" {
            return " "
        }
        return Character(scalar)
    }
    let cleaned = String(stripped)
        .trimmingCharacters(in: .whitespacesAndNewlines)
        .trimmingCharacters(in: CharacterSet(charactersIn: "."))
        .trimmingCharacters(in: .whitespacesAndNewlines)
        .prefix(180)
    let result = String(cleaned)
    return result.isEmpty ? "Shared file" : result
}
