import Darwin
import XCTest

/// Void has no Equatable conformance; compare success/failure without dropping errors.
func assertVoidResultEqual<E: Error & Equatable>(
    _ actual: Result<Void, E>, _ expected: Result<Void, E>,
    file: StaticString = #filePath, line: UInt = #line
) {
    switch (actual, expected) {
    case (.success, .success): break
    case (.failure(let left), .failure(let right)):
        XCTAssertEqual(left, right, file: file, line: line)
    default: XCTFail("Expected \(expected), received \(actual)", file: file, line: line)
    }
}

// POSIX canonicalization keeps /private/var intact; Foundation standardization may
// collapse it back to /var, which is intentionally refused by no-follow access.
func canonicalFixtureDirectory(_ directory: URL) throws -> URL {
    guard let resolved = realpath(directory.path, nil) else {
        throw POSIXError(POSIXErrorCode(rawValue: errno) ?? .EIO)
    }
    defer { free(resolved) }
    return URL(fileURLWithPath: String(cString: resolved), isDirectory: true)
}
