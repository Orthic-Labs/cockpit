import AppKit
import ApplicationServices
import Foundation

// MARK: - What the hub's sharing service publishes (share-state.json)

struct ShareDevice: Decodable, Equatable {
    let fingerprint: String
    let alias: String
    let deviceType: String?
    let deviceModel: String?
    let ip: String?
}

struct ShareIncomingFile: Decodable, Equatable {
    let name: String
    let size: Int64
}

struct ShareIncoming: Decodable, Equatable {
    let id: String
    let from: String
    let fileCount: Int
    let totalBytes: Int64
    let isMessage: Bool
    let preview: String?
    let files: [ShareIncomingFile]?
}

struct ShareTransfer: Decodable, Equatable {
    let id: String
    /// "send" or "receive".
    let direction: String
    let peer: String
    /// waiting, active, done, failed, cancelled or declined.
    let state: String
    let totalBytes: Int64
    let doneBytes: Int64
    let filesTotal: Int
    let filesDone: Int
    let current: String?
    let savedTo: String?
    let savedFiles: [String]?
    let error: String?
    /// A received text message, shown with Copy instead of saved as a file.
    let message: String?

    var isOpen: Bool { state == "active" || state == "waiting" }
}

struct ShareNotice: Decodable, Equatable {
    let id: Int
    let text: String
}

struct ShareState: Decodable, Equatable {
    var running: Bool
    var error: String?
    var alias: String?
    var saveDir: String?
    var devices: [ShareDevice]
    var incoming: [ShareIncoming]
    var transfers: [ShareTransfer]
    var warnings: [String]?
    var localNetwork: String?
    var notice: ShareNotice?
    /// The hub is announcing and scanning the subnet for devices right now.
    var scanning: Bool?
    /// Milliseconds since 1970, when the hub last wrote this.
    var updatedAt: Double
}

/// Pulse fork: the notch's side of nearby sharing (the LocalSend protocol).
///
/// The hub process runs the service (`hub/src-tauri/src/share.rs`). The notch
/// reads its `share-state.json` (devices, requests, transfers), shows requests
/// and results as notch cards, and hands it work through `share-commands/`
/// files and a Darwin notification, the same way the hub already talks to the
/// notch. It also starts the hub in the background when sharing is on and the
/// hub is not running.
@MainActor
final class NearbySharing {
    static let shared = NearbySharing()
    static let providerID = SystemProviders.sendID
    /// The paste button row in the Send cell's hover card.
    static let pasteRowID = "action:paste"

    /// The Send cell has something new to show.
    var onChange: (() -> Void)?
    /// The notch card slot (the fleet's disk image card). Returns whether a notch could show it.
    var present: ((DiskImagePrompt?) -> Bool)?
    /// Whether the preference is on.
    var enabled: () -> Bool = { true }

    private(set) var dropTargeting = false
    private var state: ShareState?
    private var started = false
    private var watchdog: Timer?
    private var hubLaunchedAt = Date.distantPast
    private var primed = false
    private var seenFinished = Set<String>()
    private var lastNoticeID = 0

    private enum Card: Equatable {
        case none
        case incoming(String)
        case saved([String])
        case message(String)
        case note
        case choose
        case sending
    }
    private var card = Card.none
    private var cardTimer: Timer?
    private var cardHovered = false
    private var pending: (urls: [URL], text: String?)?
    private var lastChoose: DiskImagePrompt?
    /// The send the card follows: to whom, which transfer once the hub lists it,
    /// and the transfers that already existed (so it is not mistaken for one).
    private var sendPeer: ShareDevice?
    private var sendSummary = ""
    private var sendBaseline = Set<String>()
    private var sendTransferID: String?
    private var sendFinished = false

    private var hovering = Set<ObjectIdentifier>()
    private var tap: EventTapHub?
    private var tapToken: EventTapToken?

    private static let stateNotification = "dev.orthic.pulse.share.state"
    private static let commandNotification = "dev.orthic.pulse.share.command"

    private static var stateURL: URL {
        HubBridge.directory.appendingPathComponent("share-state.json")
    }

    private static var commandsDirectory: URL {
        HubBridge.directory.appendingPathComponent("share-commands", isDirectory: true)
    }

    // MARK: - Lifecycle

    func start() {
        guard !started else { return }
        started = true
        DarwinNotify.observe(Self.stateNotification) { [weak self] in self?.readState() }
        readState()
        watchdog = Timer.scheduledTimer(withTimeInterval: 5, repeats: true) { [weak self] _ in
            MainActor.assumeIsolated { self?.watch() }
        }
        watch()
    }

    /// The hub writes its state at least every four seconds; older than fifteen
    /// means it is not running.
    private func fresh(_ state: ShareState?) -> ShareState? {
        guard let state else { return nil }
        return Date().timeIntervalSince1970 * 1000 - state.updatedAt < 15_000 ? state : nil
    }

    /// Sharing on, hub not running: start it with no window.
    private func watch() {
        guard enabled() else { return }
        if fresh(state) != nil { return }
        if HubLauncher.isRunning { return }
        if Date().timeIntervalSince(hubLaunchedAt) < 30 { return }
        hubLaunchedAt = Date()
        HubLauncher.launchInBackground()
        onChange?()
    }

    private func readState() {
        guard let data = try? Data(contentsOf: Self.stateURL),
              let decoded = try? JSONDecoder().decode(ShareState.self, from: data)
        else { return }
        state = decoded
        process(decoded)
        updatePasteTap()
        onChange?()
    }

    // MARK: - Devices and the target

    var devices: [ShareDevice] { fresh(state)?.devices ?? [] }

    /// Where a paste or drop goes without asking: the only device nearby. With
    /// several, every send asks, so nothing goes to a device by habit.
    var target: ShareDevice? {
        let list = devices
        return list.count == 1 ? list[0] : nil
    }

    /// A click on a device in the hover card. A paste or drop that was waiting
    /// for an answer goes to it at once.
    func select(_ fingerprint: String) {
        if let waiting = pending, let device = devices.first(where: { $0.fingerprint == fingerprint }) {
            pending = nil
            deliver(waiting.urls, waiting.text, to: device)
        }
        onChange?()
    }

    // MARK: - Sending

    func send(urls: [URL], text: String? = nil) {
        let files = urls.filter { FileManager.default.fileExists(atPath: $0.path) }
        guard !files.isEmpty || !(text ?? "").isEmpty else { return }
        guard let live = fresh(state), live.running else {
            showNote(title: L10n.t("Sharing isn't running"),
                     detail: state?.error ?? L10n.t("Nearby sharing is off or still starting."),
                     problem: true)
            return
        }
        guard let device = target else {
            // Several devices, or none yet: list them on the card (looking again
            // when there are none) and send when one is clicked.
            pending = (files, text)
            showChoose(refreshing: live.devices.isEmpty)
            return
        }
        deliver(files, text, to: device)
    }

    private func deliver(_ urls: [URL], _ text: String?, to device: ShareDevice) {
        var body: [String: Any] = [
            "command": "send", "to": device.fingerprint, "paths": urls.map(\.path),
        ]
        if let text, !text.isEmpty { body["text"] = text }
        command(body)
        beginSending(to: device, urls: urls, text: text)
        onChange?()
    }

    /// ⌘V while hovering the Send cell: the clipboard's files, an image, or text.
    func pasteClipboard() {
        let board = NSPasteboard.general
        if let urls = board.readObjects(forClasses: [NSURL.self],
                                        options: [.urlReadingFileURLsOnly: true]) as? [URL], !urls.isEmpty {
            send(urls: urls)
            return
        }
        if let image = NSImage(pasteboard: board), let file = Self.saveImage(image) {
            send(urls: [file])
            return
        }
        if let text = board.string(forType: .string), !text.isEmpty {
            send(urls: [], text: text)
            return
        }
        showNote(title: L10n.t("Nothing to send"),
                 detail: L10n.t("The clipboard has no files or text."), problem: true)
    }

    private static func saveImage(_ image: NSImage) -> URL? {
        guard let tiff = image.tiffRepresentation,
              let bitmap = NSBitmapImageRep(data: tiff),
              let png = bitmap.representation(using: .png, properties: [:])
        else { return nil }
        let folder = FileManager.default.temporaryDirectory.appendingPathComponent("Pulse Clipboard", isDirectory: true)
        try? FileManager.default.createDirectory(at: folder, withIntermediateDirectories: true)
        let stamp = Int(Date().timeIntervalSince1970)
        let file = folder.appendingPathComponent("Clipboard \(stamp).png")
        do {
            try png.write(to: file)
            return file
        } catch {
            return nil
        }
    }

    /// Files dropped on the Send cell.
    func drop(urls: [URL]) -> Bool {
        guard !urls.isEmpty else { return false }
        send(urls: urls)
        return true
    }

    func setDropTargeting(_ on: Bool) {
        guard dropTargeting != on else { return }
        dropTargeting = on
        onChange?()
    }

    /// Ask the hub to announce itself and scan the subnet again.
    func refreshDevices() {
        command(["command": "refresh"])
    }

    func cancelCurrent() {
        if let open = fresh(state)?.transfers.last(where: \.isOpen) {
            command(["command": "cancel", "id": open.id])
        }
    }

    private func command(_ body: [String: Any]) {
        guard let data = try? JSONSerialization.data(withJSONObject: body) else { return }
        let directory = Self.commandsDirectory
        try? FileManager.default.createDirectory(at: directory, withIntermediateDirectories: true)
        let stamp = String(format: "%020.0f-%@", Date().timeIntervalSince1970 * 1_000_000, UUID().uuidString)
        let temp = directory.appendingPathComponent("\(stamp).tmp")
        let final = directory.appendingPathComponent("\(stamp).json")
        do {
            try data.write(to: temp, options: .atomic)
            try FileManager.default.moveItem(at: temp, to: final)
        } catch { return }
        DarwinNotify.post(Self.commandNotification)
    }

    // MARK: - Hover and ⌘V

    /// The pointer is over (or left) the Send cell or its hover card, on one notch.
    func hoverChanged(_ controller: ObjectIdentifier, overSend: Bool) {
        if overSend { hovering.insert(controller) } else { hovering.remove(controller) }
        updatePasteTap()
        onChange?()
    }

    var canPasteWithKeyboard: Bool { AXIsProcessTrusted() }

    /// A key tap exists only while the pointer is on the cell: ⌘V is taken
    /// from the app in front just then, and never otherwise. It needs the
    /// Accessibility permission, like the other key conveniences.
    private func updatePasteTap() {
        let wanted = !hovering.isEmpty && fresh(state)?.running == true
        if wanted {
            guard tap == nil, AXIsProcessTrusted() else { return }
            let hub = EventTapHub(location: .cgSessionEventTap, events: [.keyDown])
            guard hub.start() else { return }
            tapToken = hub.register(priority: 0) { type, event in
                guard type == .keyDown,
                      event.getIntegerValueField(.keyboardEventKeycode) == 9,
                      event.flags.contains(.maskCommand),
                      event.flags.intersection([.maskShift, .maskAlternate, .maskControl]).isEmpty
                else { return .pass }
                if event.getIntegerValueField(.keyboardEventAutorepeat) == 0 {
                    DispatchQueue.main.async {
                        MainActor.assumeIsolated { NearbySharing.shared.pasteClipboard() }
                    }
                }
                return .swallow
            }
            tap = hub
        } else if let hub = tap {
            if let tapToken { hub.unregister(tapToken) }
            hub.stop()
            tap = nil
            tapToken = nil
        }
    }

    // MARK: - The Send cell

    private func size(_ bytes: Int64) -> String {
        ByteCountFormatter.string(fromByteCount: bytes, countStyle: .file)
    }

    private func kind(of device: ShareDevice) -> String {
        switch device.deviceType {
        case "mobile": return L10n.t("Phone")
        case "desktop": return L10n.t("Computer")
        case "web": return L10n.t("Browser")
        case "headless": return L10n.t("Terminal")
        case "server": return L10n.t("Server")
        default: return L10n.t("Device")
        }
    }

    /// The cell: a ring for the transfer in flight (empty and idle otherwise),
    /// then the devices nearby, then a hint.
    func providerSnapshot() -> ProviderSnapshot {
        let live = fresh(state)
        let devices = live?.devices ?? []
        let active = live?.transfers.last(where: \.isOpen)
        let headline: LimitWindow
        if let transfer = active {
            let fraction = transfer.totalBytes > 0
                ? min(max(Double(transfer.doneBytes) / Double(transfer.totalBytes), 0), 1) : 0
            let label = transfer.direction == "send"
                ? L10n.t("Sending to \(transfer.peer)") : L10n.t("Receiving from \(transfer.peer)")
            let detail: String
            if transfer.state == "waiting" {
                detail = transfer.direction == "send"
                    ? L10n.t("Waiting for \(transfer.peer) to accept") : L10n.t("Waiting")
            } else {
                detail = L10n.t("\(size(transfer.doneBytes)) of \(size(transfer.totalBytes))")
                    + (transfer.filesTotal > 1
                        ? " · " + L10n.t("\(min(transfer.filesDone + 1, transfer.filesTotal)) of \(transfer.filesTotal) files")
                        : "")
            }
            headline = LimitWindow(id: "transfer", label: label, usedFraction: fraction,
                                   detail: detail, bandOverride: .ample)
        } else {
            let line: String
            if !enabled() {
                line = L10n.t("Off")
            } else if let live {
                if let error = live.error {
                    line = error
                } else if dropTargeting, let target {
                    line = L10n.t("Release to send to \(target.alias)")
                } else if devices.isEmpty {
                    line = L10n.t("No devices nearby")
                } else {
                    line = L10n.t("\(devices.count) nearby")
                }
            } else {
                line = L10n.t("Starting…")
            }
            headline = LimitWindow(id: "transfer", label: L10n.t("Nearby sharing"),
                                   usedText: L10n.t("Idle"), detail: line, prefersUsedText: true)
        }

        var windows = [headline]
        for device in devices {
            windows.append(LimitWindow(
                id: "nearby:" + device.fingerprint, label: device.alias, detail: kind(of: device)))
        }
        if live?.localNetwork == "blocked" {
            windows.append(LimitWindow(id: "hint-network", label: "",
                                       detail: L10n.t("Allow Local Network for Pulse in System Settings.")))
        } else if !devices.isEmpty {
            windows.append(LimitWindow(id: Self.pasteRowID, label: L10n.t("Paste clipboard")))
            if !canPasteWithKeyboard {
                windows.append(LimitWindow(id: "hint-keys", label: "",
                                           detail: L10n.t("Allow Accessibility to paste with ⌘V")))
            } else {
                windows.append(LimitWindow(id: "hint-paste", label: "",
                                           detail: L10n.t("⌘V sends the clipboard · drop files here")))
            }
        }
        return ProviderSnapshot(id: Self.providerID, displayName: L10n.t("Send"), glyph: .send,
                                fidelity: .official, status: .ok, windows: windows,
                                headlineID: "transfer", kind: .system)
    }

    // MARK: - Notch cards

    private var defaultFolder: String { NSHomeDirectory() + "/Downloads" }

    private func iconPath(_ preferred: String?) -> String {
        if let preferred, FileManager.default.fileExists(atPath: preferred) { return preferred }
        return FileManager.default.fileExists(atPath: defaultFolder) ? defaultFolder : "/Applications"
    }

    /// A card is up: its buttons and hover come here instead of the disk image installer.
    func handlesCard() -> Bool { card != .none }

    func handleCardChoice(_ choice: DiskImageChoice) {
        switch card {
        case .incoming(let id):
            command(["command": choice == .install ? "accept" : "decline", "id": id])
            clearCard()
        case .saved(let files):
            if choice == .showImage {
                let urls = files.map { URL(fileURLWithPath: $0) }.filter { FileManager.default.fileExists(atPath: $0.path) }
                if !urls.isEmpty { NSWorkspace.shared.activateFileViewerSelecting(urls) }
            }
            clearCard()
        case .message(let text):
            if choice == .copyText {
                NSPasteboard.general.clearContents()
                NSPasteboard.general.setString(text, forType: .string)
                // A brief "Copied" in place of the card, then it goes.
                show(DiskImagePrompt(iconPath: iconPath("/System/Applications/Messages.app"),
                                     title: L10n.t("Copied"), detail: "", style: .done),
                     as: .note)
                scheduleExpiry(after: 1.2)
                return
            }
            if choice == .openLink, let url = Self.link(in: text) {
                NSWorkspace.shared.open(url)
            }
            clearCard()
        case .choose:
            switch choice {
            case .sendTo(let fingerprint):
                if let waiting = pending, let device = devices.first(where: { $0.fingerprint == fingerprint }) {
                    pending = nil
                    deliver(waiting.urls, waiting.text, to: device)
                }
            case .refresh:
                refreshDevices()
            default:
                pending = nil
                clearCard()
            }
        case .sending:
            if choice == .cancel, let id = sendTransferID,
               fresh(state)?.transfers.first(where: { $0.id == id })?.isOpen == true {
                command(["command": "cancel", "id": id])
            }
            // The close only puts the card away: the transfer carries on and
            // is announced when it ends.
            clearCard()
        case .note, .none:
            clearCard()
        }
    }

    func handleCardHover(_ on: Bool) {
        cardHovered = on
        switch card {
        case .saved, .note:
            cardTimer?.invalidate()
            if !on { scheduleExpiry(after: 3) }
        case .choose:
            cardTimer?.invalidate()
            if !on { scheduleExpiry(after: 30) }
        case .sending:
            if on { cardTimer?.invalidate() } else if sendFinished { scheduleExpiry(after: 2) }
        default:
            break
        }
    }

    private func clearCard() {
        cardTimer?.invalidate()
        cardTimer = nil
        cardHovered = false
        guard card != .none else { return }
        if card == .choose { lastChoose = nil }
        if card == .sending { sendPeer = nil; sendTransferID = nil; sendFinished = false }
        card = .none
        _ = present?(nil)
    }

    private func scheduleExpiry(after seconds: TimeInterval) {
        cardTimer?.invalidate()
        let shown = card
        cardTimer = Timer.scheduledTimer(withTimeInterval: seconds, repeats: false) { [weak self] _ in
            MainActor.assumeIsolated {
                guard let self, self.card == shown, !self.cardHovered else { return }
                if shown == .choose { self.pending = nil }
                self.clearCard()
            }
        }
    }

    private func show(_ prompt: DiskImagePrompt, as next: Card) {
        card = next
        _ = present?(prompt)
    }

    private func process(_ new: ShareState) {
        if !primed {
            // What finished before the notch looked is not news.
            primed = true
            for transfer in new.transfers where !transfer.isOpen { seenFinished.insert(transfer.id) }
            lastNoticeID = new.notice?.id ?? 0
        }
        if card == .choose { refreshChooseCard() }
        if card == .sending { updateSending(new) }
        if case .incoming(let id) = card, !new.incoming.contains(where: { $0.id == id }) {
            clearCard()
        }
        if let request = new.incoming.first {
            if card != .incoming(request.id) { showIncoming(request, folder: new.saveDir) }
            return
        }
        for transfer in new.transfers where !transfer.isOpen && !seenFinished.contains(transfer.id) {
            seenFinished.insert(transfer.id)
            announce(transfer, folder: new.saveDir)
        }
        if let notice = new.notice, notice.id > lastNoticeID {
            lastNoticeID = notice.id
            showNote(title: L10n.t("Couldn't send"), detail: notice.text, problem: true)
        }
    }

    private func showIncoming(_ request: ShareIncoming, folder: String?) {
        let title: String
        var detail: String
        if request.isMessage {
            title = L10n.t("\(request.from) wants to send a message")
            detail = request.preview ?? ""
        } else {
            let total = size(request.totalBytes)
            title = request.fileCount == 1
                ? L10n.t("\(request.from) wants to send 1 file (\(total))")
                : L10n.t("\(request.from) wants to send \(request.fileCount) files (\(total))")
            detail = request.files?.first?.name ?? ""
        }
        let place = URL(fileURLWithPath: folder ?? defaultFolder).lastPathComponent
        if !detail.isEmpty { detail += " · " }
        detail += L10n.t("Saves to \(place)")
        cardTimer?.invalidate()
        show(DiskImagePrompt(iconPath: iconPath(folder), title: title, detail: detail, style: .ask,
                             primary: .init(choice: .install, label: L10n.t("Accept")),
                             secondary: .init(choice: .cancel, label: L10n.t("Decline"))),
             as: .incoming(request.id))
    }

    /// The message as a web address, when it is nothing else.
    private static func link(in text: String) -> URL? {
        let trimmed = text.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !trimmed.contains(where: \.isWhitespace),
              let url = URL(string: trimmed), let scheme = url.scheme?.lowercased(),
              scheme == "http" || scheme == "https" else { return nil }
        return url
    }

    private func announce(_ transfer: ShareTransfer, folder: String?) {
        switch (transfer.direction, transfer.state) {
        case ("receive", "done") where transfer.message != nil:
            let text = transfer.message ?? ""
            let link = Self.link(in: text)
            show(DiskImagePrompt(iconPath: iconPath("/System/Applications/Messages.app"),
                                 title: L10n.t("Message from \(transfer.peer)"), detail: text, style: .ask,
                                 primary: .init(choice: .copyText, label: L10n.t("Copy")),
                                 secondary: link == nil ? nil : .init(choice: .openLink, label: L10n.t("Open"))),
                 as: .message(text))
        case ("receive", "done"):
            let files = transfer.savedFiles ?? []
            let place = URL(fileURLWithPath: transfer.savedTo ?? folder ?? defaultFolder).lastPathComponent
            let detail = files.count == 1
                ? URL(fileURLWithPath: files[0]).lastPathComponent
                : L10n.t("\(files.count) files from \(transfer.peer)")
            show(DiskImagePrompt(iconPath: iconPath(files.first ?? transfer.savedTo),
                                 title: L10n.t("Saved to \(place)"), detail: detail, style: .done,
                                 primary: .init(choice: .showImage, label: L10n.t("Show"))),
                 as: .saved(files))
            scheduleExpiry(after: 8)
        case ("send", "done"):
            let count = max(transfer.filesTotal, 1)
            showNote(title: L10n.t("Sent to \(transfer.peer)"),
                     detail: count == 1 ? L10n.t("1 file") : L10n.t("\(count) files"), problem: false)
        case (_, "declined"):
            showNote(title: L10n.t("\(transfer.peer) declined"), detail: L10n.t("Nothing was sent."), problem: true)
        case (_, "failed"):
            showNote(title: transfer.direction == "send" ? L10n.t("Couldn't send") : L10n.t("Couldn't receive"),
                     detail: transfer.error ?? "", problem: true)
        default:
            break
        }
    }

    private func showNote(title: String, detail: String, problem: Bool) {
        show(DiskImagePrompt(iconPath: iconPath(nil), title: title, detail: detail,
                             style: problem ? .problem : .done),
             as: .note)
        scheduleExpiry(after: problem ? 8 : 4)
    }

    // MARK: - The device list and the transfer, on the card

    private func symbol(of device: ShareDevice) -> String {
        switch device.deviceType {
        case "mobile": return "iphone"
        case "desktop": return "laptopcomputer"
        case "web": return "globe"
        case "headless": return "terminal"
        case "server": return "server.rack"
        default: return "display"
        }
    }

    private func summary(urls: [URL], text: String?) -> String {
        if urls.isEmpty { return L10n.t("Text") }
        if urls.count == 1 { return urls[0].lastPathComponent }
        return L10n.t("\(urls.count) files")
    }

    private var pendingSummary: String {
        guard let pending else { return "" }
        return summary(urls: pending.urls, text: pending.text)
    }

    private func choosePrompt() -> DiskImagePrompt {
        let live = fresh(state)
        let list = live?.devices ?? []
        let rows = list.map {
            SendCardContent.Row(id: $0.fingerprint, alias: $0.alias,
                                model: $0.deviceModel ?? kind(of: $0), symbol: symbol(of: $0))
        }
        let title = list.isEmpty ? L10n.t("Looking for devices…") : L10n.t("Send to…")
        let detail = list.isEmpty
            ? L10n.t("Open LocalSend on the other device.") : pendingSummary
        return DiskImagePrompt(iconPath: iconPath(nil), title: title, detail: detail, style: .ask,
                               send: SendCardContent(rows: rows, scanning: live?.scanning ?? false))
    }

    private func showChoose(refreshing: Bool) {
        let prompt = choosePrompt()
        lastChoose = prompt
        show(prompt, as: .choose)
        scheduleExpiry(after: 30)
        if refreshing || fresh(state)?.devices.isEmpty == true { refreshDevices() }
    }

    /// Devices come and go, and a scan starts and ends, while the card is up.
    private func refreshChooseCard() {
        let prompt = choosePrompt()
        guard prompt != lastChoose else { return }
        lastChoose = prompt
        _ = present?(prompt)
    }

    private func beginSending(to device: ShareDevice, urls: [URL], text: String?) {
        // A request waiting for an answer keeps the card; the Send ring still shows this.
        if case .incoming = card { return }
        sendPeer = device
        sendSummary = summary(urls: urls, text: text)
        sendBaseline = Set(state?.transfers.map(\.id) ?? [])
        sendTransferID = nil
        sendFinished = false
        cardTimer?.invalidate()
        show(sendingPrompt(nil, peer: device), as: .sending)
        // The hub should list the transfer at once; if it never does, let go.
        scheduleExpiry(after: 20)
    }

    private func sendingPrompt(_ transfer: ShareTransfer?, peer: ShareDevice) -> DiskImagePrompt {
        let deviceSymbol = symbol(of: peer)
        func prompt(_ title: String, _ detail: String, _ style: DiskImagePrompt.Style,
                    _ state: SendCardContent.Transfer.State, symbol: String,
                    fraction: Double? = nil, cancel: Bool = false) -> DiskImagePrompt {
            DiskImagePrompt(iconPath: iconPath(nil), title: title, detail: detail, style: style,
                            send: SendCardContent(
                                rows: [], scanning: false,
                                transfer: .init(state: state, symbol: symbol, fraction: fraction,
                                                canCancel: cancel)))
        }
        let waiting = L10n.t("Waiting for \(peer.alias) to accept…")
        guard let transfer else {
            return prompt(waiting, sendSummary, .working, .waiting, symbol: deviceSymbol, cancel: false)
        }
        switch transfer.state {
        case "active":
            var detail = L10n.t("\(size(transfer.doneBytes)) of \(size(transfer.totalBytes))")
            if transfer.filesTotal > 1 {
                detail += " · " + L10n.t("\(min(transfer.filesDone + 1, transfer.filesTotal)) of \(transfer.filesTotal) files")
            } else if let current = transfer.current, !current.isEmpty {
                detail = current + " · " + detail
            }
            let fraction = transfer.totalBytes > 0
                ? min(max(Double(transfer.doneBytes) / Double(transfer.totalBytes), 0), 1) : 0
            return prompt(L10n.t("Sending to \(peer.alias)"), detail, .working, .active,
                          symbol: deviceSymbol, fraction: fraction, cancel: true)
        case "done":
            let count = max(transfer.filesTotal, 1)
            let detail = L10n.t("To \(peer.alias)") + " · "
                + (count == 1 ? sendSummary : L10n.t("\(count) files"))
            return prompt(L10n.t("Sent"), detail, .done, .done, symbol: "checkmark.circle")
        case "declined":
            return prompt(L10n.t("Declined"), L10n.t("\(peer.alias) declined. Nothing was sent."),
                          .problem, .problem, symbol: "hand.raised")
        case "failed":
            return prompt(L10n.t("Couldn't send"), transfer.error ?? "", .problem, .problem,
                          symbol: "exclamationmark.triangle")
        default:
            return prompt(waiting, sendSummary, .working, .waiting, symbol: deviceSymbol, cancel: true)
        }
    }

    /// Follows the transfer this card started, from "waiting" to its end.
    private func updateSending(_ new: ShareState) {
        guard let peer = sendPeer else { return }
        if sendTransferID == nil {
            guard let started = new.transfers.last(where: {
                $0.direction == "send" && !sendBaseline.contains($0.id)
            }) else { return }
            sendTransferID = started.id
            cardTimer?.invalidate()
        }
        guard let transfer = new.transfers.first(where: { $0.id == sendTransferID }) else { return }
        switch transfer.state {
        case "cancelled":
            seenFinished.insert(transfer.id)
            clearCard()
        case "done", "declined", "failed":
            seenFinished.insert(transfer.id)
            sendFinished = true
            let problem = transfer.state != "done"
            _ = present?(sendingPrompt(transfer, peer: peer))
            if !cardHovered { scheduleExpiry(after: problem ? 6 : 2) }
        default:
            _ = present?(sendingPrompt(transfer, peer: peer))
        }
    }
}
