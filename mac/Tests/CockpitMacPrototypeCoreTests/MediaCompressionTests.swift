import XCTest
@testable import CockpitMacPrototypeCore

final class MediaCompressionTests: XCTestCase {
    private var root: URL!
    private var input: URL { root.appendingPathComponent("input", isDirectory: true) }
    private var output: URL { root.appendingPathComponent("output", isDirectory: true) }
    private var source: URL { input.appendingPathComponent("fixture.png") }

    override func setUpWithError() throws {
        root = FileManager.default.temporaryDirectory
            .resolvingSymlinksInPath()
            .appendingPathComponent("cockpit-media-\(UUID().uuidString)", isDirectory: true)
        try FileManager.default.createDirectory(at: input, withIntermediateDirectories: true)
        try FileManager.default.createDirectory(at: output, withIntermediateDirectories: false)
        // Disposable 1x1 RGBA PNG. Tests never touch user media.
        let png = Data([
            0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A,
            0x00, 0x00, 0x00, 0x0D, 0x49, 0x48, 0x44, 0x52,
            0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01,
            0x08, 0x06, 0x00, 0x00, 0x00, 0x1F, 0x15, 0xC4,
            0x89, 0x00, 0x00, 0x00, 0x0D, 0x49, 0x44, 0x41,
            0x54, 0x78, 0x9C, 0x63, 0xF8, 0xCF, 0xC0, 0xF0,
            0x1F, 0x00, 0x05, 0x00, 0x01, 0xFF, 0x89, 0x99,
            0x3D, 0x1D, 0x00, 0x00, 0x00, 0x00, 0x49, 0x45,
            0x4E, 0x44, 0xAE, 0x42, 0x60, 0x82
        ])
        try png.write(to: source, options: .withoutOverwriting)
    }

    override func tearDownWithError() throws {
        try? FileManager.default.removeItem(at: root)
    }

    private func request(format: MediaCompressionFormat = .jpeg,
                         quality: Double = 0.8,
                         target: Int64? = nil) -> MediaCompressionRequest {
        MediaCompressionRequest(sourceURL: source, inputDirectoryURL: input,
                                outputDirectoryURL: output, format: format,
                                quality: quality, maxPixelDimension: nil,
                                targetSizeBytes: target)
    }

    func testInvalidParametersAreRejectedWithoutEncoding() async {
        let job = MediaCompressionJob(request: request(quality: 2))
        let result = await job.run()
        XCTAssertEqual(result, .failure(.invalidRequest))
        XCTAssertEqual(job.phase, .failed)
    }

    func testCancellationStopsBeforePublishAndLeavesOriginal() async throws {
        let original = try Data(contentsOf: source)
        let job = MediaCompressionJob(request: request())
        job.cancel()
        let result = await job.run()
        XCTAssertEqual(result, .failure(.cancelled))
        XCTAssertEqual(job.phase, .cancelled)
        XCTAssertEqual(try Data(contentsOf: source), original)
        XCTAssertTrue(try FileManager.default.contentsOfDirectory(at: output, includingPropertiesForKeys: nil).isEmpty)
    }

    func testImageEncodePublishesUniqueOutputAndPreservesOriginal() async throws {
        let original = try Data(contentsOf: source)
        let result = await MediaCompressionJob(request: request()).run()
        guard case .success(let result) = result else { return XCTFail("synthetic image should encode") }
        XCTAssertTrue(FileManager.default.fileExists(atPath: result.outputURL.path))
        XCTAssertGreaterThan(result.outputBytes, 0)
        XCTAssertNil(result.estimate)
        XCTAssertEqual(try Data(contentsOf: source), original)
        XCTAssertNotEqual(result.outputURL, source)
    }

    func testTargetSizeFailureRemovesOnlyJobOutputAndPreservesOriginal() async throws {
        let original = try Data(contentsOf: source)
        let result = await MediaCompressionJob(request: request(target: 1)).run()
        XCTAssertEqual(result, .failure(.outputExceedsTarget))
        XCTAssertEqual(try Data(contentsOf: source), original)
        XCTAssertTrue(try FileManager.default.contentsOfDirectory(at: output, includingPropertiesForKeys: nil).isEmpty)
    }

    func testSymlinkedSourceIsRejected() async throws {
        let planted = root.appendingPathComponent("planted.png")
        try Data([1, 2, 3]).write(to: planted, options: .withoutOverwriting)
        try FileManager.default.removeItem(at: source)
        try FileManager.default.createSymbolicLink(at: source, withDestinationURL: planted)
        let result = await MediaCompressionJob(request: request()).run()
        XCTAssertEqual(result, .failure(.sourceSymlink))
        XCTAssertEqual(try Data(contentsOf: planted), Data([1, 2, 3]))
    }
}
