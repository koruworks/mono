// SPDX-License-Identifier: MPL-2.0

import XCTest

@testable import KoruMobile

/// The inbox format's pin — ADR-137. The extension writes it, the app reads
/// it, and this suite (compiled with the app) is where the two are held to one
/// story: commit is the manifest, uncommitted is invisible, old debris sweeps.
final class ShareInboxTests: XCTestCase {
    private func freshInbox() -> ShareInbox {
        ShareInbox(
            root: FileManager.default.temporaryDirectory
                .appendingPathComponent("share-inbox-\(UUID().uuidString)", isDirectory: true))
    }

    /// The group is the build's, never the code's: `KoruAppGroup` in the
    /// app's Info.plist is `group.` + its bundle id, for Koru
    /// (`group.do.koru.app`) and Koru Nightly (`group.do.koru.app.nightly`)
    /// alike, which is the group both entitlements files name.
    func testTheAppGroupIsTheBuildsOwn() throws {
        let bundleId = try XCTUnwrap(Bundle.main.bundleIdentifier)
        XCTAssertEqual(ShareInbox.appGroup, "group.\(bundleId)")
    }

    func testACommittedEntryRoundTrips() throws {
        let inbox = freshInbox()
        let entry = try inbox.begin()
        try Data("hello".utf8).write(to: entry.appendingPathComponent("payload-1"))
        let manifest = ShareInbox.Manifest(files: [
            ShareInbox.File(file: "payload-1", name: "Note.txt", mimeType: "text/plain", declaredSize: 5)
        ])
        try inbox.commit(entry, manifest: manifest)

        let drained = inbox.drain()
        XCTAssertEqual(drained.count, 1)
        XCTAssertEqual(drained[0].manifest, manifest)
        let bytes = try Data(
            contentsOf: drained[0].directory.appendingPathComponent("payload-1"))
        XCTAssertEqual(String(decoding: bytes, as: UTF8.self), "hello")

        inbox.remove(drained[0])
        XCTAssertTrue(inbox.drain().isEmpty)
    }

    func testAnUncommittedEntryIsInvisible() throws {
        let inbox = freshInbox()
        let entry = try inbox.begin()
        try Data("half".utf8).write(to: entry.appendingPathComponent("payload-1"))
        // No manifest: a crash mid-copy. Invisible — and, being young, not yet
        // swept, because it may be an extension writing right now.
        XCTAssertTrue(inbox.drain().isEmpty)
        XCTAssertTrue(FileManager.default.fileExists(atPath: entry.path))
    }

    func testEntriesDrainOldestFirst() throws {
        let inbox = freshInbox()
        for name in ["first", "second"] {
            let entry = try inbox.begin()
            try inbox.commit(
                entry,
                manifest: ShareInbox.Manifest(files: [
                    ShareInbox.File(file: "p", name: name, mimeType: "text/plain", declaredSize: nil)
                ]))
            // Creation dates need a beat to differ.
            Thread.sleep(forTimeInterval: 0.05)
        }
        XCTAssertEqual(inbox.drain().map { $0.manifest.files[0].name }, ["first", "second"])
    }

    func testAChosenWorkspaceRoundTripsThroughTheManifest() throws {
        let inbox = freshInbox()
        let entry = try inbox.begin()
        let manifest = ShareInbox.Manifest(
            files: [
                ShareInbox.File(file: "p", name: "Link.url", mimeType: "text/uri-list", declaredSize: nil)
            ],
            workspaceId: "11111111-1111-1111-1111-111111111111")
        try inbox.commit(entry, manifest: manifest)
        XCTAssertEqual(inbox.drain().first?.manifest, manifest)
    }

    /// The optional field is the format's whole versioning story: an entry
    /// written by the pre-picker extension must decode, with no destination.
    func testAManifestFromBeforeThePickerStillDecodes() throws {
        let old = Data(
            """
            {"files":[{"file":"p","name":"Note.txt","mimeType":"text/plain","declaredSize":5}]}
            """.utf8)
        let manifest = try JSONDecoder().decode(ShareInbox.Manifest.self, from: old)
        XCTAssertNil(manifest.workspaceId)
        XCTAssertEqual(manifest.files.first?.name, "Note.txt")
    }
}

/// The destinations snapshot's pin — the format the app exports and the
/// extension's picker reads, held to one story the way the manifest is.
final class ShareDestinationsTests: XCTestCase {
    func testTheSnapshotRoundTrips() throws {
        let snapshot = ShareDestinations(
            profiles: [
                ShareDestinations.Profile(id: "p1", label: "Home", leadingEmoji: "🏠", color: "purple")
            ],
            workspaces: [
                ShareDestinations.Workspace(
                    id: "w1", profileId: "p1", label: "Notes", leadingEmoji: nil, color: nil)
            ],
            generation: 42)
        let decoded = try JSONDecoder().decode(
            ShareDestinations.self, from: JSONEncoder().encode(snapshot))
        XCTAssertEqual(decoded, snapshot)
    }
}

/// The picker leads with the recent destinations in their own section, most
/// recent first, and the profile-grouped sections follow with those
/// workspaces filtered out — no row appears twice.
final class DestinationSectionsTests: XCTestCase {
    private let snapshot = ShareDestinations(
        profiles: [
            ShareDestinations.Profile(id: "p1", label: "Home", leadingEmoji: nil, color: nil),
            ShareDestinations.Profile(id: "p2", label: "Work", leadingEmoji: nil, color: nil),
            ShareDestinations.Profile(id: "empty", label: "Idle", leadingEmoji: nil, color: nil),
        ],
        workspaces: [
            ShareDestinations.Workspace(id: "w1", profileId: "p1", label: "Notes", leadingEmoji: nil, color: nil),
            ShareDestinations.Workspace(id: "w2", profileId: "p2", label: "Plans", leadingEmoji: nil, color: nil),
            ShareDestinations.Workspace(id: "w3", profileId: "p2", label: "Specs", leadingEmoji: nil, color: nil),
        ],
        generation: 1)

    func testRecentsLeadInTheirOwnSectionAndLeaveTheirProfiles() {
        let sections = destinationSections(snapshot, recents: ["w3"])
        XCTAssertEqual(sections.count, 3)
        XCTAssertNil(sections[0].profile)
        XCTAssertEqual(sections[0].workspaces.map(\.id), ["w3"])
        XCTAssertEqual(sections[1].profile?.id, "p1")
        XCTAssertEqual(sections[1].workspaces.map(\.id), ["w1"])
        XCTAssertEqual(sections[2].profile?.id, "p2")
        XCTAssertEqual(sections[2].workspaces.map(\.id), ["w2"])
    }

    func testARecentWorkspaceIsFilteredOutOfItsOwnSection() {
        // w1 is p1's only workspace: recent, it leads — and p1 is not offered.
        let sections = destinationSections(snapshot, recents: ["w1"])
        XCTAssertNil(sections[0].profile)
        XCTAssertEqual(sections[0].workspaces.map(\.id), ["w1"])
        XCTAssertEqual(sections.map(\.profile?.id), [nil, "p2"])
        XCTAssertEqual(sections[1].workspaces.map(\.id), ["w2", "w3"])
    }

    func testRecentsKeepRecencyOrderAndDoNotDuplicate() {
        let sections = destinationSections(snapshot, recents: ["w2", "w3", "w2"])
        XCTAssertEqual(sections[0].workspaces.map(\.id), ["w2", "w3"])
        XCTAssertEqual(sections.map(\.profile?.id), [nil, "p1"])
    }

    func testNoRecentsOrStaleRecentsKeepSnapshotOrderAndDropEmptyProfiles() {
        for recents in [[], ["gone"]] {
            let sections = destinationSections(snapshot, recents: recents)
            XCTAssertEqual(sections.map(\.profile?.id), ["p1", "p2"], "\(recents)")
            XCTAssertEqual(sections[1].workspaces.map(\.id), ["w2", "w3"])
        }
    }
}

/// The recents store's pin — the cap, the dedup, the order, and the legacy
/// single-slot file seeding the first read.
final class ShareRecentDestinationsTests: XCTestCase {
    private func freshRoot() -> URL {
        let root = FileManager.default.temporaryDirectory
            .appendingPathComponent("share-recents-\(UUID().uuidString)", isDirectory: true)
        try? FileManager.default.createDirectory(at: root, withIntermediateDirectories: true)
        return root
    }

    func testAnEmptyRootReadsEmpty() {
        XCTAssertEqual(ShareRecentDestinations.read(in: freshRoot()), [])
    }

    func testRecordingLeadsMostRecentFirst() {
        let root = freshRoot()
        ShareRecentDestinations.record(workspaceId: "w1", in: root)
        ShareRecentDestinations.record(workspaceId: "w2", in: root)
        XCTAssertEqual(
            ShareRecentDestinations.read(in: root),
            [
                ShareRecentDestinations.Entry(workspaceId: "w2"),
                ShareRecentDestinations.Entry(workspaceId: "w1"),
            ])
    }

    func testRecordingAnExistingWorkspaceMovesItToTheFront() {
        let root = freshRoot()
        ShareRecentDestinations.record(workspaceId: "w1", in: root)
        ShareRecentDestinations.record(workspaceId: "w2", in: root)
        ShareRecentDestinations.record(workspaceId: "w1", in: root)
        XCTAssertEqual(
            ShareRecentDestinations.read(in: root),
            [
                ShareRecentDestinations.Entry(workspaceId: "w1"),
                ShareRecentDestinations.Entry(workspaceId: "w2"),
            ])
    }

    func testTheListIsCappedAtThree() {
        let root = freshRoot()
        for id in ["w1", "w2", "w3", "w4"] {
            ShareRecentDestinations.record(workspaceId: id, in: root)
        }
        XCTAssertEqual(
            ShareRecentDestinations.read(in: root).map(\.workspaceId), ["w4", "w3", "w2"])
    }

    func testALegacyLastDestinationSeedsTheFirstRead() throws {
        let root = freshRoot()
        let legacy = try JSONEncoder().encode(["workspaceId": "w9"])
        try legacy.write(to: ShareRecentDestinations.legacyFile(in: root))

        let seeded = ShareRecentDestinations.read(in: root)
        XCTAssertEqual(seeded.map(\.workspaceId), ["w9"])

        // The seed joins the recorded list: a new save leads, the legacy
        // memory follows, and the next read comes from the store's own file.
        ShareRecentDestinations.record(workspaceId: "w1", in: root)
        XCTAssertEqual(
            ShareRecentDestinations.read(in: root).map(\.workspaceId), ["w1", "w9"])
    }
}

/// Which providers on one `NSExtensionItem` stage — the duplicate-URL bug's
/// regression table. Providers on one item are representations of one thing.
final class StagedProviderIndicesTests: XCTestCase {
    func testASafariShareStagesTheURLOnce() {
        // Safari offers a page as url + plain-text siblings: one stages.
        XCTAssertEqual(
            stagedProviderIndices([["public.url"], ["public.plain-text"]]), [0])
    }

    func testAMultiPhotoShareStagesEveryPhoto() {
        let photo = ["public.jpeg", "public.image", "public.data"]
        XCTAssertEqual(stagedProviderIndices([photo, photo, photo]), [0, 1, 2])
    }

    func testAFileBesideItsTextDescriptionStagesTheFile() {
        XCTAssertEqual(
            stagedProviderIndices([
                ["public.plain-text"],
                ["public.file-url", "public.data"],
            ]), [1])
    }

    func testTextPreferenceIsUrlOverHtmlOverPlainText() {
        XCTAssertEqual(
            stagedProviderIndices([["public.plain-text"], ["public.html"], ["public.url"]]), [2])
        XCTAssertEqual(
            stagedProviderIndices([["public.plain-text"], ["public.html"]]), [1])
        XCTAssertEqual(stagedProviderIndices([["public.plain-text"]]), [0])
    }

    func testNothingRecognizableStagesNothing() {
        XCTAssertEqual(stagedProviderIndices([["com.example.opaque"]]), [])
        XCTAssertEqual(stagedProviderIndices([]), [])
    }
}

/// The text-normalization mirror of Android's `SharedTextTest.kt`: what one
/// shared string becomes must not depend on the phone it was shared on.
final class SharedTextTests: XCTestCase {
    func testASingleWebURLBecomesAUrlFile() {
        let file = SharedText.normalize("  https://koru.do/a?b=c  ", subject: nil)
        XCTAssertEqual(file.name, "Shared link.url")
        XCTAssertEqual(file.mimeType, "text/uri-list")
        XCTAssertEqual(file.content, "https://koru.do/a?b=c\n")
    }

    func testPlainTextBecomesATxtFile() {
        let file = SharedText.normalize("two words https://koru.do", subject: nil)
        XCTAssertEqual(file.name, "Shared text.txt")
        XCTAssertEqual(file.mimeType, "text/plain")
        XCTAssertEqual(file.content, "two words https://koru.do")
    }

    func testHtmlBecomesAnHtmlFile() {
        let file = SharedText.normalize("<p>hi</p>", subject: "A Page", html: true)
        XCTAssertEqual(file.name, "A Page.html")
        XCTAssertEqual(file.mimeType, "text/html")
    }

    func testASubjectNamesTheFileAndASuffixIsNotDoubled() {
        XCTAssertEqual(
            SharedText.normalize("https://koru.do", subject: "Roadmap.URL").name, "Roadmap.URL")
        XCTAssertEqual(
            SharedText.normalize("https://koru.do", subject: "Roadmap").name, "Roadmap.url")
    }

    func testDisplayNamesAreSanitized() {
        XCTAssertEqual(safeDisplayName("evil\u{0007}name"), "evil name")
        XCTAssertEqual(safeDisplayName("a/b\\c"), "a b c")
        XCTAssertEqual(safeDisplayName("..name.."), "name")
        XCTAssertEqual(safeDisplayName("   "), "Shared file")
        XCTAssertEqual(safeDisplayName(String(repeating: "x", count: 500)).count, 180)
    }
}
