import Combine
import Foundation
import ServiceManagement
import os

/// What the user has chosen, kept in `UserDefaults`.
@MainActor
final class Preferences: ObservableObject {
    static let showUsagePaceKey = "showUsagePace"

    /// Provider IDs that currently have a ring. Stored as the ones that are
    /// on, so a provider added later stays off until someone switches it on —
    /// Claude and Codex excepted, which still default on as a family.
    @Published var connectedProviders: Set<String> {
        didSet { defaults.set(Array(connectedProviders), forKey: Keys.connected) }
    }

    /// Every provider id this copy has already decided on, so a later version's
    /// new provider is recognised as new rather than as "never chosen".
    @Published private(set) var seenProviders: Set<String> {
        didSet { defaults.set(Array(seenProviders), forKey: Keys.seen) }
    }

    /// Loaded-model cells hide without stopping the shared runtime. Stored as
    /// the ones that are off: a model Ollama or LM Studio loads later stays
    /// visible until someone hides it. Providers cannot share this list —
    /// their on-list treats absence as off.
    @Published private(set) var disabledModels: Set<String> {
        didSet { defaults.set(Array(disabledModels), forKey: Keys.disabledModels) }
    }

    /// The old hidden-providers list, kept only until `reconcile` can invert it
    /// against the ids actually on this Mac.
    private var pendingHidden: Set<String>?

    /// Providers whose threshold alerts are muted. Stored as the muted set so
    /// a provider added later alerts by default. Connection is stored the
    /// other way: the ones that are on.
    @Published var mutedAlertProviders: Set<String> {
        didSet { defaults.set(Array(mutedAlertProviders), forKey: Keys.mutedAlerts) }
    }

    /// Names given to accounts in Settings, by provider id. Only the ones
    /// set: an account with no entry keeps the name its provider gives.
    @Published var accountNicknames: [String: String] {
        didSet { defaults.set(accountNicknames, forKey: Keys.accountNicknames) }
    }

    /// The order the user has dragged the rings into, as provider ids.
    ///
    /// Stored as the ids actually placed rather than as every id known at the
    /// time: providers are discovered at launch — Claude Code contributes one
    /// per `~/.claude-<slug>` — so an exhaustive list written today is wrong
    /// the moment a profile appears. `ProviderOrder` reconciles the two,
    /// forgivingly in both directions.
    ///
    /// Empty means never chosen, which is not the same as having chosen the
    /// order the app ships with: keeping them distinct is what lets a later
    /// version change the built-in order for everyone who never had an opinion.
    @Published var providerOrder: [String] {
        didSet { defaults.set(providerOrder, forKey: Keys.order) }
    }

    /// How much of itself the notch shows at rest.
    @Published var notchVisibility: NotchVisibility {
        didSet { defaults.set(notchVisibility.rawValue, forKey: Keys.visibility) }
    }

    /// Whether a frontmost full-screen app folds the notch away.
    @Published var foldsForFullScreen: Bool {
        didSet { defaults.set(foldsForFullScreen, forKey: Keys.foldsForFullScreen) }
    }

    /// Which screen edge the notch is welded to.
    @Published var notchEdge: NotchEdge {
        didSet { defaults.set(notchEdge.rawValue, forKey: Keys.edge) }
    }

    /// How large the notch is drawn, as one of three named sizes.
    ///
    /// Ignored while `usesCustomNotchScale` is on — the two are kept apart
    /// rather than collapsed into one number so that switching back to the
    /// presets returns to the preset you last chose, instead of to whichever
    /// preset happens to sit nearest the slider.
    @Published var notchSize: NotchSize {
        didSet { defaults.set(notchSize.rawValue, forKey: Keys.size) }
    }

    /// Whether the slider decides the size rather than the three presets.
    @Published var usesCustomNotchScale: Bool {
        didSet { defaults.set(usesCustomNotchScale, forKey: Keys.usesCustomSize) }
    }

    /// The slider's own multiplier, honoured only when the slider is in
    /// charge. Clamped on the way in: a value typed straight into `defaults`
    /// could otherwise shrink the notch to nothing or blow it off the screen.
    @Published var customNotchScale: Double {
        didSet {
            let clamped = min(max(customNotchScale, Self.customScaleRange.lowerBound),
                              Self.customScaleRange.upperBound)
            if clamped != customNotchScale { customNotchScale = clamped; return }
            defaults.set(customNotchScale, forKey: Keys.customSize)
        }
    }

    /// Where the slider may go. Wider than the presets at both ends, but not
    /// unbounded.
    ///
    /// The floor was three quarters, because below that the percentage under
    /// each ring stopped being readable — and that is the one thing the notch
    /// exists for. The reading is a setting of its own now (`showsNotchReadings`),
    /// so anyone who wants the notch smaller than the type allows can turn the
    /// type off and keep the rings, which read as colour and fill at any size.
    /// Half is as small as a ring stays legible as a ring.
    static let customScaleRange: ClosedRange<Double> = 0.5...1.5

    /// What the notch is actually drawn at, whichever control is in charge.
    var notchScale: CGFloat {
        usesCustomNotchScale ? CGFloat(customNotchScale) : notchSize.scale
    }

    /// The display the notch stays on, or the original focus-following behaviour.
    ///
    /// Only meaningful in `NotchScreenScope.main` — pinning a display and
    /// drawing on every display are two different questions, and this answers
    /// the first one. `all` ignores it entirely: there is no "the" display to
    /// pin when every one of them gets its own notch.
    @Published var displayPreference: DisplayPreference {
        didSet {
            switch displayPreference {
            case .followActiveWindow:
                defaults.removeObject(forKey: Keys.display)
            case .display(let id):
                defaults.set(id, forKey: Keys.display)
            }
        }
    }

    /// Which displays get a notch when more than one is connected.
    @Published var notchScope: NotchScreenScope {
        didSet { defaults.set(notchScope.rawValue, forKey: Keys.scope) }
    }

    /// Where along that edge the notch sits, nudged from the centred default
    /// by ⌥-dragging the pill. One value per edge — moving it on the right
    /// should not silently relocate it on the top too — so this is read and
    /// written through `offset(for:)`/`setOffset(_:for:)` rather than exposed
    /// as a single published value the way the other settings are.
    func offset(for edge: NotchEdge) -> CGFloat {
        CGFloat(defaults.double(forKey: Self.offsetKey(for: edge)))
    }

    func setOffset(_ offset: CGFloat, for edge: NotchEdge) {
        defaults.set(Double(offset), forKey: Self.offsetKey(for: edge))
    }

    private static func offsetKey(for edge: NotchEdge) -> String { "notchOffset.\(edge.rawValue)" }

    @Published var resetTimeFormat: ResetTimeFormat {
        didSet { defaults.set(resetTimeFormat.rawValue, forKey: Keys.resetTimeFormat) }
    }

    @Published var showUsagePace: Bool {
        didSet { defaults.set(showUsagePace, forKey: Self.showUsagePaceKey) }
    }

    /// Whether pointing at a ring, or opening the menu bar's menu, refuses every
    /// reading a provider is holding and asks the provider itself.
    ///
    /// Off by default, and it has to be: it is not strictly better. A look
    /// already asks for a live reading, which is served from a cache only while
    /// that cache is newer than a couple of minutes. This spends a request even
    /// when the cache was written seconds ago — and on a provider that rate
    /// limits, one request too many is answered with a back-off that then holds
    /// a number older than the cache would have been. Worth having for somebody
    /// comparing Codenotch against a vendor's own dashboard figure by figure;
    /// not worth making everybody pay for.
    @Published var asksProviderOnLook: Bool {
        didSet { defaults.set(asksProviderOnLook, forKey: Keys.asksProviderOnLook) }
    }

    /// Whether Pulse looks for a newer release, at most every six hours. On by
    /// default. It only checks: nothing is downloaded or installed until Update.
    @Published var autoUpdateCheck: Bool {
        didSet { defaults.set(autoUpdateCheck, forKey: Keys.autoUpdateCheck) }
    }

    // Pulse fork: Mac conveniences. Every one is off until chosen, and each
    // needs Accessibility (see `Conveniences/ConveniencesService.swift`).
    @Published var convFinderCutPaste: Bool {
        didSet { defaults.set(convFinderCutPaste, forKey: Keys.convFinderCutPaste) }
    }
    @Published var convWindowMaximizer: Bool {
        didSet { defaults.set(convWindowMaximizer, forKey: Keys.convWindowMaximizer) }
    }
    @Published var convDockClickMinimize: Bool {
        didSet { defaults.set(convDockClickMinimize, forKey: Keys.convDockClickMinimize) }
    }
    /// Fn works as Command (see `Keyboard/FnCommand.swift`).
    @Published var convFnCommand: Bool {
        didSet { defaults.set(convFnCommand, forKey: Keys.convFnCommand) }
    }
    @Published var convAutoQuit: Bool {
        didSet { defaults.set(convAutoQuit, forKey: Keys.convAutoQuit) }
    }
    /// Bundle ids opted in to Auto Quit. Empty until the person adds one.
    /// Offer to install an app from a mounted disk image and eject it (on by
    /// default), and move the downloaded .dmg to the Trash afterwards (off).
    @Published var convDiskImageInstaller: Bool {
        didSet { defaults.set(convDiskImageInstaller, forKey: Keys.convDiskImageInstaller) }
    }
    /// Install automatically, with no prompt, when the image holds one signed,
    /// notarized app that is not installed and not running (on by default).
    /// Everything else is asked in the notch.
    @Published var convDiskImageAuto: Bool {
        didSet { defaults.set(convDiskImageAuto, forKey: Keys.convDiskImageAuto) }
    }
    /// Also replace an installed copy with a lower version, with no prompt, when
    /// the image's app is signed, notarized, not running and the same bundle id
    /// (off by default). Undo restores the old copy from the Trash.
    @Published var convDiskImageAutoUpdate: Bool {
        didSet { defaults.set(convDiskImageAutoUpdate, forKey: Keys.convDiskImageAutoUpdate) }
    }
    @Published var convDiskImageTrashDownload: Bool {
        didSet { defaults.set(convDiskImageTrashDownload, forKey: Keys.convDiskImageTrashDownload) }
    }
    @Published var convAutoQuitApps: [String] {
        didSet { defaults.set(convAutoQuitApps, forKey: Keys.convAutoQuitApps) }
    }

    /// Pulse fork: the middle-click tool wheel (Conveniences/ToolWheel.swift).
    var toolWheelEnabled: Bool {
        get { defaults.object(forKey: "toolWheelEnabled") as? Bool ?? true }
        set { defaults.set(newValue, forKey: "toolWheelEnabled") }
    }

    /// Nearby sharing (LocalSend protocol, run by the hub): on by default.
    @Published var nearbyEnabled: Bool {
        didSet { defaults.set(nearbyEnabled, forKey: Keys.nearbyEnabled) }
    }
    /// How other devices list this Mac; empty means "<Mac name> (Pulse)".
    @Published var nearbyAlias: String {
        didSet { defaults.set(nearbyAlias, forKey: Keys.nearbyAlias) }
    }
    /// Where received files go; empty means Downloads.
    @Published var nearbySaveFolder: String {
        didSet { defaults.set(nearbySaveFolder, forKey: Keys.nearbySaveFolder) }
    }
    /// Skip the question for devices accepted before (off by default).
    @Published var nearbyAcceptKnown: Bool {
        didSet { defaults.set(nearbyAcceptKnown, forKey: Keys.nearbyAcceptKnown) }
    }

    /// Pulse fork: the launcher is off until switched on.
    @Published var launcherEnabled: Bool {
        didSet { defaults.set(launcherEnabled, forKey: Keys.launcherEnabled) }
    }

    @Published var launcherHotkey: LauncherHotkeyChoice {
        didSet { defaults.set(launcherHotkey.rawValue, forKey: Keys.launcherHotkey) }
    }

    /// Launcher items the hub edits: pinned apps, file folders, app and command
    /// hotkeys, quicklinks, snippets, commands. One JSON string; see `LauncherConfig`.
    @Published var launcherConfigJSON: String {
        didSet { defaults.set(launcherConfigJSON, forKey: Keys.launcherConfig) }
    }
    /// Launcher sources, each off or on in the hub. Clipboard and currency are
    /// off until chosen: one stores what you copy, the other fetches rates.
    @Published var launcherClipboard: Bool {
        didSet { defaults.set(launcherClipboard, forKey: Keys.launcherClipboard) }
    }
    @Published var launcherCurrency: Bool {
        didSet { defaults.set(launcherCurrency, forKey: Keys.launcherCurrency) }
    }
    @Published var launcherDictionary: Bool {
        didSet { defaults.set(launcherDictionary, forKey: Keys.launcherDictionary) }
    }
    @Published var launcherShortcuts: Bool {
        didSet { defaults.set(launcherShortcuts, forKey: Keys.launcherShortcuts) }
    }

    /// Pulse fork: window-management shortcuts (`Conveniences/Windows/`), off
    /// until switched on. `windowHotkeys` maps an action id to its shortcut
    /// text; a missing id means the action's default, "" means none.
    @Published var windowManagementEnabled: Bool {
        didSet { defaults.set(windowManagementEnabled, forKey: Keys.windowManagementEnabled) }
    }

    @Published var windowHotkeys: [String: String] {
        didSet { defaults.set(windowHotkeys, forKey: Keys.windowHotkeys) }
    }

    /// Whether Claude's big ring shows the day's share of the weekly limit
    /// instead of the session. See `DailyPace`.
    @Published var claudeDailyPaceRing: Bool {
        didSet { defaults.set(claudeDailyPaceRing, forKey: Keys.claudeDailyPaceRing) }
    }

    /// Whether the big ring shows the weekly limit instead of the shorter
    /// window, for every provider that has both. See `WeeklyHeadline`.
    @Published var weeklyHeadline: Bool {
        didSet { defaults.set(weeklyHeadline, forKey: Keys.weeklyHeadline) }
    }

    /// Whether Spark and code-review Codex windows appear in the hover card.
    /// On by default so a first launch shows them; the ring still follows
    /// the main Codex window either way.
    @Published var showCodexExtraLimits: Bool {
        didSet { defaults.set(showCodexExtraLimits, forKey: Keys.showCodexExtraLimits) }
    }

    /// Whether the weekly limit gets a ring of its own, and where it sits.
    /// Whether each ring carries its percentage under it, on every edge.
    ///
    /// On by default, which is what the notch has always drawn everywhere but
    /// the strip beside a Mac's own cutout. There it costs ring size, because
    /// the bar is the cutout's depth and one ring already fills it — see
    /// `NotchViewModel.showsCellReading`.
    @Published var showsNotchReadings: Bool {
        didSet { defaults.set(showsNotchReadings, forKey: Keys.showsNotchReadings) }
    }

    @Published var weeklyRingDashed: Bool {
        didSet { defaults.set(weeklyRingDashed, forKey: Keys.weeklyRingDashed) }
    }

    /// Whether the reading under each ring adds the weekly ring's percentage,
    /// as "30%/70%". Only while the weekly ring is on.
    @Published var weeklyReading: Bool {
        didSet { defaults.set(weeklyReading, forKey: Keys.weeklyReading) }
    }

    @Published var weeklyRing: WeeklyRing {
        didSet { defaults.set(weeklyRing.rawValue, forKey: Keys.weeklyRing) }
    }


    /// The colour used for positive usage and active-work indicators.
    @Published var accentColor: AccentColorChoice {
        didSet { defaults.set(accentColor.rawValue, forKey: Keys.accentColor) }
    }

    /// The material the expanded notch, tooltip and settings orb are painted with.
    @Published var notchSurfaceStyle: NotchSurfaceStyle {
        didSet { defaults.set(notchSurfaceStyle.rawValue, forKey: Keys.notchSurfaceStyle) }
    }

    @Published var watchLimit: Double {
        didSet {
            let clamped = min(max(watchLimit, 0.01), criticalLimit - 0.01)
            if clamped != watchLimit { watchLimit = clamped; return }
            defaults.set(watchLimit, forKey: Keys.watchLimit)
        }
    }

    @Published var criticalLimit: Double {
        didSet {
            let clamped = min(max(criticalLimit, watchLimit + 0.01), 1.0)
            if clamped != criticalLimit { criticalLimit = clamped; return }
            defaults.set(criticalLimit, forKey: Keys.criticalLimit)
        }
    }

    /// Hard step or continuous ramp — see `ColorTransitionStyle`.
    @Published var colorTransitionStyle: ColorTransitionStyle {
        didSet { defaults.set(colorTransitionStyle.rawValue, forKey: Keys.colorTransitionStyle) }
    }

    /// The language the app itself speaks.
    ///
    /// `.system` follows the Mac. Written through `L10n.apply` so the store
    /// and the change notification stay a single write.
    @Published var language: AppLanguage {
        didSet { L10n.apply(language) }
    }

    /// Where every notification goes: the notch, or a banner. One choice for
    /// all of them; which events notify stays a switch per event.
    @Published var notificationChannel: NotificationChannel {
        didSet { defaults.set(notificationChannel.rawValue, forKey: Keys.notificationChannel) }
    }

    /// Open the notch for a few seconds when an agent stops working.
    ///
    /// On by default: the app already knows the moment a session ends, and a
    /// user who installed a thing that watches sessions is unlikely to want
    /// that particular fact kept from them. It is a peek, not a notification —
    /// nothing to dismiss, and it takes no focus.
    @Published var announceSessionEnd: Bool {
        didSet { defaults.set(announceSessionEnd, forKey: Keys.announceSessionEnd) }
    }

    /// How long that peek lasts.
    @Published var peekDuration: PeekDuration {
        didSet { defaults.set(peekDuration.rawValue, forKey: Keys.peekDuration) }
    }

    /// Sound the system alert alongside the peek.
    ///
    /// Separate from the peek because they fail differently: the peek is no use
    /// on another Space or behind a full-screen window, and the sound is no use
    /// in a meeting. Kept switchable on its own so neither one forces the
    /// other.
    @Published var sessionEndSound: Bool {
        didSet { defaults.set(sessionEndSound, forKey: Keys.sessionEndSound) }
    }

    /// Which sound a finished turn makes.
    @Published var sessionEndSoundName: String {
        didSet { defaults.set(sessionEndSoundName, forKey: Keys.sessionEndSoundName) }
    }

    /// And which one a session blocked on you makes.
    ///
    /// A separate choice because the two say different things — one is "that's
    /// done", the other is "you are the hold-up" — and a single sound for both
    /// makes the second one easy to ignore.
    @Published var sessionBlockedSoundName: String {
        didSet { defaults.set(sessionBlockedSoundName, forKey: Keys.sessionBlockedSoundName) }
    }

    /// Show a notification modal from the notch when a provider's limit resets.
    @Published var announceUsageReset: Bool {
        didSet { defaults.set(announceUsageReset, forKey: Keys.announceUsageReset) }
    }

    /// Sound an alert alongside the usage reset notification modal.
    @Published var usageResetSound: Bool {
        didSet { defaults.set(usageResetSound, forKey: Keys.usageResetSound) }
    }

    /// Which sound a usage reset notification makes.
    @Published var usageResetSoundName: String {
        didSet { defaults.set(usageResetSoundName, forKey: Keys.usageResetSoundName) }
    }

    /// Show a notification modal from the notch when a provider's session limit is reached.
    @Published var announceSessionLimitReached: Bool {
        didSet { defaults.set(announceSessionLimitReached, forKey: Keys.announceSessionLimitReached) }
    }

    /// Show a notification modal from the notch when a provider's weekly limit is reached.
    @Published var announceWeeklyLimitReached: Bool {
        didSet { defaults.set(announceWeeklyLimitReached, forKey: Keys.announceWeeklyLimitReached) }
    }

    /// Sound an alert alongside the limit reached notification modal.
    @Published var limitReachedSound: Bool {
        didSet { defaults.set(limitReachedSound, forKey: Keys.limitReachedSound) }
    }

    /// Which sound a limit reached notification makes.
    @Published var limitReachedSoundName: String {
        didSet { defaults.set(limitReachedSoundName, forKey: Keys.limitReachedSoundName) }
    }

    /// The version whose changes have already been shown.
    ///
    /// Written when the What's New dialogue is dismissed rather than when it
    /// opens, so a crash in between cannot swallow the one launch it was going
    /// to appear on.
    @Published var lastSeenVersion: String? {
        didSet { defaults.set(lastSeenVersion, forKey: Keys.lastSeenVersion) }
    }

    @Published var launchAtLogin: Bool {
        didSet {
            guard launchAtLogin != Self.isRegisteredForLogin else { return }
            applyLaunchAtLogin()
        }
    }

    /// Set when the login-item request was refused, so the UI can say so rather
    /// than quietly flipping the switch back.
    @Published private(set) var launchAtLoginProblem: String?

    private let defaults: UserDefaults
    private enum Keys {
        /// The old off-list. Kept so a 1.9 install can invert it once.
        static let disconnected = "hiddenProviders"
        static let connected = "connectedProviders"
        static let seen = "seenProviders"
        static let disabledModels = "disabledModels"
        static let introducedOllama = "introducedOllama"
        static let autoUpdateCheck = "autoUpdateCheck"
        static let migratedOllamaID = "migratedOllamaLocalID"
        static let mutedAlerts = "mutedAlertProviders"
        static let accountNicknames = "accountNicknames"
        static let hasLaunched = "hasLaunchedBefore"
        static let visibility = "notchVisibility"
        static let foldsForFullScreen = "foldsForFullScreen"
        static let notificationChannel = "notificationChannel"
        static let edge = "notchEdge"
        // A new key, so there is nothing under the old app name to migrate.
        static let size = "notchSize"
        static let usesCustomSize = "usesCustomNotchScale"
        static let customSize = "customNotchScale"
        static let display = "notchDisplay"
        static let resetTimeFormat = "resetTimeFormat"
        static let asksProviderOnLook = "asksProviderOnLook"
        static let convFinderCutPaste = "convFinderCutPaste"
        static let convWindowMaximizer = "convWindowMaximizer"
        static let convDockClickMinimize = "convDockClickMinimize"
        static let convFnCommand = "convFnCommand"
        static let convAutoQuit = "convAutoQuit"
        static let convAutoQuitApps = "convAutoQuitApps"
        static let convDiskImageInstaller = "convDiskImageInstaller"
        static let convDiskImageTrashDownload = "convDiskImageTrashDownload"
        static let convDiskImageAuto = "convDiskImageAuto"
        static let convDiskImageAutoUpdate = "convDiskImageAutoUpdate"
        static let nearbyEnabled = "nearbyEnabled"
        static let nearbyAlias = "nearbyAlias"
        static let nearbySaveFolder = "nearbySaveFolder"
        static let nearbyAcceptKnown = "nearbyAcceptKnown"
        static let launcherEnabled = "launcherEnabled"
        static let launcherHotkey = "launcherHotkey"
        static let launcherConfig = "launcherConfig"
        static let launcherClipboard = "launcherClipboard"
        static let launcherCurrency = "launcherCurrency"
        static let launcherDictionary = "launcherDictionary"
        static let launcherShortcuts = "launcherShortcuts"
        static let windowManagementEnabled = "windowManagementEnabled"
        static let windowHotkeys = "windowHotkeys"
        static let scope = "notchScope"
        static let accentColor = "accentColor"
        // A new key, so there is nothing under the old app name to migrate.
        static let weeklyRing = "weeklyRing"
        static let weeklyRingDashed = "weeklyRingDashed"
        static let showsNotchReadings = "showsNotchReadings"
        static let weeklyReading = "weeklyReading"
        static let claudeDailyPaceRing = "claudeDailyPaceRing"
        static let weeklyHeadline = "weeklyHeadline"
        static let notchSurfaceStyle = "notchSurfaceStyle"
        static let watchLimit = "watchLimit"
        static let criticalLimit = "criticalLimit"
        static let colorTransitionStyle = "colorTransitionStyle"
        static let lastSeenVersion = "lastSeenVersion"
        static let order = "providerOrder"
        static let announceSessionEnd = "announceSessionEnd"
        static let sessionEndSound = "sessionEndSound"
        static let peekDuration = "peekDuration"
        static let sessionEndSoundName = "sessionEndSoundName"
        static let sessionBlockedSoundName = "sessionBlockedSoundName"
        static let announceUsageReset = "announceUsageReset"
        static let usageResetSound = "usageResetSound"
        static let usageResetSoundName = "usageResetSoundName"
        static let announceSessionLimitReached = "announceSessionLimitReached"
        static let announceWeeklyLimitReached = "announceWeeklyLimitReached"
        static let limitReachedSound = "limitReachedSound"
        static let limitReachedSoundName = "limitReachedSoundName"
        static let showCodexExtraLimits = "showCodexExtraLimits"
    }

    /// Whether extra Codex windows (Spark, code review) are shown, read off
    /// the main actor.
    ///
    /// The Codex provider is an actor and asks for this on every fetch, and
    /// `@Published` state is main-actor-isolated where `UserDefaults` is
    /// thread-safe — so the provider reads the store, not the object. Absent
    /// means on: a first launch should show them. `bool(forKey:)` cannot stand
    /// in for that default — it answers false for a key that was never written.
    nonisolated static func storedShowCodexExtraLimits(
        defaults: UserDefaults = .standard
    ) -> Bool {
        defaults.object(forKey: Keys.showCodexExtraLimits) as? Bool ?? true
    }

    /// True the very first time this copy runs, and never again.
    ///
    /// Deliberately *not* inferred from "there are no readings yet" — that is
    /// also true of someone who switched every provider off, and re-introducing
    /// them to the app every launch would be worse than never introducing them
    /// at all.
    let isFirstLaunch: Bool

    /// The bundle identifier before the app was renamed to Codenotch.
    ///
    /// A bundle id is the name of the defaults domain, so renaming the app
    /// silently moved every setting to a new, empty one — connection choices,
    /// the notch's mode, the archived readings, all apparently lost. Copying
    /// the old domain across once is the difference between a rename and what
    /// looks like a reset.
    private static let previousDomain = "com.vinz.usagenotch"

    static func migrateFromPreviousName(into defaults: UserDefaults = .standard,
                                        from domain: String = previousDomain) {
        // The emptiness test has to be about the object being written to, not
        // about `Bundle.main` — under test those are different domains, and the
        // first version happily copied real settings into a test's scratch
        // suite. `hasLaunched` is the sentinel: `Preferences.init` sets it, so
        // its absence means nothing has ever used this domain.
        guard defaults.object(forKey: Keys.hasLaunched) == nil,
              let old = defaults.persistentDomain(forName: domain), !old.isEmpty
        else { return }

        for (key, value) in old { defaults.set(value, forKey: key) }
        Log.usage.info("migrated \(old.count) settings from the previous app name")
    }

    init(defaults: UserDefaults = .standard) {
        self.defaults = defaults
        self.isFirstLaunch = !defaults.bool(forKey: Keys.hasLaunched)
        defaults.set(true, forKey: Keys.hasLaunched)
        // Only the earlier local integration used this sentinel. Keep unrelated
        // provider IDs untouched when upgrading from upstream.
        if defaults.bool(forKey: Keys.introducedOllama),
           !defaults.bool(forKey: Keys.migratedOllamaID) {
            for key in [Keys.disconnected, Keys.order, Keys.mutedAlerts] {
                var seen = Set<String>()
                let migrated = (defaults.stringArray(forKey: key) ?? []).map { id in
                    if id == "ollama" { return "ollama-local" }
                    if id.hasPrefix("ollama:model:") {
                        return "ollama-local:model:" + id.dropFirst("ollama:model:".count)
                    }
                    return id
                }.filter { seen.insert($0).inserted }
                defaults.set(migrated, forKey: key)
            }
            defaults.set(true, forKey: Keys.migratedOllamaID)
        }
        let storedConnected = defaults.stringArray(forKey: Keys.connected)
        let storedSeen = Set(defaults.stringArray(forKey: Keys.seen) ?? [])
        let connected: Set<String>
        let seen: Set<String>
        let hidden: Set<String>?
        if let storedConnected {
            connected = Set(storedConnected)
            seen = storedSeen.isEmpty ? connected : storedSeen
            hidden = nil
        } else if defaults.object(forKey: Keys.disconnected) != nil {
            // An empty off-list is still a choice: everyone was on.
            hidden = Set(defaults.stringArray(forKey: Keys.disconnected) ?? [])
            connected = []
            seen = storedSeen
        } else if !self.isFirstLaunch || defaults.bool(forKey: Keys.introducedOllama) {
            // Launched before this key existed, and never hid anyone.
            hidden = []
            connected = []
            seen = storedSeen
        } else {
            hidden = nil
            connected = []
            seen = storedSeen
        }
        self.connectedProviders = connected.filter { !Self.isModelCell($0) }
        self.seenProviders = seen.filter { !Self.isModelCell($0) }
        self.pendingHidden = hidden
        let models: Set<String>
        if let storedDisabled = defaults.stringArray(forKey: Keys.disabledModels) {
            models = Set(storedDisabled)
        } else {
            // Model cells that lived on the old off-list stay off. Read the
            // leftover even after `connectedProviders` exists: an earlier
            // invert left those ids in `hiddenProviders` and then ignored them.
            let leftover = hidden ?? Set(defaults.stringArray(forKey: Keys.disconnected) ?? [])
            models = leftover.filter(Self.isModelCell)
        }
        self.disabledModels = models
        defaults.set(Array(models), forKey: Keys.disabledModels)
        self.mutedAlertProviders = Set(defaults.stringArray(forKey: Keys.mutedAlerts) ?? [])
        self.accountNicknames = defaults.dictionary(forKey: Keys.accountNicknames) as? [String: String] ?? [:]
        // Absent means never chosen, which is the hover behaviour the app was
        // designed around — not hidden, which would make a fresh install look
        // like it failed to start.
        self.notchVisibility = defaults.string(forKey: Keys.visibility)
            // Pulse fork: the notch is always on screen by default.
            .flatMap(NotchVisibility.init(rawValue:)) ?? .alwaysShow
        // Absent means the fold that has shipped since full-screen detection
        // exists — the setting silences it, it does not introduce it.
        self.foldsForFullScreen = defaults.object(forKey: Keys.foldsForFullScreen) as? Bool ?? true
        // The notch, because that is what every earlier version did; a banner
        // is the choice of someone who found the notch too quiet.
        self.notificationChannel = defaults.string(forKey: Keys.notificationChannel)
            .flatMap(NotificationChannel.init(rawValue:)) ?? .notch
        // The right edge is where the notch has always been, and it is the one
        // side of a Mac that no system chrome claims by default.
        self.notchEdge = defaults.string(forKey: Keys.edge)
            .flatMap(NotchEdge.init(rawValue:)) ?? .right
        // Medium is the design frame at 1:1, so an install that predates this
        // choice keeps exactly the notch it already had.
        self.notchSize = defaults.string(forKey: Keys.size)
            // Pulse fork: small by default.
            .flatMap(NotchSize.init(rawValue:)) ?? .small
        // Absent means never chosen, and the presets are what every earlier
        // version had — so the slider is opt-in rather than the default.
        self.usesCustomNotchScale = defaults.bool(forKey: Keys.usesCustomSize)
        let stored = defaults.object(forKey: Keys.customSize) as? Double
        self.customNotchScale = stored.map {
            min(max($0, Self.customScaleRange.lowerBound), Self.customScaleRange.upperBound)
        } ?? 1
        self.displayPreference = defaults.string(forKey: Keys.display)
            .map(DisplayPreference.display) ?? .followActiveWindow
        self.resetTimeFormat = defaults.string(forKey: Keys.resetTimeFormat)
            .flatMap(ResetTimeFormat.init(rawValue:)) ?? .automatic
        self.showUsagePace = defaults.bool(forKey: Self.showUsagePaceKey)
        // Off by default: see the property. A request spent on every look is a
        // choice, and on a rate-limited provider it can cost freshness rather
        // than buy it.
        self.asksProviderOnLook = defaults.bool(forKey: Keys.asksProviderOnLook)
        // On unless the user has switched it off: absent means never chosen.
        self.autoUpdateCheck = defaults.object(forKey: Keys.autoUpdateCheck) as? Bool ?? true
        self.convFinderCutPaste = defaults.object(forKey: Keys.convFinderCutPaste) as? Bool ?? true
        self.convWindowMaximizer = defaults.bool(forKey: Keys.convWindowMaximizer)
        self.convDockClickMinimize = defaults.bool(forKey: Keys.convDockClickMinimize)
        self.convFnCommand = defaults.bool(forKey: Keys.convFnCommand)
        self.convAutoQuit = defaults.bool(forKey: Keys.convAutoQuit)
        self.convDiskImageInstaller = defaults.object(forKey: Keys.convDiskImageInstaller) as? Bool ?? true
        self.convDiskImageTrashDownload = defaults.bool(forKey: Keys.convDiskImageTrashDownload)
        self.convDiskImageAuto = defaults.object(forKey: Keys.convDiskImageAuto) as? Bool ?? true
        self.convDiskImageAutoUpdate = defaults.bool(forKey: Keys.convDiskImageAutoUpdate)
        self.convAutoQuitApps = defaults.stringArray(forKey: Keys.convAutoQuitApps) ?? []
        self.nearbyEnabled = defaults.object(forKey: Keys.nearbyEnabled) as? Bool ?? true
        self.nearbyAlias = defaults.string(forKey: Keys.nearbyAlias) ?? ""
        self.nearbySaveFolder = defaults.string(forKey: Keys.nearbySaveFolder) ?? ""
        self.nearbyAcceptKnown = defaults.bool(forKey: Keys.nearbyAcceptKnown)
        self.launcherEnabled = defaults.bool(forKey: Keys.launcherEnabled)
        self.launcherHotkey = defaults.string(forKey: Keys.launcherHotkey)
            .flatMap(LauncherHotkeyChoice.init(rawValue:)) ?? .optionSpace
        self.launcherConfigJSON = defaults.string(forKey: Keys.launcherConfig) ?? ""
        self.launcherClipboard = defaults.bool(forKey: Keys.launcherClipboard)
        self.launcherCurrency = defaults.bool(forKey: Keys.launcherCurrency)
        self.launcherDictionary = defaults.object(forKey: Keys.launcherDictionary) as? Bool ?? true
        self.launcherShortcuts = defaults.object(forKey: Keys.launcherShortcuts) as? Bool ?? true
        // Off by default, like every convenience.
        self.windowManagementEnabled = defaults.bool(forKey: Keys.windowManagementEnabled)
        self.windowHotkeys = defaults.dictionary(forKey: Keys.windowHotkeys) as? [String: String] ?? [:]
        // Off by default: it swaps what Claude's ring means, and that is a
        // choice for whoever budgets their week that way.
        self.claudeDailyPaceRing = defaults.bool(forKey: Keys.claudeDailyPaceRing)
        // Off by default for the same reason: it changes what every ring means.
        // Pulse fork: the weekly limit leads (main, outer ring); the
        // five-hour session is the thin inner ring.
        self.weeklyHeadline = defaults.object(forKey: Keys.weeklyHeadline) as? Bool ?? true
        self.showCodexExtraLimits = Self.storedShowCodexExtraLimits(defaults: defaults)
        // Absent means never chosen. Main display only, because that is what a
        // single-panel setup always did — all-displays on a fresh install
        // would put notches where none were expected.
        self.notchScope = defaults.string(forKey: Keys.scope)
            .flatMap(NotchScreenScope.init(rawValue:)) ?? .mainDisplay
        // Follow the Mac unless the user explicitly chooses a Codenotch colour.
        // Off by default: an extra arc in a 44pt circle is a change to how
        // every reading looks, and nobody asked for it on their behalf.
        self.weeklyRingDashed = defaults.object(forKey: Keys.weeklyRingDashed) as? Bool ?? false
        // Pulse fork: rings only by default; numbers are in the hover card.
        self.showsNotchReadings = defaults.object(forKey: Keys.showsNotchReadings) as? Bool ?? false
        self.weeklyReading = defaults.object(forKey: Keys.weeklyReading) as? Bool ?? false

        self.weeklyRing = defaults.string(forKey: Keys.weeklyRing)
            // Pulse fork: one cell per reading pair — the main ring outside,
            // the second reading as a thinner ring inside it.
            .flatMap(WeeklyRing.init(rawValue:)) ?? .inside
        // On unless turned off: it is how the notch is carried to another edge,
        // and a control that is missing by default is one nobody finds.
        self.accentColor = defaults.string(forKey: Keys.accentColor)
            .flatMap(AccentColorChoice.init(rawValue:)) ?? .green
        self.notchSurfaceStyle = defaults.string(forKey: Keys.notchSurfaceStyle)
            .flatMap(NotchSurfaceStyle.init(rawValue:)) ?? .glass
        let storedWatchLimit = defaults.object(forKey: Keys.watchLimit) as? Double ?? 0.70
        let storedCriticalLimit = defaults.object(forKey: Keys.criticalLimit) as? Double ?? 0.90
        // `didSet` does the clamping, and it does not run for these assignments,
        // so a stored pair that crossed over is repaired here instead.
        let critical = min(max(storedCriticalLimit, 0.02), 1.0)
        self.criticalLimit = critical
        self.watchLimit = min(max(storedWatchLimit, 0.01), critical - 0.01)
        self.colorTransitionStyle = defaults.string(forKey: Keys.colorTransitionStyle)
            .flatMap(ColorTransitionStyle.init(rawValue:)) ?? .hardStep
        // Absent means never chosen, which is follow-the-Mac.
        self.language = defaults.string(forKey: L10n.languageDefaultsKey)
            .flatMap(AppLanguage.init(rawValue:)) ?? .system
        // Absent means nothing has been shown yet, which is true of a fresh
        // install — so the current release reads as new to it.
        self.lastSeenVersion = defaults.string(forKey: Keys.lastSeenVersion)
        // Absent means never chosen, so the rings keep the order the app ships
        // with until someone drags one.
        self.providerOrder = defaults.stringArray(forKey: Keys.order) ?? []
        // Both default to on, so `bool(forKey:)` — which answers false for a
        // key that was never written — cannot stand in for the default.
        self.announceSessionEnd = defaults.object(forKey: Keys.announceSessionEnd) as? Bool ?? true
        self.sessionEndSound = defaults.object(forKey: Keys.sessionEndSound) as? Bool ?? false
        self.peekDuration = defaults.string(forKey: Keys.peekDuration)
            .flatMap(PeekDuration.init(rawValue:)) ?? .standard
        self.sessionEndSoundName = defaults.string(forKey: Keys.sessionEndSoundName)
            ?? SessionChime.defaultFinished
        self.sessionBlockedSoundName = defaults.string(forKey: Keys.sessionBlockedSoundName)
            ?? SessionChime.defaultBlocked
        self.announceUsageReset = defaults.object(forKey: Keys.announceUsageReset) as? Bool ?? true
        self.usageResetSound = defaults.object(forKey: Keys.usageResetSound) as? Bool ?? false
        self.usageResetSoundName = defaults.string(forKey: Keys.usageResetSoundName)
            ?? SessionChime.defaultFinished
        self.announceSessionLimitReached = defaults.object(forKey: Keys.announceSessionLimitReached) as? Bool ?? true
        self.announceWeeklyLimitReached = defaults.object(forKey: Keys.announceWeeklyLimitReached) as? Bool ?? true
        self.limitReachedSound = defaults.object(forKey: Keys.limitReachedSound) as? Bool ?? false
        self.limitReachedSoundName = defaults.string(forKey: Keys.limitReachedSoundName)
            ?? SessionChime.defaultBlocked
        // Read from the system rather than from our own store: the user can turn
        // this off in System Settings, and a remembered `true` would then be a lie.
        self.launchAtLogin = Self.isRegisteredForLogin
    }

    // MARK: Account names

    func nickname(for providerID: String) -> String? {
        accountNicknames[providerID]
    }

    /// Blank, or only spaces, goes back to the provider's own name.
    func setNickname(_ name: String, for providerID: String) {
        let trimmed = name.trimmingCharacters(in: .whitespaces)
        if trimmed.isEmpty {
            accountNicknames.removeValue(forKey: providerID)
        } else {
            accountNicknames[providerID] = trimmed
        }
    }

    // MARK: Threshold alerts

    func isMutedAlerts(for providerID: String) -> Bool {
        mutedAlertProviders.contains(providerID)
    }

    func setAlertsMuted(_ muted: Bool, for providerID: String) {
        if muted {
            mutedAlertProviders.insert(providerID)
        } else {
            mutedAlertProviders.remove(providerID)
        }
    }

    /// Claude and Codex stay on for a first install and for a newly discovered
    /// profile. Everyone else starts off.
    nonisolated static func isDefaultOnFamily(_ providerID: String) -> Bool {
        ClaudeProfile.isClaude(providerID: providerID)
            || CodexProfile.isCodex(providerID: providerID)
            || SystemProviders.isSystem(providerID: providerID)
    }

    /// Model cells are `providerID:model:…`. A new loaded model is not a new
    /// provider, and absence on the provider on-list cannot mean on for these.
    static func isModelCell(_ id: String) -> Bool {
        id.contains(":model:")
    }

    func isConnected(_ providerID: String) -> Bool {
        if Self.isModelCell(providerID) {
            return !disabledModels.contains(providerID)
        }
        if defaults.object(forKey: Keys.connected) != nil {
            return connectedProviders.contains(providerID)
        }
        if let hidden = pendingHidden {
            // Absence from the old off-list means on — for providers that
            // off-list could have named. MiniMax did not exist then, so
            // missing from it is not a choice to show it.
            if providerID == "minimax" { return false }
            return !hidden.contains(providerID)
        }
        return Self.isDefaultOnFamily(providerID)
    }

    func setConnected(_ connected: Bool, for providerID: String) {
        if Self.isModelCell(providerID) {
            if connected {
                disabledModels.remove(providerID)
            } else {
                disabledModels.insert(providerID)
            }
            return
        }
        if defaults.object(forKey: Keys.connected) == nil, let hidden = pendingHidden {
            let next = connected ? hidden.subtracting([providerID]) : hidden.union([providerID])
            pendingHidden = next
            defaults.set(Array(next), forKey: Keys.disconnected)
            seenProviders.insert(providerID)
            return
        }
        if defaults.object(forKey: Keys.connected) == nil {
            // First install: persist Claude and Codex as on, then apply this toggle.
            connectedProviders = ["claude", "codex"]
        }
        if connected {
            connectedProviders.insert(providerID)
        } else {
            connectedProviders.remove(providerID)
        }
        seenProviders.insert(providerID)
    }

    /// Fold this Mac's current provider ids into the stored on-list.
    ///
    /// First launch writes Claude and Codex. An upgrade from `hiddenProviders`
    /// inverts that off-list against `discoveredIDs`. After that, only a
    /// never-seen Claude or Codex id is added automatically. Model cells stay
    /// on `disabledModels` and are not inverted.
    func reconcile(discoveredIDs: [String]) {
        let discovered = Set(discoveredIDs.filter { !Self.isModelCell($0) })
        if defaults.object(forKey: Keys.connected) != nil {
            connectedProviders.subtract(connectedProviders.filter(Self.isModelCell))
            seenProviders.subtract(seenProviders.filter(Self.isModelCell))
            let novel = discovered.subtracting(seenProviders)
            for id in novel where Self.isDefaultOnFamily(id) {
                connectedProviders.insert(id)
            }
            seenProviders.formUnion(discovered)
            return
        }
        if let hidden = pendingHidden {
            // Invert the old off-list, then drop MiniMax: it did not exist
            // when that list was written, so absence from it is not "on".
            connectedProviders = discovered
                .subtracting(hidden.filter { !Self.isModelCell($0) })
                .subtracting(["minimax"])
            seenProviders = discovered
            pendingHidden = nil
            return
        }
        connectedProviders = Set(discovered.filter(Self.isDefaultOnFamily))
        seenProviders = discovered
    }

    /// What `UsageStore` still treats as the off-list, among ids it knows.
    func disconnectedIDs(among discovered: [String]) -> Set<String> {
        Set(discovered.filter { !isConnected($0) })
    }

    /// Record a new order, keeping the ids that are not on this Mac today.
    ///
    /// Settings can only show what was discovered at launch, so writing its
    /// list verbatim would quietly forget where a Claude profile sat the moment
    /// its directory was moved away — and put it back at the end when it
    /// returned, for something the user never did.
    func setProviderOrder(_ ids: [String]) {
        providerOrder = ProviderOrder.remember(ids, keeping: providerOrder)
    }

    /// Forget everything this app has stored and quit.
    ///
    /// Deleting an app on macOS leaves `~/Library` untouched, so reinstalling
    /// brings back the old readings, the old connection choices and the old
    /// first-launch flag — which is exactly what makes a reinstall look broken.
    /// Nothing but the app itself can clean that up, so the app has to offer it.
    ///
    /// Not tied to uninstalling: a reinstall is indistinguishable from an
    /// update, and wiping data on every Sparkle update would be catastrophic.
    /// It has to be something the user asks for.
    static func eraseAllData() {
        let bundleID = Bundle.main.bundleIdentifier ?? "dev.orthic.pulse"
        UserDefaults.standard.removePersistentDomain(forName: bundleID)
        UserDefaults.standard.synchronize()

        let library = FileManager.default.urls(for: .libraryDirectory, in: .userDomainMask).first
        for relative in ["Caches/\(bundleID)",
                         "WebKit/\(bundleID)",
                         "HTTPStorages/\(bundleID)",
                         "HTTPStorages/\(bundleID).binarycookies",
                         "Saved Application State/\(bundleID).savedState"] {
            if let url = library?.appendingPathComponent(relative) {
                try? FileManager.default.removeItem(at: url)
            }
        }
    }

    // MARK: - Login item

    static var isRegisteredForLogin: Bool {
        SMAppService.mainApp.status == .enabled
    }

    private func applyLaunchAtLogin() {
        do {
            if launchAtLogin {
                try SMAppService.mainApp.register()
            } else {
                try SMAppService.mainApp.unregister()
            }
            launchAtLoginProblem = nil
        } catch {
            // Commonly refused for an app running from a build directory rather
            // than /Applications, which is worth saying plainly.
            Log.usage.error("launch at login failed: \(error.localizedDescription, privacy: .public)")
            launchAtLoginProblem = L10n.t("macOS refused this — try moving Pulse to /Applications.")
            launchAtLogin = Self.isRegisteredForLogin
        }
    }
}
