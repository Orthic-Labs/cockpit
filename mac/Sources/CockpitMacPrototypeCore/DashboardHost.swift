import Foundation
#if canImport(AppKit) && canImport(WebKit)
import AppKit
import WebKit
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
                let queue = DispatchQueue(label: "cockpit.dashboard.scan")
                queue.async { continuation.resume(with: .init { try self.execute(request, generation: generation) }) }
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

    func append(stdout chunk: Data) { append(&out, chunk: chunk, limit: outLimit) }
    func append(stderr chunk: Data) { append(&err, chunk: chunk, limit: errLimit) }

    private func append(_ buffer: inout Data, chunk: Data, limit: Int) {
        guard !chunk.isEmpty else { return }
        lock.lock()
        defer { lock.unlock() }
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
    public static let allowedScriptMessages: Set<String> = ["cockpitStatus"]
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

    public init(configuration: DashboardHostConfiguration = .bundleDefault(),
                runner: any ScanRunning = ProcessScanRunner()) {
        self.configuration = configuration
        self.coordinator = ScanCoordinator(configuration: configuration, runner: runner)
    }

    public func show() {
        if window == nil { buildWindow() }
        window?.makeKeyAndOrderFront(nil)
        NSApp.activate(ignoringOtherApps: true)
    }

    /// Cancels any scan, destroys the webview and releases the window; reopening is fresh.
    public func stop() {
        scanTask?.cancel()
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
        config.userContentController.add(BridgeSink(host: self), name: "cockpitStatus")
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
        let panel = NSOpenPanel()
        panel.canChooseFiles = false
        panel.canChooseDirectories = true
        panel.allowsMultipleSelection = false
        panel.message = "Choose a folder to scan (read-only)"
        guard window != nil, panel.runModal() == .OK, let root = panel.url else { return }
        scanTask?.cancel()
        scanTask = Task { [weak self] in
            guard let self else { return }
            do {
                let script = try await self.coordinator.importScript(for: root)
                guard !Task.isCancelled, let webView = self.webView else { return }
                if script.count > self.configuration.requestLimit { throw ScanFailure.outputTruncated }
                try await webView.evaluateJavaScript(script)
            } catch is CancellationError {
            } catch {
                try? await self.webView?.evaluateJavaScript(
                    DashboardInjection.errorJavaScript("Scan failed: \(error.localizedDescription)"))
            }
        }
    }
}

/// Drops every script message except allowlisted ones; messages never carry commands.
private final class BridgeSink: NSObject, WKScriptMessageHandler {
    weak var host: DashboardHost?
    init(host: DashboardHost) { self.host = host }
    func userContentController(_ controller: WKUserContentController, didReceive message: WKScriptMessage) {
        guard DashboardNavigation.allowedScriptMessages.contains(message.name) else { return }
        // Informational only — status strings are never executed.
    }
}

extension DashboardHost: NSWindowDelegate {
    public func windowWillClose(_ notification: Notification) { stop() }
}

extension DashboardHost: WKNavigationDelegate {
    public func webView(_ webView: WKWebView,
                        decidePolicyFor action: WKNavigationAction,
                        decisionHandler: @escaping (WKNavigationActionPolicy) -> Void) {
        let url = action.request.url
        let allowed = url.map { DashboardNavigation.isAllowed($0, dashboardRoot: configuration.dashboardDirectory) } ?? false
        decisionHandler(allowed ? .allow : .cancel)
    }
}

extension DashboardHost: WKUIDelegate {}

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
