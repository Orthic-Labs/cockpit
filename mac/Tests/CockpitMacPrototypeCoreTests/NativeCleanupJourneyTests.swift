import XCTest
import Foundation
@testable import CockpitMacPrototypeCore

@MainActor
final class NativeCleanupJourneyTests: XCTestCase {
    private var fixture: URL!
    private var root: URL { fixture.appendingPathComponent("root", isDirectory: true) }
    private var sourceDirectory: URL { root.appendingPathComponent("source", isDirectory: true) }
    private var trash: URL { fixture.appendingPathComponent("trash", isDirectory: true) }
    private var state: URL { fixture.appendingPathComponent("state", isDirectory: true) }

    override func setUpWithError() throws {
        fixture = FileManager.default.temporaryDirectory.appendingPathComponent("cockpit-native-cleanup-\(UUID().uuidString)", isDirectory: true)
        try FileManager.default.createDirectory(at: sourceDirectory, withIntermediateDirectories: true, attributes: [.posixPermissions: 0o700])
        try FileManager.default.createDirectory(at: trash, withIntermediateDirectories: false, attributes: [.posixPermissions: 0o700])
        try FileManager.default.createDirectory(at: state, withIntermediateDirectories: false, attributes: [.posixPermissions: 0o700])
    }

    override func tearDownWithError() throws { try? FileManager.default.removeItem(at: fixture) }

    func testReviewApplyRestartUndoConflictAndAncestorProtection() throws {
        let source = sourceDirectory.appendingPathComponent("report.txt")
        let hardlink = sourceDirectory.appendingPathComponent("report-hardlink.txt")
        try Data("retained".utf8).write(to: source)
        try FileManager.default.linkItem(atPath: source.path, toPath: hardlink.path)
        let alias = sourceDirectory.appendingPathComponent("report-alias.txt")
        try FileManager.default.createSymbolicLink(at: alias, withDestinationURL: source)

        let first = NativeCleanupService(stateDirectory: state, trashDirectory: trash)
        XCTAssertThrowsError(try first.reviewForTesting(paths: [alias], root: root)) { error in
            XCTAssertEqual(error as? NativeCleanupService.Error, .unsupported("symlink"))
        }
        try FileManager.default.removeItem(at: alias)
        let review = try first.reviewForTesting(paths: [source], root: root)
        let planID = try XCTUnwrap(review["plan_id"] as? String)
        let reviewedItems = try XCTUnwrap(review["items"] as? [[String: Any]])
        XCTAssertEqual(reviewedItems.first?["filename"] as? String, "report.txt")
        XCTAssertEqual(reviewedItems.first?["logical_bytes"] as? UInt64, 8)

        let applied = try first.apply(planID: planID)
        XCTAssertEqual(applied["state"] as? String, "completed")
        XCTAssertFalse(FileManager.default.fileExists(atPath: source.path))
        XCTAssertTrue(FileManager.default.fileExists(atPath: hardlink.path), "hardlink is retained")
        let appliedOutcomes = try XCTUnwrap(applied["outcomes"] as? [[String: Any]])
        let trashPath = try XCTUnwrap(appliedOutcomes.first?["trash_path"] as? String)
        XCTAssertTrue(FileManager.default.fileExists(atPath: trashPath))

        let restarted = NativeCleanupService(stateDirectory: state, trashDirectory: trash)
        let history = try restarted.historyPayload()
        XCTAssertEqual((history["plans"] as? [[String: Any]])?.count, 1)
        XCTAssertThrowsError(try restarted.apply(planID: planID)) { error in
            XCTAssertEqual(error as? NativeCleanupService.Error, .reviewRequired)
        }
        let restored = try restarted.undo(planID: planID)
        XCTAssertEqual(restored["state"] as? String, "completed")
        XCTAssertTrue(FileManager.default.fileExists(atPath: source.path))
        XCTAssertTrue(FileManager.default.fileExists(atPath: hardlink.path))
        XCTAssertThrowsError(try restarted.undo(planID: planID)) { error in
            XCTAssertEqual(error as? NativeCleanupService.Error, .planAlreadyClaimed)
        }

        let conflict = sourceDirectory.appendingPathComponent("conflict.txt")
        try Data("conflict".utf8).write(to: conflict)
        let conflictReview = try restarted.reviewForTesting(paths: [conflict], root: root)
        let conflictPlan = try XCTUnwrap(conflictReview["plan_id"] as? String)
        _ = try restarted.apply(planID: conflictPlan)
        try Data("new occupant".utf8).write(to: conflict)
        let conflictUndo = try restarted.undo(planID: conflictPlan)
        let conflictOutcomes = try XCTUnwrap(conflictUndo["outcomes"] as? [[String: Any]])
        XCTAssertEqual(conflictOutcomes.first?["status"] as? String, "conflict_original_occupied")
        let conflictHistory = try restarted.historyPayload()
        let conflictPlanRows = try XCTUnwrap(conflictHistory["plans"] as? [[String: Any]])
        let conflictRow = try XCTUnwrap(conflictPlanRows.first(where: { $0["plan_id"] as? String == conflictPlan }))
        let conflictItemRows = try XCTUnwrap(conflictRow["items"] as? [[String: Any]])
        let conflictOutcome = try XCTUnwrap(conflictItemRows.first?["outcome"] as? [String: Any])
        let conflictTrashPath = try XCTUnwrap(conflictOutcome["trash_path"] as? String)
        XCTAssertTrue(FileManager.default.fileExists(atPath: conflictTrashPath))
        try FileManager.default.removeItem(at: conflict)
        XCTAssertThrowsError(try restarted.undo(planID: conflictPlan)) { error in
            XCTAssertEqual(error as? NativeCleanupService.Error, .planAlreadyClaimed)
        }

        let replacedParent = root.appendingPathComponent("replace-me", isDirectory: true)
        let movedParent = root.appendingPathComponent("moved-parent", isDirectory: true)
        try FileManager.default.createDirectory(at: replacedParent, withIntermediateDirectories: false, attributes: [.posixPermissions: 0o700])
        let guarded = replacedParent.appendingPathComponent("guarded.txt")
        try Data("guarded".utf8).write(to: guarded)
        let guardedReview = try restarted.reviewForTesting(paths: [guarded], root: root)
        let guardedPlan = try XCTUnwrap(guardedReview["plan_id"] as? String)
        try FileManager.default.moveItem(at: replacedParent, to: movedParent)
        try FileManager.default.createSymbolicLink(at: replacedParent, withDestinationURL: movedParent)
        let guardedApply = try restarted.apply(planID: guardedPlan)
        let guardedOutcomes = try XCTUnwrap(guardedApply["outcomes"] as? [[String: Any]])
        XCTAssertEqual(guardedOutcomes.first?["status"] as? String, "failed")
        XCTAssertTrue(FileManager.default.fileExists(atPath: movedParent.appendingPathComponent("guarded.txt").path))

        let crashSource = sourceDirectory.appendingPathComponent("crash.txt")
        let crashSecond = sourceDirectory.appendingPathComponent("crash-second.txt")
        try Data("crash".utf8).write(to: crashSource)
        try Data("crash-second".utf8).write(to: crashSecond)
        let crashService = NativeCleanupService(stateDirectory: state, trashDirectory: trash)
        let crashReview = try crashService.reviewForTesting(paths: [crashSource, crashSecond], root: root)
        let crashPlan = try XCTUnwrap(crashReview["plan_id"] as? String)
        crashService.crashAfterRenameForTesting = true
        let interrupted = try crashService.apply(planID: crashPlan)
        XCTAssertEqual(interrupted["state"] as? String, "interrupted")
        XCTAssertFalse(FileManager.default.fileExists(atPath: crashSource.path))
        XCTAssertTrue(FileManager.default.fileExists(atPath: crashSecond.path), "untouched Planned item remains in place")
        let recovered = NativeCleanupService(stateDirectory: state, trashDirectory: trash)
        recovered.crashAfterUndoRenameForTesting = true
        let undoInterrupted = try recovered.undo(planID: crashPlan)
        XCTAssertEqual(undoInterrupted["state"] as? String, "interrupted")
        let recoveredAfterUndoCrash = NativeCleanupService(stateDirectory: state, trashDirectory: trash)
        XCTAssertThrowsError(try recoveredAfterUndoCrash.undo(planID: crashPlan)) { error in
            XCTAssertEqual(error as? NativeCleanupService.Error, .planAlreadyClaimed)
        }
        XCTAssertTrue(FileManager.default.fileExists(atPath: crashSource.path))
        XCTAssertTrue(FileManager.default.fileExists(atPath: crashSecond.path), "untouched item is never implicitly applied")
    }
}
