import Foundation
#if canImport(AppKit) && canImport(WebKit)
import AppKit
import WebKit
import UniformTypeIdentifiers
import Quartz
#endif

// On-demand dashboard host: an AppKit window hosting the bundled static dashboard
// (Resources/dashboard/index.html). "Scan folder" opens a native NSOpenPanel, runs the
// bundled read-only CLI at Contents/Helpers/cockpit with `scan <root> --save --json`,
// parses stdout and injects the object into window.CockpitDashboard.importScan().
// No PATH lookup, no shell, no web-supplied paths, no timers.

// MARK: - Configuration

public struct DashboardHostConfiguration: Sendable {
    /// Directory containing index.html, app.mjs and style.css.
    public var dashboardDirectory: URL
    /// Bundled CLI executable (Contents/Helpers/cockpit). nil renders the host's
    /// "helper unavailable" error in the window instead of fake data.
    public var helperURL: URL?
    public var deadline: TimeInterval = 120
    /// stdout cap — the scan report envelope. Truncated output is surfaced, never parsed.
    public var stdoutLimit = 32 * 1024 * 1024
    /// stderr cap retained for diagnostics.
    public var stderrLimit = 1 * 1024 * 1024
    /// JS string bound for a single evaluateJavaScript call.
    public var requestLimit = 32 * 1024 * 1024

    public init(dashboardDirectory: URL, helperURL: URL?) {
        self.dashboardDirectory = dashboardDirectory.standardizedFileURL
        self.helperURL = helperURL
    }

    public var indexURL: URL { dashboardDirectory.appendingPathComponent("index.html") }

    /// Default locations inside an .app bundle: executable at Contents/MacOS/<name>,
    /// CLI at Contents/Helpers/cockpit, assets at Contents/Resources/dashboard/.
    public static func bundleDefault(_ bundle: Bundle = .main) -> DashboardHostConfiguration {
        let contents = bundle.executableURL?
            .deletingLastPathComponent().deletingLastPathComponent()
        let helper = contents?.appendingPathComponent("Helpers/cockpit")
        let dashboard = bundle.url(forResource: "index", withExtension: "html", subdirectory: "dashboard")
            .map { $0.deletingLastPathComponent() }
            ?? contents?.appendingPathComponent("Resources/dashboard")
            ?? bundle.resourceURL ?? bundle.bundleURL
        let helperExists = helper.map { FileManager.default.isExecutableFile(atPath: $0.path) } ?? false
        return DashboardHostConfiguration(dashboardDirectory: dashboard, helperURL: helperExists ? helper : nil)
    }
}

// MARK: - Scan request & outcome

public struct ScanRequest: Sendable, Equatable {
    public let executable: URL
    /// argv only — never routed through a shell.
    public let arguments: [String]
    public let deadline: TimeInterval
    public let stdoutLimit: Int
    public let stderrLimit: Int

    public static func scan(helper: URL, root: URL, deadline: TimeInterval, stdoutLimit: Int, stderrLimit: Int) -> ScanRequest {
        ScanRequest(executable: helper,
                    arguments: ["scan", root.path, "--save", "--json"],
                    deadline: deadline, stdoutLimit: stdoutLimit, stderrLimit: stderrLimit)
    }
}

public struct ScanOutcome: Sendable, Equatable {
    public let stdout: Data
    /// Trailing stderr (bounded), kept for the error surface.
    public let stderr: String
    public let status: Int32
    public let truncated: Bool
}

public enum ScanFailure: Error, Equatable {
    case helperMissing
    case launchFailed(String)
    case timedOut
    case nonZeroExit(Int32, String)
    case outputTruncated
    case invalidJSON(String)
    /// Scan ran but produced no scan report the dashboard can import.
    case emptyReport
}

public protocol ScanRunning: Sendable {
    func run(_ request: ScanRequest) async throws -> ScanOutcome
    func cancel()
}

// MARK: - Process runner (no shell, bounded pipes, terminate→kill→verify)

public final class ProcessScanRunner: ScanRunning, @unchecked Sendable {
    private let lock = NSLock()
    private var current: Process?
    private var cancelGeneration = 0
    private let executionQueue = DispatchQueue(label: "cockpit.dashboard.scan")

    public init() {}

    public func cancel() {
        lock.lock()
        cancelGeneration += 1
        let process = current
        lock.unlock()
        guard let process, process.isRunning else { return }
        process.terminate()
        DispatchQueue.global().asyncAfter(deadline: .now() + 2) {
            if process.isRunning { kill(process.processIdentifier, SIGKILL) }
        }
    }

    public func run(_ request: ScanRequest) async throws -> ScanOutcome {
        try Task.checkCancellation()
        lock.lock()
        let generation = cancelGeneration
        lock.unlock()
        return try await withTaskCancellationHandler {
            try await withCheckedThrowingContinuation { continuation in
                executionQueue.async { continuation.resume(with: .init { try self.execute(request, generation: generation) }) }
            }
        } onCancel: { self.cancel() }
    }

    private func execute(_ request: ScanRequest, generation: Int) throws -> ScanOutcome {
        let process = Process()
        process.executableURL = request.executable
        process.arguments = request.arguments
        let outPipe = Pipe(), errPipe = Pipe()
        process.standardOutput = outPipe
        process.standardError = errPipe

        let state = BoundedPipes(outLimit: request.stdoutLimit, errLimit: request.stderrLimit)
        outPipe.fileHandleForReading.readabilityHandler = { handle in
            state.append(stdout: handle.availableData)
        }
        errPipe.fileHandleForReading.readabilityHandler = { handle in
            state.append(stderr: handle.availableData)
        }

        lock.lock()
        guard generation == cancelGeneration else {
            lock.unlock()
            throw CancellationError()
        }
        do { try process.run() } catch {
            lock.unlock()
            throw ScanFailure.launchFailed(error.localizedDescription)
        }
        current = process
        lock.unlock()
        defer {
            lock.lock()
            current = nil
            lock.unlock()
        }
        var timedOut = false
        let deadline = ProcessInfo.processInfo.systemUptime + request.deadline
        while process.isRunning {
            if ProcessInfo.processInfo.systemUptime > deadline {
                timedOut = true
                process.terminate()
                // Verify termination; escalate to SIGKILL so no orphan child survives.
                let killDeadline = ProcessInfo.processInfo.systemUptime + 2
                while process.isRunning && ProcessInfo.processInfo.systemUptime < killDeadline { Thread.sleep(forTimeInterval: 0.01) }
                if process.isRunning { kill(process.processIdentifier, SIGKILL) }
                process.waitUntilExit()
                break
            }
            Thread.sleep(forTimeInterval: 0.01)
        }
        if !timedOut { process.waitUntilExit() }

        outPipe.fileHandleForReading.readabilityHandler = nil
        errPipe.fileHandleForReading.readabilityHandler = nil
        state.append(stdout: outPipe.fileHandleForReading.readDataToEndOfFile())
        state.append(stderr: errPipe.fileHandleForReading.readDataToEndOfFile())
        let (stdout, stderr) = state.result()

        if timedOut { throw ScanFailure.timedOut }
        if process.terminationStatus != 0 {
            throw ScanFailure.nonZeroExit(process.terminationStatus, String(stderr.suffix(500)))
        }
        return ScanOutcome(stdout: stdout, stderr: String(stderr.suffix(500)),
                           status: process.terminationStatus, truncated: state.truncated)
    }
}

/// Accumulates bounded stdout/stderr from pipe handlers; drops (but counts) overflow.
final class BoundedPipes: @unchecked Sendable {
    private let lock = NSLock()
    private var out = Data()
    private var err = Data()
    private(set) var truncated = false
    let outLimit: Int
    let errLimit: Int

    init(outLimit: Int, errLimit: Int) {
        self.outLimit = outLimit
        self.errLimit = errLimit
    }

    func append(stdout chunk: Data) {
        lock.lock()
        defer { lock.unlock() }
        append(&out, chunk: chunk, limit: outLimit)
    }
    func append(stderr chunk: Data) {
        lock.lock()
        defer { lock.unlock() }
        append(&err, chunk: chunk, limit: errLimit)
    }

    private func append(_ buffer: inout Data, chunk: Data, limit: Int) {
        guard !chunk.isEmpty else { return }
        let room = limit - buffer.count
        if room <= 0 { truncated = true; return }
        if chunk.count > room { truncated = true }
        buffer.append(chunk.prefix(room))
    }

    func result() -> (Data, String) {
        lock.lock()
        defer { lock.unlock() }
        return (out, String(decoding: err, as: UTF8.self))
    }
}

// MARK: - Injection & navigation policy (pure, unit-testable)

public enum DashboardInjection {
    /// Serializes the scan object and embeds it as a JS literal. `<`, U+2028 and U+2029
    /// are escaped so the literal can't break out of evaluateJavaScript context.
    public static func importJavaScript(for object: Any) throws -> String {
        guard JSONSerialization.isValidJSONObject(object) else {
            throw ScanFailure.invalidJSON("scan output is not a JSON object")
        }
        let data = try JSONSerialization.data(withJSONObject: object)
        let literal = String(decoding: data, as: UTF8.self)
            .replacingOccurrences(of: "<", with: "\\u003c")
            .replacingOccurrences(of: "\u{2028}", with: "\\u2028")
            .replacingOccurrences(of: "\u{2029}", with: "\\u2029")
        return "(function(){var s=\(literal);var d=window.CockpitDashboard;"
            + "if(d&&typeof d.importScan===\"function\"){d.importScan(s);return true;}"
            + "throw new Error(\"CockpitDashboard.importScan unavailable\");})();"
    }

    public static func errorJavaScript(_ message: String) -> String {
        let escaped = message.replacingOccurrences(of: "\\", with: "\\\\")
            .replacingOccurrences(of: "\"", with: "\\\"")
            .replacingOccurrences(of: "<", with: "\\u003c")
            .replacingOccurrences(of: "\n", with: "\\n")
            .replacingOccurrences(of: "\r", with: "\\r")
            .replacingOccurrences(of: "\u{2028}", with: "\\u2028")
            .replacingOccurrences(of: "\u{2029}", with: "\\u2029")
        return "(function(){var c=document.getElementById(\"scan-status\");"
            + "if(c){c.textContent=\(escapedAsLiteral(escaped));c.setAttribute(\"data-tone\",\"bad\");}return true;})();"
    }

    private static func escapedAsLiteral(_ s: String) -> String { "\"\(s)\"" }

    /// Parses CLI stdout into the envelope the dashboard importer accepts
    /// ({snapshot:{report:…}} — unwrapInput already handles the wrapper).
    public static func scanObject(from stdout: Data, truncated: Bool) throws -> Any {
        if truncated { throw ScanFailure.outputTruncated }
        if stdout.isEmpty { throw ScanFailure.emptyReport }
        let object = try JSONSerialization.jsonObject(with: stdout)
        guard let dict = object as? [String: Any],
              let snapshot = dict["snapshot"] as? [String: Any],
              let report = snapshot["report"] as? [String: Any], !report.isEmpty else {
            throw ScanFailure.invalidJSON("expected {snapshot:{report:…}}")
        }
        return dict
    }
}

public enum DashboardNavigation {
    /// Only file: URLs at or under the dashboard directory may load. about:blank is
    /// permitted so the host can render its own error surface when assets are missing.
    public static func isAllowed(_ url: URL, dashboardRoot: URL) -> Bool {
        if url.absoluteString == "about:blank" { return true }
        guard url.isFileURL else { return false }
        var root = dashboardRoot.standardizedFileURL.path
        if !root.hasSuffix("/") { root += "/" }
        let path = url.standardizedFileURL.path
        return path.hasPrefix(root)
    }

    /// Allowlisted script-message names the page may send. Anything else is dropped.
    public static let allowedScriptMessages: Set<String> = ["cockpitStatus", "cockpitAction"]
}

// MARK: - Scan coordinator (AppKit-free test seam)

/// Runs the scan pipeline independent of AppKit: request build → runner → parse → JS.
public struct ScanCoordinator: Sendable {
    public let configuration: DashboardHostConfiguration
    public let runner: any ScanRunning

    public init(configuration: DashboardHostConfiguration, runner: any ScanRunning) {
        self.configuration = configuration
        self.runner = runner
    }

    public func commandObject(arguments: [String]) async throws -> [String: Any] {
        guard let helper = configuration.helperURL else { throw ScanFailure.helperMissing }
        let request = ScanRequest(executable: helper, arguments: arguments,
                                  deadline: configuration.deadline,
                                  stdoutLimit: configuration.stdoutLimit,
                                  stderrLimit: configuration.stderrLimit)
        let outcome = try await runner.run(request)
        guard !outcome.truncated else { throw ScanFailure.outputTruncated }
        guard let object = try JSONSerialization.jsonObject(with: outcome.stdout) as? [String: Any] else {
            throw ScanFailure.invalidJSON("Expected a JSON object")
        }
        return object
    }

    public func scanExport(root: URL, stateDirectory: URL) async throws -> [String: Any] {
        var arguments = ["scan", root.path, "--save", "--state-dir", stateDirectory.path, "--json"]
        let canonicalRoot = root.resolvingSymlinksInPath().standardizedFileURL.path
        let canonicalState = stateDirectory.resolvingSymlinksInPath().standardizedFileURL.path
        if canonicalState == canonicalRoot || canonicalState.hasPrefix(canonicalRoot == "/" ? "/" : canonicalRoot + "/") {
            arguments += ["--exclude-state", stateDirectory.path]
        }
        let object = try await commandObject(arguments: arguments)
        guard let snapshot = object["snapshot"] as? [String: Any], let id = snapshot["id"] as? String else {
            throw ScanFailure.emptyReport
        }
        return try await commandObject(arguments: ["export", id, "--state-dir", stateDirectory.path, "--json"])
    }

    /// Full scan for a user-selected root; throws ScanFailure on any bound violation.
    public func importScript(for root: URL) async throws -> String {
        guard let helper = configuration.helperURL else { throw ScanFailure.helperMissing }
        let request = ScanRequest.scan(helper: helper, root: root,
                                       deadline: configuration.deadline,
                                       stdoutLimit: configuration.stdoutLimit,
                                       stderrLimit: configuration.stderrLimit)
        let outcome = try await runner.run(request)
        let object = try DashboardInjection.scanObject(from: outcome.stdout, truncated: outcome.truncated)
        return try DashboardInjection.importJavaScript(for: object)
    }
}

// MARK: - AppKit host

#if canImport(AppKit) && canImport(WebKit)
@MainActor
public final class DashboardHost: NSObject {
    public let coordinator: ScanCoordinator
    private let configuration: DashboardHostConfiguration
    private var window: NSWindow?
    private var webView: WKWebView?
    private var scanTask: Task<Void, Never>?
    private var actionTask: Task<Void, Never>?
    private var activeAction: String?
    private var activeRequestID: String?
    private var busy = false
    private let stateDirectory: URL
    private let nativeServices: NativeStorageServices
    private let cleanupService: NativeCleanupService
    private let applicationDetails: NativeApplicationDetails
    private let applicationInspection: NativeApplicationInspection
    private let applicationUpdates = NativeApplicationUpdates()
    private let filenameIndex: NativeFilenameIndex
    private var trustedApplications: [String: String] = [:]
    private var reviewedApplications: [String: (path: String, bundleID: String)] = [:]
    private var trustedDuplicateReport: [String: Any]?
    private var reviewedDuplicates: [String: NativeDuplicateRevalidation] = [:]
    private var trustedSnapshotID: String?
    private var trustedRoot: URL?
    private var trustedEntries: [String: [String: Any]] = [:]
    private var trustedCleanupPaths: Set<String> = []
    private var previewURL: URL?
    private var restoreEnabled = true


    public init(configuration: DashboardHostConfiguration = .bundleDefault(),
                runner: any ScanRunning = ProcessScanRunner(), stateDirectory: URL? = nil) {
        let state = stateDirectory ?? FileManager.default.homeDirectoryForCurrentUser.resolvingSymlinksInPath()
            .appendingPathComponent("Library/Application Support/Cockpit/Storage", isDirectory: true)
        self.stateDirectory = state.standardizedFileURL
        self.nativeServices = NativeStorageServices(stateDirectory: state)
        self.cleanupService = NativeCleanupService(stateDirectory: state)
        self.applicationDetails = NativeApplicationDetails(stateDirectory: state)
        self.applicationInspection = NativeApplicationInspection(stateDirectory: state)
        self.filenameIndex = NativeFilenameIndex(stateDirectory: state)
        self.configuration = configuration
        self.coordinator = ScanCoordinator(configuration: configuration, runner: runner)
    }

    public func show() {
        if window == nil { buildWindow() }
        window?.makeKeyAndOrderFront(nil)
        NSApp.activate(ignoringOtherApps: true)
    }

    public func updateResources(_ reading: [String: Any]) {
        nativeServices.updateResources(reading)
    }

    /// Hosted package smoke: verify bundled page scripts & real scanner exchange.
    public func verifyBundledScan(root: URL) async throws {
        restoreEnabled = false
        show()
        guard let webView else { throw ScanFailure.emptyReport }
        var ready = false
        for _ in 0..<150 {
            if let loaded = try? await webView.evaluateJavaScript("typeof window.CockpitDashboard === 'object'"),
               (loaded as? Bool) == true { ready = true; break }
            try await Task.sleep(nanoseconds: 100_000_000)
        }
        guard ready else { throw ScanFailure.invalidJSON("Bundled dashboard did not become ready") }
        let temporaryRoot = ProcessInfo.processInfo.environment["RUNNER_TEMP"]
            .map { URL(fileURLWithPath: $0, isDirectory: true) } ?? FileManager.default.temporaryDirectory
        let smokeState = temporaryRoot.resolvingSymlinksInPath()
            .appendingPathComponent("cockpit-dashboard-state-" + UUID().uuidString, isDirectory: true)
        try FileManager.default.createDirectory(at: smokeState, withIntermediateDirectories: false,
                                               attributes: [.posixPermissions: 0o700])
        defer { try? FileManager.default.removeItem(at: smokeState) }
        let object = try await coordinator.scanExport(root: root, stateDirectory: smokeState)
        acceptNativeScan(object)
        _ = try await webView.evaluateJavaScript(DashboardInjection.importJavaScript(for: object))
        let imported = try await webView.evaluateJavaScript("window.CockpitDashboard.getState().scan.entries.length > 0")
        guard (imported as? Bool) == true else { throw ScanFailure.emptyReport }
        // Navigate actual bundled routes after a real scan. A helper exchange can
        // succeed while a native payload crashes one renderer & leaves it blank.
        let routesPassed = try await webView.evaluateJavaScript("""
            (() => {
                const failures = [];
                const capture = event => failures.push(String(event.message));
                window.addEventListener('error', capture);
                try {
                    for (const route of ['storage', 'find', 'duplicates', 'cleanup', 'apps', 'monitor', 'activity', 'compress']) {
                        document.querySelector(`[data-view="${route}"]`).click();
                        const view = document.querySelector('#app-view');
                        const title = view.querySelector('.page-head .eyebrow');
                        if (!title || title.textContent.trim().toLowerCase() !== route || !view.querySelector('.card, .capability')) {
                            failures.push(`Blank or mismatched ${route} section`);
                        }
                    }
                    return failures.join('; ');
                } finally { window.removeEventListener('error', capture); }
            })()
            """)
        guard let routeFailures = routesPassed as? String, routeFailures.isEmpty else {
            throw ScanFailure.invalidJSON("Dashboard route journey failed: \(routesPassed)")
        }
        let searchPassed = try await webView.evaluateJavaScript("""
            (() => {
                document.querySelector('[data-view="find"]').click();
                document.querySelector('#find-name').value = 'example.txt';
                document.querySelector('#find-form').requestSubmit();
                const one = document.querySelector('.results-bar').textContent.trim() === '1 matches';
                document.querySelector('#find-name').value = 'cockpit-no-match-92481';
                document.querySelector('#find-form').requestSubmit();
                const zero = document.querySelector('.results-bar').textContent.trim() === '0 matches';
                document.querySelector('[data-clear-filters]').click();
                return one && zero;
            })()
            """)
        guard (searchPassed as? Bool) == true else { throw ScanFailure.invalidJSON("Dashboard search regression") }
        // Exercise real WKScriptMessage numeric bridging through indexed search.
        // JavaScript integral numbers arrive as floating NSNumber values, unlike
        // directly constructed Swift dictionaries in helper checks.
        _ = try await webView.evaluateJavaScript("""
            document.querySelector('[data-action="refresh_filename_index"]').click();
            """)
        var indexReady = false
        for _ in 0..<150 {
            let loaded = try await webView.evaluateJavaScript("""
                !!document.querySelector('[data-action="filename_index_status"]')
                """)
            if (loaded as? Bool) == true { indexReady = true; break }
            try await Task.sleep(nanoseconds: 100_000_000)
        }
        guard indexReady else { throw ScanFailure.invalidJSON("Native filename index did not become ready") }
        _ = try await webView.evaluateJavaScript("""
            document.querySelector('#find-name').value = 'example.txt';
            document.querySelector('#find-minBytes').value = '1';
            document.querySelector('#find-maxBytes').value = '1048576';
            document.querySelector('#find-createdAfter').value = '2000-01-01';
            document.querySelector('#find-createdBefore').value = '2999-12-31';
            document.querySelector('#find-modifiedAfter').value = '2000-01-01';
            document.querySelector('#find-modifiedBefore').value = '2999-12-31';
            document.querySelector('#find-form').requestSubmit();
            """)
        var indexedSearchPassed = false
        for _ in 0..<150 {
            let matched = try await webView.evaluateJavaScript("""
                document.querySelector('.results-bar').textContent.trim() === '1 indexed matches · 0 offset'
                && !!document.querySelector('[data-index-inspect$="/example.txt"]')
                """)
            if (matched as? Bool) == true { indexedSearchPassed = true; break }
            try await Task.sleep(nanoseconds: 100_000_000)
        }
        guard indexedSearchPassed else { throw ScanFailure.invalidJSON("Native indexed dashboard search regression") }
        window?.performClose(nil)
        show()
        let retained = try await webView.evaluateJavaScript("window.CockpitDashboard.getState().scan.entries.length > 0")
        guard (retained as? Bool) == true else { throw ScanFailure.invalidJSON("Dashboard close lost scan") }
    }

    /// Full app shutdown: cancels scanning & releases retained dashboard state.
    public func stop() {
        nativeServices.stop()
        filenameIndex.stop()
        scanTask?.cancel()
        actionTask?.cancel()
        coordinator.runner.cancel()
        webView?.stopLoading()
        webView?.navigationDelegate = nil
        webView?.uiDelegate = nil
        webView = nil
        window?.delegate = nil
        window?.orderOut(nil)
        window?.close()
        window = nil
    }

    private func buildWindow() {
        let config = WKWebViewConfiguration()
        config.defaultWebpagePreferences.allowsContentJavaScript = true
        // No WKUserContentController script handlers beyond the allowlisted name.
        let sink = BridgeSink(host: self)
        config.userContentController.add(sink, name: "cockpitStatus")
        config.userContentController.add(sink, name: "cockpitAction")
        // CSP via document-start script: file:// pages can't carry response headers.
        // 'self' + file subresources under the read-access grant; connect-src 'none' blocks
        // any network fetch the page might attempt.
        config.userContentController.addUserScript(WKUserScript(
            source: """
            var m=document.createElement('meta');m.httpEquiv='Content-Security-Policy';
            m.content="default-src 'none';script-src 'self' 'unsafe-inline';style-src 'self' 'unsafe-inline';img-src 'self' data:;font-src 'self';connect-src 'none'";
            (document.head||document.documentElement).appendChild(m);
            """,
            injectionTime: .atDocumentStart, forMainFrameOnly: true))
        let web = WKWebView(frame: NSRect(x: 0, y: 0, width: 1100, height: 760), configuration: config)
        web.navigationDelegate = self
        web.uiDelegate = self

        let win = NSWindow(contentRect: web.frame,
                           styleMask: [.titled, .closable, .resizable, .miniaturizable],
                           backing: .buffered, defer: false)
        win.title = "Cockpit — Storage"
        win.minSize = NSSize(width: 640, height: 480)
        win.isReleasedWhenClosed = false
        win.delegate = self
        win.contentView = web

        let toolbar = NSToolbar(identifier: "cockpit.dashboard")
        toolbar.delegate = self
        win.toolbar = toolbar
        win.toolbarStyle = .unified

        self.webView = web
        self.window = win

        if FileManager.default.fileExists(atPath: configuration.indexURL.path) {
            web.loadFileURL(configuration.indexURL, allowingReadAccessTo: configuration.dashboardDirectory)
        } else {
            web.loadHTMLString("<h1 style='font-family:system-ui'>Dashboard assets missing from bundle</h1>", baseURL: nil)
        }
    }

    @objc private func scanFolder() {
        guard !busy else { return }
        let panel = NSOpenPanel()
        panel.canChooseFiles = false
        panel.canChooseDirectories = true
        panel.allowsMultipleSelection = false
        panel.message = "Choose a folder to scan (read-only)"
        guard window != nil, panel.runModal() == .OK, let root = panel.url else { return }
        scanTask?.cancel()
        busy = true
        scanTask = Task { [weak self] in
            guard let self else { return }
            defer { self.busy = false; self.scanTask = nil }
            do {
                let object = try await self.coordinator.scanExport(root: root, stateDirectory: self.stateDirectory)
                let script = try DashboardInjection.importJavaScript(for: object)
                guard !Task.isCancelled, let webView = self.webView else { return }
                if script.count > self.configuration.requestLimit { throw ScanFailure.outputTruncated }
                try await webView.evaluateJavaScript(script)
                self.acceptNativeScan(object)
            } catch is CancellationError {
            } catch {
                guard !Task.isCancelled else { return }
                try? await self.webView?.evaluateJavaScript(
                    DashboardInjection.errorJavaScript("Scan failed: \(error.localizedDescription)"))
            }
        }
    }

    private func acceptNativeScan(_ object: [String: Any]) {
        guard let snapshot = object["snapshot"] as? [String: Any],
              let id = snapshot["id"] as? String,
              let report = snapshot["report"] as? [String: Any],
              let roots = report["roots"] as? [String], roots.count == 1,
              let entries = report["entries"] as? [[String: Any]] else { return }
        cleanupService.revokeReviewedPlans()
        trustedDuplicateReport = nil
        reviewedDuplicates.removeAll()
        trustedCleanupPaths = Set((snapshot["findings"] as? [[String: Any]] ?? []).compactMap { finding in
            guard finding["eligible"] as? Bool == true,
                  finding["report_only"] as? Bool != true,
                  finding["route"] as? String == "trash" else { return nil }
            return finding["path"] as? String
        })
        trustedSnapshotID = id
        trustedRoot = URL(fileURLWithPath: roots[0]).standardizedFileURL
        trustedEntries = Dictionary(entries.compactMap { entry in
            guard let path = entry["path"] as? String else { return nil }
            return (path, entry)
        }, uniquingKeysWith: { first, _ in first })
    }

    private func requireNativeScan(_ payload: [String: Any]) throws -> URL {
        guard let root = trustedRoot, let id = payload["snapshot_id"] as? String,
              id == trustedSnapshotID else {
            throw ScanFailure.invalidJSON("Scan a folder in Cockpit before using this native action")
        }
        return root
    }

    fileprivate func receiveAction(_ message: WKScriptMessage) {
        guard message.frameInfo.isMainFrame,
              message.frameInfo.request.url.map({ DashboardNavigation.isAllowed($0, dashboardRoot: configuration.dashboardDirectory) }) == true,
              let body = message.body as? [String: Any], (body["version"] as? Int) == 1,
              let id = body["request_id"] as? String, !id.isEmpty, id.utf8.count <= 128,
              let action = body["action"] as? String, action.utf8.count <= 64,
              let bytes = try? JSONSerialization.data(withJSONObject: body), bytes.count <= 64 * 1024 else { return }
        let payload = body["payload"] as? [String: Any] ?? [:]
        if action == "cancel_compress" {
            let result = activeAction == "compress_media" && payload["compression_request_id"] as? String == activeRequestID ? nativeServices.cancelCompression()
                : ["state": "no-active", "status": "no-active", "phase": NSNull()]
            respond(id: id, action: action, result: .success(result))
            return
        }
        guard !busy else {
            respond(id: id, action: action, result: .failure(ScanFailure.invalidJSON("Another operation is running")))
            return
        }
        busy = true
        activeAction = action
        activeRequestID = id
        actionTask = Task { [weak self] in
            guard let self else { return }
            defer { self.busy = false; self.actionTask = nil; self.activeAction = nil; self.activeRequestID = nil }
            do {
                let data = try await self.performAction(action, payload: payload)
                try Task.checkCancellation()
                self.respond(id: id, action: action, result: .success(data))
            } catch {
                self.respond(id: id, action: action, result: .failure(error))
            }
        }
    }

    private func performAction(_ action: String, payload: [String: Any]) async throws -> [String: Any] {
        switch action {
        case "refresh_filename_index":
            let root = try requireNativeScan(payload)
            return try await filenameIndex.refresh(root: root)
        case "query_filename_index", "filename_index_status":
            let root = try requireNativeScan(payload)
            guard filenameIndex.statusPayload()["root"] as? String == root.path else {
                throw ScanFailure.invalidJSON("Build filename index for current folder first")
            }
            if action == "filename_index_status" { return filenameIndex.statusPayload() }
            return try filenameIndex.query(payload: payload)
        case "refresh_apps":
            reviewedApplications.removeAll()
            cleanupService.revokeReviewedPlans()
            reviewedDuplicates.removeAll()
            var apps = try await nativeServices.appsPayload()
            let rows = apps["apps"] as? [[String: Any]] ?? []
            trustedApplications = Dictionary(rows.compactMap { row in
                guard let path = row["path"] as? String, let bundle = row["bundleID"] as? String else { return nil }
                return (path, bundle)
            }, uniquingKeysWith: { first, _ in first })
            // Standard roots do not cover portable/external installations.
            do { try applicationDetails.recordInventory(rows, coverageComplete: false) }
            catch { apps["historyReason"] = "history_persistence_unavailable" }
            apps["history"] = applicationDetails.historyPayload()
            return apps
        case "app_details":
            guard let path = payload["path"] as? String, let bundle = trustedApplications[path] else {
                throw ScanFailure.invalidJSON("Refresh inventory & select an application first")
            }
            let before = try? await coordinator.commandObject(arguments: ["procs", "--sort", "ram", "--json"])
            var details = try await applicationDetails.details(appPath: path, bundleID: bundle)
            do {
                guard let before, let gui = details["processes"] as? [String: Any],
                      let entries = gui["entries"] as? [[String: Any]], !entries.isEmpty else {
                    throw ScanFailure.invalidJSON("No observable running GUI process")
                }
                let after = try await coordinator.commandObject(arguments: ["procs", "--sort", "ram", "--json"])
                let pending = try await applicationInspection.inspect(applicationPath: path, observations: ["before": before, "after": after, "gui": gui])
                let final = try await coordinator.commandObject(arguments: ["procs", "--sort", "ram", "--json"])
                details["inspection"] = try applicationInspection.confirm(applicationPath: path, inspection: pending, finalObservations: final)
            } catch {
                details["inspection"] = ["available": false, "reason": "Process inspection unavailable: \(error.localizedDescription)", "coverage": "unknown"]
            }
            return details
        case "check_app_updates":
            guard let path = payload["path"] as? String, let bundle = trustedApplications[path] else {
                throw ScanFailure.invalidJSON("Refresh inventory & select an application first")
            }
            let provider = payload["provider"] as? String ?? "appcast"
            var result: [String: Any]
            switch provider {
            case "appcast":
                result = try await applicationUpdates.check(applicationPath: URL(fileURLWithPath: path), expectedBundleID: bundle)
            case "homebrew":
                result = await NativeHomebrewUpdates.check(bundleURL: URL(fileURLWithPath: path), bundleID: bundle)
            default:
                throw ScanFailure.invalidJSON("Unsupported update metadata provider")
            }
            result["path"] = path
            return result
        case "review_app_uninstall":
            guard let path = payload["path"] as? String, let bundle = trustedApplications[path] else {
                throw ScanFailure.invalidJSON("Refresh inventory & select an application first")
            }
            let url = URL(fileURLWithPath: path)
            try NativeApplicationLiveness.requireNotRunning(bundle: url)
            let result = try cleanupService.reviewApplication(bundle: url, bundleID: bundle, presenting: window)
            // Native confirmation can stay open while another process starts.
            // Repeat complete admission after confirmation & again at claim.
            try NativeApplicationLiveness.requireNotRunning(bundle: url)
            guard let plan = result["plan_id"] as? String else {
                throw ScanFailure.invalidJSON("Application review did not return a plan")
            }
            reviewedApplications[plan] = (path, bundle)
            return result
        case "apply_app_uninstall":
            guard let plan = payload["plan_id"] as? String,
                  let reviewed = reviewedApplications[plan],
                  trustedApplications[reviewed.path] == reviewed.bundleID else {
                throw ScanFailure.invalidJSON("Review selected application again")
            }
            let result = try cleanupService.applyApplication(planID: plan) {
                try NativeApplicationLiveness.requireNotRunning(bundle: URL(fileURLWithPath: reviewed.path))
            }
            reviewedApplications.removeValue(forKey: plan)
            var response = result
            response["cleanup"] = try cleanupService.historyPayload()
            trustedApplications.removeValue(forKey: reviewed.path)
            return response
        case "refresh_monitor":
            let before = try await coordinator.commandObject(arguments: ["procs", "--sort", "ram", "--json"])
            var monitor = try await nativeServices.monitorPayload()
            let processes = try await coordinator.commandObject(arguments: ["procs", "--sort", "ram", "--groups", "--json"])
            monitor["processes"] = processes["processes"]
            monitor["process_groups"] = processes["process_groups"]
            if var ports = monitor["listeningPorts"] as? [String: Any],
               let rows = ports["ports"] as? [[String: Any]] {
                let beforeIDs = processIncarnations(before)
                let afterIDs = processIncarnations(processes)
                ports["ports"] = rows.map { row -> [String: Any] in
                    var observed = row
                    if let pid = (row["pid"] as? NSNumber)?.intValue,
                       let start = beforeIDs[pid], start > 0, afterIDs[pid] == start {
                        observed["startTime"] = start
                        observed["processIdentityVerified"] = true
                    } else {
                        observed["processIdentityVerified"] = false
                        observed["ownerReason"] = "process_incarnation_not_verified"
                    }
                    observed["protocol"] = "TCP"
                    let address = row["address"] as? String ?? ""
                    observed["exposure"] = address == "127.0.0.1" || address == "[::1]" || address == "::1" ? "Loopback" : "Network interface"
                    return observed
                }
                monitor["listeningPorts"] = ports
            }
            return monitor
        case "refresh_activity":
            let compression = try await nativeServices.activityPayload()
            let cleanup = try cleanupService.historyPayload()
            let scan = (try? await coordinator.commandObject(arguments: ["export", "--state-dir", stateDirectory.path, "--json"])) ?? [:]
            var activity = NativeActivityProjection.project(compression: compression, cleanup: cleanup, scans: scan)
            activity["cleanup"] = cleanup
            if let modules = scan["modules"] as? [String: Any] { activity["history"] = modules["history"] }
            if scan.isEmpty { activity["scanHistoryReason"] = "scan_history_unavailable" }
            return activity
        case "find_duplicates":
            let root = try requireNativeScan(payload)
            let minimum = payload["min_size"] as? Int ?? 102400
            guard minimum >= 1 else { throw ScanFailure.invalidJSON("Minimum duplicate size must be positive") }
            let result = try await coordinator.commandObject(arguments: ["duplicates", root.path, "--min-size", String(minimum), "--max-files", "10000", "--max-read-bytes", "1073741824", "--seconds", "30", "--json"])
            guard let report = result["duplicates"] as? [String: Any] else { throw ScanFailure.invalidJSON("Duplicate report missing") }
            cleanupService.revokeReviewedPlans()
            reviewedDuplicates.removeAll()
            trustedDuplicateReport = report
            return report
        case "compress_media":
            let quality = (payload["quality"] as? NSNumber)?.doubleValue ?? 0.8
            return try await nativeServices.compress(format: payload["format"] as? String ?? "jpeg",
                                                      quality: quality,
                                                      maxPixelDimension: payload["max_pixel_dimension"] as? Int,
                                                      targetSizeBytes: payload["target_size_bytes"] as? Int64,
                                                      presenting: window)
        case "preview_compressed_output":
            let url = try nativeServices.verifiedCompressionOutput()
            guard payload["path"] as? String == url.path else {
                throw ScanFailure.invalidJSON("Compression output no longer matches this result")
            }
            previewURL = url
            guard let panel = QLPreviewPanel.shared() else {
                throw ScanFailure.invalidJSON("Quick Look is unavailable")
            }
            panel.dataSource = self
            panel.makeKeyAndOrderFront(nil)
            panel.reloadData()
            return ["path": url.path, "state": "opened"]
        case "reveal_item", "preview_item":
            _ = try requireNativeScan(payload)
            guard let path = payload["path"] as? String, trustedEntries[path] != nil else {
                throw ScanFailure.invalidJSON("Item is outside current native scan")
            }
            let url = URL(fileURLWithPath: path)
            if action == "reveal_item" { NSWorkspace.shared.activateFileViewerSelecting([url]) }
            else {
                previewURL = url
                if let panel = QLPreviewPanel.shared() { panel.dataSource = self; panel.makeKeyAndOrderFront(nil); panel.reloadData() }
            }
            return ["path": path, "state": "opened"]
        case "review_cleanup":
            let root = try requireNativeScan(payload)
            guard let paths = payload["paths"] as? [String], !paths.isEmpty, paths.count <= 100 else {
                throw ScanFailure.invalidJSON("Select between 1 & 100 ordinary files")
            }
            for path in paths {
                guard payload["selection_mode"] as? String == "manual" || trustedCleanupPaths.contains(path) else {
                    throw ScanFailure.invalidJSON("This finding is report-only or lacks verified eligibility")
                }
                guard let entry = trustedEntries[path], let metadata = entry["metadata"] as? [String: Any],
                      metadata["kind"] as? String == "File", metadata["metadata_complete"] as? Bool == true,
                      metadata["is_placeholder"] as? Bool == false else {
                    throw ScanFailure.invalidJSON("Cleanup requires fully inspected ordinary files")
                }
            }
            let duplicates = try NativeDuplicateRevalidation.review(report: trustedDuplicateReport ?? [:], selectedPaths: paths)
            var result = try cleanupService.review(paths: paths.map { URL(fileURLWithPath: $0) }, root: root, presenting: window)
            if let duplicates {
                try duplicates.revalidate()
                guard let plan = result["plan_id"] as? String else { throw ScanFailure.invalidJSON("Cleanup review did not return a plan") }
                reviewedDuplicates[plan] = duplicates
                result["duplicate_content_revalidated"] = true
            }
            return result
        case "apply_cleanup":
            guard let plan = payload["plan_id"] as? String else { throw ScanFailure.invalidJSON("Review cleanup first") }
            let duplicates = reviewedDuplicates[plan]
            var result = try cleanupService.apply(planID: plan) { try duplicates?.revalidate() }
            reviewedDuplicates.removeValue(forKey: plan)
            if duplicates != nil { result["duplicate_content_revalidated"] = true }
            return result
        case "undo_cleanup":
            guard let plan = payload["plan_id"] as? String else { throw ScanFailure.invalidJSON("Missing cleanup plan") }
            if payload["paths"] != nil, payload["paths"] as? [String] == nil {
                throw ScanFailure.invalidJSON("Restore selection must contain item paths")
            }
            var result = try cleanupService.undo(planID: plan, paths: payload["paths"] as? [String])
            result["cleanup"] = try cleanupService.historyPayload()
            return result
        default: throw ScanFailure.invalidJSON("Unsupported native action")
        }
    }

    private func processIncarnations(_ payload: [String: Any]) -> [Int: UInt64] {
        var result: [Int: UInt64] = [:]
        for process in payload["processes"] as? [[String: Any]] ?? [] {
            guard let identity = process["identity"] as? [String: Any],
                  let pid = identity["pid"] as? NSNumber,
                  let start = identity["start_time"] as? NSNumber else { continue }
            result[pid.intValue] = start.uint64Value
        }
        return result
    }

    private func respond(id: String, action: String, result: Result<[String: Any], Error>) {
        var object: [String: Any] = ["request_id": id, "action": action]
        switch result {
        case .success(let data): object["ok"] = true; object["data"] = data
        case .failure(let error):
            object["ok"] = false
            object["error"] = (error as? NativeCleanupService.Error)?.errorDescription ?? String(describing: error)
        }
        guard let data = try? JSONSerialization.data(withJSONObject: object), data.count <= configuration.requestLimit else { return }
        let literal = String(decoding: data, as: UTF8.self).replacingOccurrences(of: "<", with: "\\u003c")
            .replacingOccurrences(of: "\u{2028}", with: "\\u2028").replacingOccurrences(of: "\u{2029}", with: "\\u2029")
        webView?.evaluateJavaScript("window.CockpitDashboard.receiveAction(\(literal))", completionHandler: nil)
    }
}

/// Drops every script message except allowlisted ones; commands remain fixed native actions.
@MainActor
private final class BridgeSink: NSObject, WKScriptMessageHandler {
    weak var host: DashboardHost?
    init(host: DashboardHost) { self.host = host }
    func userContentController(_ controller: WKUserContentController, didReceive message: WKScriptMessage) {
        guard DashboardNavigation.allowedScriptMessages.contains(message.name) else { return }
        if message.name == "cockpitAction" { host?.receiveAction(message) }
    }
}

extension DashboardHost: QLPreviewPanelDataSource {
    public func numberOfPreviewItems(in panel: QLPreviewPanel!) -> Int { previewURL == nil ? 0 : 1 }
    public func previewPanel(_ panel: QLPreviewPanel!, previewItemAt index: Int) -> QLPreviewItem! {
        previewURL as NSURL?
    }
}

extension DashboardHost: NSWindowDelegate {
    public func windowShouldClose(_ sender: NSWindow) -> Bool {
        // Closing a dashboard is not quitting Cockpit; retain loaded scan & navigation.
        sender.orderOut(nil)
        return false
    }
}

extension DashboardHost: WKNavigationDelegate {
    public func webView(_ webView: WKWebView, didFinish navigation: WKNavigation!) {
        guard restoreEnabled, trustedSnapshotID == nil, !busy,
              FileManager.default.fileExists(atPath: stateDirectory.path) else { return }
        busy = true
        scanTask = Task { [weak self] in
            guard let self else { return }
            defer { self.busy = false; self.scanTask = nil }
            do {
                let object = try await self.coordinator.commandObject(arguments: ["export", "--state-dir", self.stateDirectory.path, "--json"])
                guard !Task.isCancelled else { return }
                _ = try await webView.evaluateJavaScript(DashboardInjection.importJavaScript(for: object))
                self.acceptNativeScan(object)
            } catch {
                // A missing/corrupt history never replaces a user-selected scan.
                // Scan folder remains available to create a fresh snapshot.
            }
        }
    }

    public func webView(_ webView: WKWebView,
                        decidePolicyFor action: WKNavigationAction,
                        decisionHandler: @escaping (WKNavigationActionPolicy) -> Void) {
        let url = action.request.url
        let allowed = url.map { DashboardNavigation.isAllowed($0, dashboardRoot: configuration.dashboardDirectory) } ?? false
        decisionHandler(allowed ? .allow : .cancel)
    }
}

extension DashboardHost: WKUIDelegate {
    public func webView(_ webView: WKWebView,
                        runOpenPanelWith parameters: WKOpenPanelParameters,
                        initiatedByFrame frame: WKFrameInfo,
                        completionHandler: @escaping ([URL]?) -> Void) {
        guard webView === self.webView, let window else { completionHandler(nil); return }
        let panel = NSOpenPanel()
        panel.canChooseFiles = true
        panel.canChooseDirectories = false
        panel.allowsMultipleSelection = false
        panel.allowedContentTypes = [.json]
        panel.message = "Choose a Cockpit scan JSON export"
        panel.beginSheetModal(for: window) { response in
            completionHandler(response == .OK ? panel.urls : nil)
        }
    }
}

extension DashboardHost: NSToolbarDelegate {
    public func toolbar(_ toolbar: NSToolbar, itemForItemIdentifier id: NSToolbarItem.Identifier,
                        willBeInsertedIntoToolbar flag: Bool) -> NSToolbarItem? {
        let item = NSToolbarItem(itemIdentifier: id)
        item.label = "Scan folder"
        item.image = NSImage(systemSymbolName: "folder.badge.magnifyingglass", accessibilityDescription: "Scan folder")
        item.target = self
        item.action = #selector(scanFolder)
        return item
    }
    public func toolbarDefaultItemIdentifiers(_ toolbar: NSToolbar) -> [NSToolbarItem.Identifier] {
        [NSToolbarItem.Identifier("cockpit.scan")]
    }
    public func toolbarAllowedItemIdentifiers(_ toolbar: NSToolbar) -> [NSToolbarItem.Identifier] {
        [NSToolbarItem.Identifier("cockpit.scan"), .flexibleSpace]
    }
}
#endif
