import AVFoundation
import CoreGraphics
import Foundation
import ImageIO
import UniformTypeIdentifiers
import Darwin

/// Formats backed by the current Mac adapter. WebP, MKV, and other codecs
/// remain unsupported until a native encoder and qualification matrix exist.
public enum MediaCompressionFormat: String, CaseIterable, Codable, Equatable {
    case jpeg
    case heic
    case png
    case mp4
    case mov

    fileprivate var isVideo: Bool { self == .mp4 || self == .mov }
    fileprivate var fileExtension: String { rawValue == "jpeg" ? "jpg" : rawValue }

    fileprivate var imageUTI: String? {
        switch self {
        case .jpeg: return UTType.jpeg.identifier
        case .heic: return UTType.heic.identifier
        case .png: return UTType.png.identifier
        case .mp4, .mov: return nil
        }
    }

    fileprivate var videoFileType: AVFileType? {
        switch self {
        case .mp4: return .mp4
        case .mov: return .mov
        case .jpeg, .heic, .png: return nil
        }
    }
}

public enum MediaCompressionError: Error, Equatable {
    case invalidRequest
    case sourceMissing
    case sourceNotRegular
    case sourceSymlink
    case sourcePlaceholder
    case sourceIdentityUnavailable
    case unsafeAncestor
    case sourceOutsideInputFolder
    case outputDirectoryMissing
    case outputDirectorySymlink
    case sourceOutputSame
    case outputExists
    case outputNotRegular
    case outputMetadataUnavailable
    case outputExceedsTarget
    case sourceChanged
    case unsupportedCodec
    case encodeFailed
    case deadlineExceeded
    case cancelled
}

public struct MediaCompressionRequest: Equatable {
    public let sourceURL: URL
    public let inputDirectoryURL: URL
    public let outputDirectoryURL: URL
    public let format: MediaCompressionFormat
    /// Lossy quality, in the closed interval 0...1. PNG ignores this value.
    public let quality: Double
    public let maxPixelDimension: Int?
    public let targetSizeBytes: Int64?

    public init(sourceURL: URL, inputDirectoryURL: URL, outputDirectoryURL: URL,
                format: MediaCompressionFormat, quality: Double = 0.8,
                maxPixelDimension: Int? = nil, targetSizeBytes: Int64? = nil) {
        self.sourceURL = sourceURL
        self.inputDirectoryURL = inputDirectoryURL
        self.outputDirectoryURL = outputDirectoryURL
        self.format = format
        self.quality = quality
        self.maxPixelDimension = maxPixelDimension
        self.targetSizeBytes = targetSizeBytes
    }
}

public enum MediaCompressionPhase: String, Codable, Equatable {
    case preparing
    case encoding
    case publishing
    case completed
    case cancelled
    case failed
}

public struct MediaCompressionEstimate: Equatable, Codable {
    public let sourceBytes: Int64
    public let estimatedOutputBytes: Int64
    public let estimatedSavedBytes: Int64
}

public struct MediaCompressionResult: Equatable, Codable {
    public let sourceURL: URL
    public let outputURL: URL
    public let sourceBytes: Int64
    public let outputBytes: Int64
    /// Nil until a real codec probe has produced an estimate. Formula-based
    /// savings are intentionally never reported.
    public let estimate: MediaCompressionEstimate?
    public let measuredSavedBytes: Int64
}

private struct MediaFileFingerprint: Equatable {
    let device: UInt64
    let inode: UInt64
    let size: Int64
    let modifiedSeconds: Int64
    let modifiedNanoseconds: Int64
    let changedSeconds: Int64
    let changedNanoseconds: Int64
}

private struct PinnedSource {
    let fd: Int32
    let fingerprint: MediaFileFingerprint
    let imageData: Data?
}

private struct PinnedDirectory {
    let fd: Int32
    let identity: MediaFileFingerprint
    let url: URL
    let name: String
}

private let dataLessFlag: UInt32 = 0x4000_0000
private let maxPixelDimensionBound = 16_384
private let maxVideoDurationSeconds = 4 * 60 * 60.0
private let maxEncodingNanoseconds: UInt64 = 30 * 60 * 1_000_000_000

/// One-shot bounded compression operation. The original is only read. Every
/// output is encoded in a UUID-owned temporary file then moved into place only
/// after successful encoding and metadata checks.
public final class MediaCompressionJob {
    public let request: MediaCompressionRequest

    private let lock = NSLock()
    private var cancelled = false
    private var activeExporter: AVAssetExportSession?
    private var phaseValue: MediaCompressionPhase = .preparing
    private var hasRun = false

    public init(request: MediaCompressionRequest) { self.request = request }

    public var phase: MediaCompressionPhase {
        lock.lock(); defer { lock.unlock() }
        return phaseValue
    }

    /// Cancellation only marks this job and asks its own AVFoundation export
    /// to stop. Cleanup is limited to this job's temporary path.
    public func cancel() {
        lock.lock()
        cancelled = true
        let exporter = activeExporter
        lock.unlock()
        exporter?.cancelExport()
    }

    public func run() async -> Result<MediaCompressionResult, MediaCompressionError> {
        lock.lock()
        guard !hasRun else {
            lock.unlock()
            return .failure(.invalidRequest)
        }
        hasRun = true
        lock.unlock()

        var temporaryName: String?
        var sourceCopyName: String?
        var entryIdentities: [String: MediaFileFingerprint] = [:]
        var privateDirectory: PinnedDirectory?
        do {
            let pinned = try openSource()
            defer { Darwin.close(pinned.fd) }
            try checkCancellation()
            if let target = request.targetSizeBytes, target >= pinned.fingerprint.size {
                throw MediaCompressionError.invalidRequest
            }
            let parent = try openOutputDirectory()
            defer { Darwin.close(parent.fd) }
            let token = UUID().uuidString
            let job = try makeJobDirectory(parent: parent, token: token)
            privateDirectory = job
            defer { Darwin.close(job.fd) }
            defer {
                cleanupJobDirectory(job, parent: parent,
                                    entries: [temporaryName, sourceCopyName].compactMap { $0 },
                                    expected: entryIdentities)
            }
            let tempName = "result.tmp"
            temporaryName = tempName
            let temporary = job.url.appendingPathComponent(tempName, isDirectory: false)
            let output = request.outputDirectoryURL.appendingPathComponent(
                "\(request.sourceURL.deletingPathExtension().lastPathComponent)-compressed-\(token).\(request.format.fileExtension)",
                isDirectory: false)
            let outputName = output.lastPathComponent
            guard !existsAt(parent.fd, name: outputName) else { throw MediaCompressionError.outputExists }

            setPhase(.encoding)
            if request.format.isVideo {
                let copyName = "source-copy.\(request.sourceURL.pathExtension)"
                sourceCopyName = copyName
                let sourceCopy = job.url.appendingPathComponent(copyName, isDirectory: false)
                try copyPinnedSource(pinned.fd, to: sourceCopy, parent: job)
                entryIdentities[copyName] = try outputFingerprint(sourceCopy)
                try await encodeVideo(sourceURL: sourceCopy, to: temporary)
            } else {
                guard let imageData = pinned.imageData else { throw MediaCompressionError.encodeFailed }
                try encodeImage(data: imageData, to: temporary, parent: job)
            }
            entryIdentities[tempName] = try outputFingerprint(temporary)
            try checkCancellation()
            guard pinned.fingerprint == (try fingerprint(fd: pinned.fd)) else {
                throw MediaCompressionError.sourceChanged
            }
            let temporaryFingerprint = try outputFingerprint(temporary)
            if let target = request.targetSizeBytes, temporaryFingerprint.size > target {
                throw MediaCompressionError.outputExceedsTarget
            }
            setPhase(.publishing)
            try checkCancellation()
            try publishNoReplace(name: tempName, parent: job,
                                 destinationName: outputName, destination: parent)
            let outputFingerprint = try outputFingerprint(parent: parent.fd, name: outputName)
            guard outputFingerprint.device != pinned.fingerprint.device
                    || outputFingerprint.inode != pinned.fingerprint.inode else {
                throw MediaCompressionError.outputMetadataUnavailable
            }
            // Cancellation or source mutation after publication is terminal;
            // retain our published file rather than deleting by pathname.
            if isCancelled() { throw MediaCompressionError.cancelled }
            guard pinned.fingerprint == (try fingerprint(fd: pinned.fd)) else {
                throw MediaCompressionError.sourceChanged
            }
            setPhase(.completed)
            return .success(MediaCompressionResult(
                sourceURL: request.sourceURL,
                outputURL: output,
                sourceBytes: pinned.fingerprint.size,
                outputBytes: outputFingerprint.size,
                estimate: nil,
                measuredSavedBytes: max(0, pinned.fingerprint.size - outputFingerprint.size)))
        } catch let error as MediaCompressionError {
            if let job = privateDirectory {
                for name in [temporaryName, sourceCopyName].compactMap({ $0 }) where entryIdentities[name] == nil {
                    if let st = try? statAt(job.fd, name: name), (st.st_mode & S_IFMT) == S_IFREG {
                        entryIdentities[name] = fingerprint(stat: st)
                    }
                }
            }
            setPhase(error == .cancelled ? .cancelled : .failed)
            return .failure(error)
        } catch {
            if let job = privateDirectory {
                for name in [temporaryName, sourceCopyName].compactMap({ $0 }) where entryIdentities[name] == nil {
                    if let st = try? statAt(job.fd, name: name), (st.st_mode & S_IFMT) == S_IFREG {
                        entryIdentities[name] = fingerprint(stat: st)
                    }
                }
            }
            setPhase(.failed)
            return .failure(.encodeFailed)
        }
    }

    private func openSource() throws -> PinnedSource {
        try validateRequestPaths()
        let parentURL = request.sourceURL.deletingLastPathComponent()
        let parentFD = try openDirectoryNoFollow(parentURL)
        defer { Darwin.close(parentFD) }
        let name = request.sourceURL.lastPathComponent
        let fd = try openFileNoFollow(parentFD, name: name)
        var keepFD = false
        defer { if !keepFD { Darwin.close(fd) } }
        let st = try fileStat(fd)
        guard (st.st_mode & S_IFMT) == S_IFREG else { throw MediaCompressionError.sourceNotRegular }
        guard st.st_size > 0 else { throw MediaCompressionError.sourceMissing }
        guard (st.st_flags & dataLessFlag) == 0 else { throw MediaCompressionError.sourcePlaceholder }
        let values = try? request.sourceURL.resourceValues(forKeys: [
            .isUbiquitousItemKey, .ubiquitousItemDownloadingStatusKey, .fileResourceIdentifierKey
        ])
        if values?.isUbiquitousItem == true {
            guard values?.ubiquitousItemDownloadingStatus == .current else {
                throw MediaCompressionError.sourcePlaceholder
            }
        }
        guard values?.fileResourceIdentifier != nil, st.st_dev != 0, st.st_ino != 0 else {
            throw MediaCompressionError.sourceIdentityUnavailable
        }
        let kindIsVideo = request.format.isVideo
        guard kindIsVideo || request.format.imageUTI != nil else {
            throw MediaCompressionError.unsupportedCodec
        }
        let data = kindIsVideo ? nil : try readAll(fd: fd)
        if let data { try validateImageDimensions(data) }
        keepFD = true
        return PinnedSource(fd: fd, fingerprint: fingerprint(stat: st), imageData: data)
    }

    private func openOutputDirectory() throws -> PinnedDirectory {
        let fd: Int32
        do {
            fd = try openDirectoryNoFollow(request.outputDirectoryURL)
        } catch MediaCompressionError.sourceMissing {
            throw MediaCompressionError.outputDirectoryMissing
        } catch MediaCompressionError.unsafeAncestor {
            throw MediaCompressionError.outputDirectorySymlink
        }
        let st = try fileStat(fd)
        guard (st.st_mode & S_IFMT) == S_IFDIR else {
            Darwin.close(fd)
            throw MediaCompressionError.outputDirectoryMissing
        }
        return PinnedDirectory(fd: fd, identity: fingerprint(stat: st),
                               url: request.outputDirectoryURL, name: request.outputDirectoryURL.lastPathComponent)
    }

    private func validateRequestPaths() throws {
        guard absoluteClean(request.sourceURL), absoluteClean(request.inputDirectoryURL),
              absoluteClean(request.outputDirectoryURL) else { throw MediaCompressionError.invalidRequest }
        guard pathIsInside(request.sourceURL, directory: request.inputDirectoryURL) else {
            throw MediaCompressionError.sourceOutsideInputFolder
        }
        guard request.inputDirectoryURL.path != request.outputDirectoryURL.path else {
            throw MediaCompressionError.sourceOutputSame
        }
        guard request.quality.isFinite, (0...1).contains(request.quality) else {
            throw MediaCompressionError.invalidRequest
        }
        if let dimension = request.maxPixelDimension, dimension <= 0 { throw MediaCompressionError.invalidRequest }
        if let dimension = request.maxPixelDimension, dimension > maxPixelDimensionBound {
            throw MediaCompressionError.invalidRequest
        }
        if let target = request.targetSizeBytes, target <= 0 { throw MediaCompressionError.invalidRequest }
        try inspectAncestors(of: request.sourceURL, includeLeaf: false)
        try inspectAncestors(of: request.inputDirectoryURL)
    }

    private func inspectAncestors(of url: URL, includeLeaf: Bool = true) throws {
        let components = url.path.split(separator: "/", omittingEmptySubsequences: true).map(String.init)
        var current = URL(fileURLWithPath: "/", isDirectory: true)
        for (index, component) in components.enumerated() {
            guard component != ".", component != ".." else { throw MediaCompressionError.unsafeAncestor }
            current.appendPathComponent(component, isDirectory: false)
            if !includeLeaf && index == components.count - 1 { break }
            let st = try lstat(current)
            if (st.st_mode & S_IFMT) == S_IFLNK { throw MediaCompressionError.unsafeAncestor }
            if current.path != url.path && (st.st_mode & S_IFMT) != S_IFDIR {
                throw MediaCompressionError.unsafeAncestor
            }
        }
    }

    private func validateImageDimensions(_ data: Data) throws {
        guard let source = CGImageSourceCreateWithData(data as CFData, nil),
              let properties = CGImageSourceCopyPropertiesAtIndex(source, 0, nil) as? NSDictionary,
              let width = (properties[kCGImagePropertyPixelWidth] as? NSNumber)?.int64Value,
              let height = (properties[kCGImagePropertyPixelHeight] as? NSNumber)?.int64Value,
              width > 0, height > 0,
              width <= 100_000, height <= 100_000,
              width.multipliedReportingOverflow(by: height).overflow == false,
              width * height <= 100_000_000 else {
            throw MediaCompressionError.encodeFailed
        }
    }

    private func encodeImage(data: Data, to url: URL, parent: PinnedDirectory) throws {
        try checkCancellation()
        guard let source = CGImageSourceCreateWithData(data as CFData, nil) else {
            throw MediaCompressionError.encodeFailed
        }
        let image: CGImage
        if let maxDimension = request.maxPixelDimension {
            let options: [CFString: Any] = [
                kCGImageSourceCreateThumbnailFromImageAlways: true,
                kCGImageSourceCreateThumbnailWithTransform: true,
                kCGImageSourceThumbnailMaxPixelSize: maxDimension
            ]
            guard let thumbnail = CGImageSourceCreateThumbnailAtIndex(source, 0, options as CFDictionary) else {
                throw MediaCompressionError.encodeFailed
            }
            image = thumbnail
        } else {
            guard let original = CGImageSourceCreateImageAtIndex(source, 0, nil) else {
                throw MediaCompressionError.encodeFailed
            }
            image = original
        }
        guard let uti = request.format.imageUTI else {
            throw MediaCompressionError.unsupportedCodec
        }
        let encoded = NSMutableData()
        guard let destination = CGImageDestinationCreateWithData(encoded as CFMutableData, uti as CFString, 1, nil) else {
            throw MediaCompressionError.unsupportedCodec
        }
        var properties: [CFString: Any] = [:]
        if request.format != .png {
            properties[kCGImageDestinationLossyCompressionQuality] = request.quality
        }
        CGImageDestinationAddImage(destination, image, properties as CFDictionary)
        guard CGImageDestinationFinalize(destination) else { throw MediaCompressionError.encodeFailed }
        try writePrivateData(Data(bytes: encoded.bytes, count: encoded.length), name: url.lastPathComponent, parent: parent)
    }

    private func encodeVideo(sourceURL: URL, to url: URL) async throws {
        try checkCancellation()
        guard let fileType = request.format.videoFileType else { throw MediaCompressionError.unsupportedCodec }
        let asset = AVURLAsset(url: sourceURL)
        let duration = asset.duration.seconds
        guard duration.isFinite, duration > 0, duration <= maxVideoDurationSeconds else {
            throw MediaCompressionError.encodeFailed
        }
        guard let preset = exportPreset() else { throw MediaCompressionError.unsupportedCodec }
        guard let exporter = AVAssetExportSession(asset: asset, presetName: preset) else {
            throw MediaCompressionError.unsupportedCodec
        }
        guard exporter.supportedFileTypes.contains(fileType) else {
            throw MediaCompressionError.unsupportedCodec
        }
        exporter.outputURL = url
        exporter.outputFileType = fileType
        exporter.shouldOptimizeForNetworkUse = true
        lock.lock(); activeExporter = exporter; lock.unlock()
        do {
            try await withThrowingTaskGroup(of: Void.self) { group in
                group.addTask {
                    await withCheckedContinuation { (continuation: CheckedContinuation<Void, Never>) in
                        exporter.exportAsynchronously { continuation.resume() }
                    }
                }
                group.addTask {
                    try await Task.sleep(nanoseconds: maxEncodingNanoseconds)
                    throw MediaCompressionError.deadlineExceeded
                }
                _ = try await group.next()
                group.cancelAll()
            }
        } catch {
            exporter.cancelExport()
            lock.lock(); activeExporter = nil; lock.unlock()
            if let error = error as? MediaCompressionError { throw error }
            throw MediaCompressionError.encodeFailed
        }
        lock.lock(); activeExporter = nil; lock.unlock()
        if exporter.status == .cancelled || isCancelled() { throw MediaCompressionError.cancelled }
        guard exporter.status == .completed else { throw MediaCompressionError.encodeFailed }
    }

    private func exportPreset() -> String? {
        if let max = request.maxPixelDimension {
            // AVAssetExportSession presets are fixed dimensions and may
            // exceed requested max dimensions. Refuse those requests rather
            // than silently violating resize semantics.
            if max < 1920 { return nil }
            return AVAssetExportPreset1920x1080
        }
        if request.quality < 0.4 { return AVAssetExportPresetLowQuality }
        if request.quality < 0.75 { return AVAssetExportPresetMediumQuality }
        return AVAssetExportPresetHighestQuality
    }

    private func outputFingerprint(_ url: URL) throws -> MediaFileFingerprint {
        let st = try lstat(url)
        return try outputFingerprint(stat: st)
    }

    private func outputFingerprint(parent: Int32, name: String) throws -> MediaFileFingerprint {
        let st = try statAt(parent, name: name)
        return try outputFingerprint(stat: st)
    }

    private func outputFingerprint(stat st: stat) throws -> MediaFileFingerprint {
        guard (st.st_mode & S_IFMT) != S_IFLNK else { throw MediaCompressionError.outputNotRegular }
        guard (st.st_mode & S_IFMT) == S_IFREG else { throw MediaCompressionError.outputNotRegular }
        guard st.st_size > 0, st.st_dev != 0, st.st_ino != 0 else {
            throw MediaCompressionError.outputMetadataUnavailable
        }
        return fingerprint(stat: st)
    }

    private func fingerprint(fd: Int32) throws -> MediaFileFingerprint {
        let st = try fileStat(fd)
        guard (st.st_mode & S_IFMT) == S_IFREG, st.st_dev != 0, st.st_ino != 0,
              (st.st_flags & dataLessFlag) == 0 else {
            throw MediaCompressionError.sourceChanged
        }
        return fingerprint(stat: st)
    }

    private func fingerprint(stat st: stat) -> MediaFileFingerprint {
        MediaFileFingerprint(device: UInt64(st.st_dev), inode: UInt64(st.st_ino), size: st.st_size,
                             modifiedSeconds: Int64(st.st_mtimespec.tv_sec),
                             modifiedNanoseconds: Int64(st.st_mtimespec.tv_nsec),
                             changedSeconds: Int64(st.st_ctimespec.tv_sec),
                             changedNanoseconds: Int64(st.st_ctimespec.tv_nsec))
    }

    private func lstat(_ url: URL) throws -> stat {
        var value = stat()
        guard Darwin.lstat(url.path, &value) == 0 else {
            if errno == ENOENT { throw MediaCompressionError.sourceMissing }
            throw MediaCompressionError.unsafeAncestor
        }
        return value
    }

    private func openDirectoryNoFollow(_ url: URL) throws -> Int32 {
        guard absoluteClean(url) else { throw MediaCompressionError.invalidRequest }
        var fd = Darwin.open("/", O_RDONLY | O_DIRECTORY | O_NOFOLLOW | O_CLOEXEC)
        guard fd >= 0 else { throw MediaCompressionError.sourceMissing }
        let components = url.path.split(separator: "/", omittingEmptySubsequences: true).map(String.init)
        for component in components {
            let next = component.withCString { Darwin.openat(fd, $0, O_RDONLY | O_DIRECTORY | O_NOFOLLOW | O_CLOEXEC) }
            guard next >= 0 else {
                let err = errno
                Darwin.close(fd)
                throw err == ELOOP ? MediaCompressionError.unsafeAncestor : MediaCompressionError.sourceMissing
            }
            Darwin.close(fd)
            fd = next
        }
        return fd
    }

    private func openFileNoFollow(_ parent: Int32, name: String) throws -> Int32 {
        let fd = name.withCString { Darwin.openat(parent, $0, O_RDONLY | O_NOFOLLOW | O_CLOEXEC) }
        guard fd >= 0 else {
            if errno == ELOOP { throw MediaCompressionError.sourceSymlink }
            throw MediaCompressionError.sourceMissing
        }
        return fd
    }

    private func fileStat(_ fd: Int32) throws -> stat {
        var value = stat()
        guard Darwin.fstat(fd, &value) == 0 else { throw MediaCompressionError.sourceChanged }
        return value
    }

    private func statAt(_ parent: Int32, name: String) throws -> stat {
        var value = stat()
        let result = name.withCString { Darwin.fstatat(parent, $0, &value, AT_SYMLINK_NOFOLLOW) }
        guard result == 0 else { throw MediaCompressionError.outputMetadataUnavailable }
        return value
    }

    private func existsAt(_ parent: Int32, name: String) -> Bool {
        (try? statAt(parent, name: name)) != nil
    }

    private func readAll(fd: Int32) throws -> Data {
        guard let st = try? fileStat(fd), st.st_size <= 256 * 1024 * 1024 else {
            throw MediaCompressionError.encodeFailed
        }
        guard Darwin.lseek(fd, 0, SEEK_SET) >= 0 else { throw MediaCompressionError.encodeFailed }
        var data = Data(capacity: Int(st.st_size))
        var buffer = [UInt8](repeating: 0, count: 1024 * 1024)
        while true {
            let count = Darwin.read(fd, &buffer, buffer.count)
            if count < 0 {
                if errno == EINTR { continue }
                throw MediaCompressionError.encodeFailed
            }
            if count == 0 { break }
            data.append(buffer, count: count)
        }
        return data
    }

    private func writePrivateData(_ data: Data, name: String, parent: PinnedDirectory) throws {
        let fd = name.withCString {
            Darwin.openat(parent.fd, $0, O_WRONLY | O_CREAT | O_EXCL | O_NOFOLLOW | O_CLOEXEC, 0o600)
        }
        guard fd >= 0 else { throw MediaCompressionError.outputExists }
        defer { Darwin.close(fd) }
        try data.withUnsafeBytes { bytes in
            guard let base = bytes.baseAddress else { throw MediaCompressionError.encodeFailed }
            var written = 0
            while written < bytes.count {
                let count = Darwin.write(fd, base.advanced(by: written), bytes.count - written)
                if count <= 0 { throw MediaCompressionError.encodeFailed }
                written += count
            }
        }
        guard Darwin.fsync(fd) == 0 else { throw MediaCompressionError.encodeFailed }
    }

    private func copyPinnedSource(_ sourceFD: Int32, to url: URL, parent: PinnedDirectory) throws {
        guard let st = try? fileStat(sourceFD), st.st_size <= 2 * 1024 * 1024 * 1024 else {
            throw MediaCompressionError.encodeFailed
        }
        guard Darwin.lseek(sourceFD, 0, SEEK_SET) >= 0 else { throw MediaCompressionError.encodeFailed }
        let fd = url.lastPathComponent.withCString {
            Darwin.openat(parent.fd, $0, O_WRONLY | O_CREAT | O_EXCL | O_NOFOLLOW | O_CLOEXEC, 0o600)
        }
        guard fd >= 0 else { throw MediaCompressionError.outputExists }
        defer { Darwin.close(fd) }
        var buffer = [UInt8](repeating: 0, count: 1024 * 1024)
        while true {
            let count = Darwin.read(sourceFD, &buffer, buffer.count)
            if count < 0 {
                if errno == EINTR { continue }
                throw MediaCompressionError.encodeFailed
            }
            if count == 0 { break }
            var written = 0
            while written < count {
                let result = Darwin.write(fd, buffer.withUnsafeBytes { $0.baseAddress!.advanced(by: written) }, count - written)
                if result <= 0 { throw MediaCompressionError.encodeFailed }
                written += result
            }
        }
        guard Darwin.fsync(fd) == 0 else { throw MediaCompressionError.encodeFailed }
    }

    private func makeJobDirectory(parent: PinnedDirectory, token: String) throws -> PinnedDirectory {
        let name = ".pulse-compress-\(token)"
        let made = name.withCString { Darwin.mkdirat(parent.fd, $0, 0o700) }
        guard made == 0 else { throw MediaCompressionError.outputExists }
        let fd = try openDirectoryAt(parent.fd, name: name)
        let st = try fileStat(fd)
        return PinnedDirectory(fd: fd, identity: fingerprint(stat: st),
                               url: parent.url.appendingPathComponent(name, isDirectory: true), name: name)
    }

    private func openDirectoryAt(_ parent: Int32, name: String) throws -> Int32 {
        let fd = name.withCString { Darwin.openat(parent, $0, O_RDONLY | O_DIRECTORY | O_NOFOLLOW | O_CLOEXEC) }
        guard fd >= 0 else { throw MediaCompressionError.unsafeAncestor }
        return fd
    }

    private func publishNoReplace(name: String, parent: PinnedDirectory,
                                  destinationName: String, destination: PinnedDirectory) throws {
        let result = name.withCString { sourceName in
            destinationName.withCString { targetName in
                Darwin.linkat(parent.fd, sourceName, destination.fd, targetName, 0)
            }
        }
        guard result == 0 else {
            if errno == EEXIST { throw MediaCompressionError.outputExists }
            throw MediaCompressionError.outputMetadataUnavailable
        }
        let before = try statAt(parent.fd, name: name)
        let published = try statAt(destination.fd, name: destinationName)
        guard before.st_dev == published.st_dev, before.st_ino == published.st_ino else {
            throw MediaCompressionError.outputMetadataUnavailable
        }
        let removed = name.withCString { Darwin.unlinkat(parent.fd, $0, 0) }
        guard removed == 0 else { throw MediaCompressionError.outputMetadataUnavailable }
    }

    private func cleanupJobDirectory(_ job: PinnedDirectory, parent: PinnedDirectory,
                                    entries: [String], expected: [String: MediaFileFingerprint]) {
        for name in entries {
            guard let st = try? statAt(job.fd, name: name),
                  (st.st_mode & S_IFMT) == S_IFREG,
                  let expectedIdentity = expected[name],
                  fingerprint(stat: st) == expectedIdentity else { continue }
            _ = name.withCString { Darwin.unlinkat(job.fd, $0, 0) }
        }
        guard let current = try? statAt(parent.fd, name: job.name),
              UInt64(current.st_dev) == job.identity.device,
              UInt64(current.st_ino) == job.identity.inode else { return }
        _ = job.name.withCString { Darwin.unlinkat(parent.fd, $0, AT_REMOVEDIR) }
    }

    private func absoluteClean(_ url: URL) -> Bool {
        guard url.isFileURL, url.path.hasPrefix("/") else { return false }
        return !url.pathComponents.contains(".") && !url.pathComponents.contains("..")
    }

    private func pathIsInside(_ path: URL, directory: URL) -> Bool {
        let base = directory.path.hasSuffix("/") ? directory.path : directory.path + "/"
        return path.path == directory.path || path.path.hasPrefix(base)
    }

    private func checkCancellation() throws {
        if isCancelled() { throw MediaCompressionError.cancelled }
    }

    private func isCancelled() -> Bool {
        lock.lock(); defer { lock.unlock() }
        return cancelled
    }

    private func setPhase(_ phase: MediaCompressionPhase) {
        lock.lock(); phaseValue = phase; lock.unlock()
    }
}
