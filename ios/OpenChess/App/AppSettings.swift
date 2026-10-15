import OpenChessCore
import SwiftUI
import UIKit

/// The app's settings, kept in the core's store (`setting` / `setSetting`).
@MainActor
final class AppSettings: ObservableObject {
    static let defaultServer = "https://chess.example.com"
    static let syncChoices: [Int] = [0, 1, 5, 10, 30, 60]

    private let store: Store

    /// The running settings, for `ocAppearance()` on sheets and covers.
    static var current: AppSettings?

    /// "dark" (the default, the esports look), "light" or "system".
    @Published var appearance: String { didSet { save("appearance", appearance) } }
    @Published var serverURL: String { didSet { save("serverURL", serverURL) } }
    /// "auto", "on" or "off".
    @Published var lowDataOverride: String { didSet { save("lowData", lowDataOverride) } }
    /// Minutes between syncs while open; 0 is manual.
    @Published var syncMinutes: Int { didSet { save("syncMinutes", String(syncMinutes)) } }
    @Published var photoUploads: Bool { didSet { save("photoUploads", photoUploads) } }
    @Published var photosWifiOnly: Bool { didSet { save("photosWifiOnly", photosWifiOnly) } }
    @Published var notifyMyMove: Bool { didSet { save("notifyMyMove", notifyMyMove) } }
    @Published var flipPassAndPlay: Bool { didSet { save("flipPassAndPlay", flipPassAndPlay) } }
    /// "", "accepted" or "declined": the photo licence, asked once.
    @Published var photoConsent: String { didSet { save("photoConsent", photoConsent) } }
    @Published var photoPublic: Bool { didSet { save("photoPublic", photoPublic) } }
    /// The signed-in player's username ("" when signed out).
    @Published var username: String { didSet { save("username", username) } }
    @Published var lastSyncMs: UInt64 { didSet { save("lastSyncMs", String(lastSyncMs)) } }
    /// The server's `now` from the last sync, sent back as `since`.
    @Published var syncSince: UInt64 { didSet { save("syncSince", String(syncSince)) } }
    @Published var botLevel: Int { didSet { save("botLevel", String(botLevel)) } }
    /// The APNs device token last sent to the server (hex).
    @Published var pushToken: String { didSet { save("pushToken", pushToken) } }
    /// Show (and speak) moves in plain English next to SAN.
    @Published var plainMoves: Bool { didSet { save("plainMoves", plainMoves) } }
    @Published var sounds: Bool { didSet { save("sounds", sounds) } }
    @Published var haptics: Bool { didSet { save("haptics", haptics) } }
    /// Speak the opponent's or computer's moves (and every move recorded on a real board).
    @Published var speakMoves: Bool { didSet { save("speakMoves", speakMoves) } }
    /// Streaming: "twitch", "youtube" or "custom".
    @Published var streamService: String { didSet { save("streamService", streamService) } }
    /// The custom service's RTMP URL (without the key).
    @Published var streamURL: String { didSet { save("streamURL", streamURL) } }
    /// Video bitrate in kbit/s.
    @Published var streamKbps: Int { didSet { save("streamKbps", String(streamKbps)) } }
    @Published var streamRecord: Bool { didSet { save("streamRecord", streamRecord) } }
    @Published var streamPortrait: Bool { didSet { save("streamPortrait", streamPortrait) } }

    init(store: Store) {
        self.store = store
        func read(_ key: String) -> String? {
            let v: String?
            do { v = try store.setting(key: key) } catch { v = nil }
            return v
        }
        func bool(_ key: String, _ def: Bool) -> Bool {
            guard let v = read(key) else { return def }
            return v == "1" || v == "true"
        }
        func int(_ key: String, _ def: Int) -> Int {
            guard let v = read(key), let n = Int(v) else { return def }
            return n
        }
        func u64(_ key: String) -> UInt64 {
            guard let v = read(key), let n = UInt64(v) else { return 0 }
            return n
        }
        appearance = read("appearance") ?? "dark"
        serverURL = read("serverURL") ?? AppSettings.defaultServer
        lowDataOverride = read("lowData") ?? "auto"
        syncMinutes = int("syncMinutes", 5)
        photoUploads = bool("photoUploads", true)
        photosWifiOnly = bool("photosWifiOnly", true)
        notifyMyMove = bool("notifyMyMove", false)
        flipPassAndPlay = bool("flipPassAndPlay", true)
        photoConsent = read("photoConsent") ?? ""
        photoPublic = bool("photoPublic", false)
        username = read("username") ?? ""
        lastSyncMs = u64("lastSyncMs")
        syncSince = u64("syncSince")
        botLevel = int("botLevel", 3)
        pushToken = read("pushToken") ?? ""
        plainMoves = bool("plainMoves", false)
        sounds = bool("sounds", true)
        haptics = bool("haptics", true)
        speakMoves = bool("speakMoves", false)
        streamService = read("streamService") ?? "twitch"
        streamURL = read("streamURL") ?? ""
        streamKbps = int("streamKbps", 4000)
        streamRecord = bool("streamRecord", false)
        streamPortrait = bool("streamPortrait", false)
    }

    private func save(_ key: String, _ value: String) {
        do {
            try store.setSetting(key: key, value: value)
        } catch {
            // Settings are a convenience; a failed write keeps the value in memory.
        }
    }

    private func save(_ key: String, _ value: Bool) {
        save(key, value ? "1" : "0")
    }

    /// The scheme to apply. The Info.plist starts the app dark, so "Match
    /// system" reads the system's own setting from the screen.
    var colorScheme: ColorScheme? {
        switch appearance {
        case "light": return .light
        case "system": return AppSettings.systemScheme()
        default: return .dark
        }
    }

    static func systemScheme() -> ColorScheme {
        let scenes = UIApplication.shared.connectedScenes.compactMap { $0 as? UIWindowScene }
        if let style = scenes.first?.screen.traitCollection.userInterfaceStyle {
            return style == .light ? .light : .dark
        }
        return .dark
    }

    var serverBase: URL {
        let s = serverURL.trimmed
        if let u = URL(string: s), u.scheme == "https" || u.scheme == "http" {
            return u
        }
        return URL(string: AppSettings.defaultServer)!
    }

    /// Low-data mode as it applies now: the override, else what the
    /// network says.
    func lowData(_ network: NetworkMonitor) -> Bool {
        switch lowDataOverride {
        case "on": return true
        case "off": return false
        default: return network.isConstrained || network.isExpensive
        }
    }

    var photoConsentGiven: Bool { photoConsent == "accepted" }

    static let streamBitrates: [Int] = [2500, 4000, 6000]

    /// The stream key lives in the Keychain, not the store.
    static let streamKeyAccount = "stream-key"

    static func syncLabel(_ minutes: Int) -> String {
        minutes == 0 ? "Manual" : (minutes == 60 ? "Every hour" : "Every \(minutes) min")
    }
}

/// Applies the Appearance setting (sheets and full-screen covers need it
/// applied to their content too).
struct OCAppearanceModifier: ViewModifier {
    @ObservedObject var settings: AppSettings

    func body(content: Content) -> some View {
        content.preferredColorScheme(settings.colorScheme)
    }
}

extension View {
    @MainActor
    @ViewBuilder
    func ocAppearance() -> some View {
        if let settings = AppSettings.current {
            modifier(OCAppearanceModifier(settings: settings))
        } else {
            preferredColorScheme(.dark)
        }
    }
}
