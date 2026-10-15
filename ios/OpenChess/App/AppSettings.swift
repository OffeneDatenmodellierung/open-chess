import OpenChessCore
import SwiftUI

/// The app's settings, kept in the core's store (`setting` / `setSetting`).
@MainActor
final class AppSettings: ObservableObject {
    static let defaultServer = "https://chess.example.com"
    static let syncChoices: [Int] = [0, 1, 5, 10, 30, 60]

    private let store: Store

    /// "system", "dark" or "light".
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
        appearance = read("appearance") ?? "system"
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

    var colorScheme: ColorScheme? {
        switch appearance {
        case "dark": return .dark
        case "light": return .light
        default: return nil
        }
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

    static func syncLabel(_ minutes: Int) -> String {
        minutes == 0 ? "Manual" : (minutes == 60 ? "Every hour" : "Every \(minutes) min")
    }
}
