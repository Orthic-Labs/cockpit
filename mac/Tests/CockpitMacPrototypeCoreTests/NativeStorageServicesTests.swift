import CoreGraphics
import Foundation
import ImageIO
import XCTest
@testable import CockpitMacPrototypeCore

@MainActor
final class NativeStorageServicesTests: XCTestCase {
    func testNativeCompressionJourneyPersistsActivityAndProtectsExistingOutput() async throws {
        let root = URL(fileURLWithPath: ProcessInfo.processInfo.environment["RUNNER_TEMP"]
                       ?? FileManager.default.temporaryDirectory.resolvingSymlinksInPath().path,
                       isDirectory: true)
            .appendingPathComponent("cockpit-native-\(UUID().uuidString)", isDirectory: true)
        let input = root.appendingPathComponent("input", isDirectory: true)
        let output = root.appendingPathComponent("output", isDirectory: true)
        let state = root.appendingPathComponent("state", isDirectory: true)
        try FileManager.default.createDirectory(at: input, withIntermediateDirectories: true)
        try FileManager.default.createDirectory(at: output, withIntermediateDirectories: true)
        defer { try? FileManager.default.removeItem(at: root) }

        let source = input.appendingPathComponent("fixture.png")
        try writeFixtureImage(to: source)
        let original = try Data(contentsOf: source)
        let request = MediaCompressionRequest(sourceURL: source, inputDirectoryURL: input,
                                              outputDirectoryURL: output, format: .jpeg,
                                              quality: 0.8, maxPixelDimension: nil, targetSizeBytes: nil)

        let first = await MediaCompressionJob(request: request).run()
        guard case .success(let firstResult) = first else { return XCTFail("native image encode failed: \(first)") }
        XCTAssertEqual(try Data(contentsOf: source), original)
        XCTAssertTrue(FileManager.default.fileExists(atPath: firstResult.outputURL.path))
        let firstOutput = try Data(contentsOf: firstResult.outputURL)

        let second = await MediaCompressionJob(request: request).run()
        guard case .success(let secondResult) = second else { return XCTFail("repeat native image encode failed: \(second)") }
        XCTAssertNotEqual(firstResult.outputURL, secondResult.outputURL)
        XCTAssertEqual(try Data(contentsOf: firstResult.outputURL), firstOutput)
        XCTAssertEqual(try Data(contentsOf: source), original)

        let service = NativeStorageServices(stateDirectory: state)
        try service.recordCompressionObservation(firstResult, format: "jpeg")
        let beforeRestart = try service.activityPayload()
        let restarted = NativeStorageServices(stateDirectory: state)
        let afterRestart = try restarted.activityPayload()
        XCTAssertEqual((beforeRestart["events"] as? [[String: Any]])?.count, 1)
        XCTAssertEqual((afterRestart["events"] as? [[String: Any]])?.count, 1)
        XCTAssertEqual((afterRestart["week"] as? [String: Any])?["measuredSavedBytes"] as? Int64,
                       firstResult.measuredSavedBytes)
    }

    private func writeFixtureImage(to url: URL) throws {
        let colorSpace = CGColorSpaceCreateDeviceRGB()
        var pixels = [UInt8](repeating: 0, count: 32 * 32 * 4)
        for index in stride(from: 0, to: pixels.count, by: 4) {
            pixels[index] = 35
            pixels[index + 1] = 100
            pixels[index + 2] = 220
            pixels[index + 3] = 255
        }
        guard let context = CGContext(data: &pixels, width: 32, height: 32, bitsPerComponent: 8,
                                      bytesPerRow: 32 * 4, space: colorSpace,
                                      bitmapInfo: CGImageAlphaInfo.premultipliedLast.rawValue),
              let image = context.makeImage(),
              let destination = CGImageDestinationCreateWithURL(url as CFURL, "public.png" as CFString, 1, nil) else {
            throw NSError(domain: "NativeStorageServicesTests", code: 1)
        }
        CGImageDestinationAddImage(destination, image, nil)
        guard CGImageDestinationFinalize(destination) else {
            throw NSError(domain: "NativeStorageServicesTests", code: 2)
        }
    }
}
