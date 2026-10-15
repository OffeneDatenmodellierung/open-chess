import BackgroundTasks
import Foundation
import OpenChessCore

/// Sends the outbox and pulls ratings, games and confirmations from the
/// server: on the chosen interval while the app is open, on "Sync now",
/// and when iOS gives the app background time.
@MainActor
final class SyncEngine: ObservableObject {
    nonisolated static let backgroundIdentifier = "org.offenedatenmodellierung.openchess.sync"
    /// Low-data mode's byte budget per sync (games only).
    static let lowDataBudget: UInt64 = 50_000
    /// Otherwise, per sync (photos included when allowed).
    static let normalBudget: UInt64 = 5_000_000

    @Published private(set) var syncing = false
    @Published private(set) var lastError: String?
    /// Results worth showing ("Game rejected: …").
    @Published private(set) var notices: [String] = []
    @Published private(set) var playing: [PlayingGame] = []
    @Published private(set) var confirmations: [Confirmation] = []
    @Published private(set) var challenges: [IncomingChallenge] = []
    @Published private(set) var outbox: [OutboxItem] = []
    @Published private(set) var totals = PendingTotals(items: 0, bytes: 0)

    var onApplied: (() -> Void)?

    private let store: Store
    private let settings: AppSettings
    private let network: NetworkMonitor
    private let auth: AuthManager
    private var timer: Timer?

    init(store: Store, settings: AppSettings, network: NetworkMonitor, auth: AuthManager) {
        self.store = store
        self.settings = settings
        self.network = network
        self.auth = auth
        refreshOutbox()
    }

    var lowData: Bool { settings.lowData(network) }

    /// Photos may go now: allowed, consented, not low data, and on Wi-Fi
    /// when set to Wi-Fi only.
    var photosAllowed: Bool {
        settings.photoUploads && settings.photoConsentGiven && !lowData
            && (!settings.photosWifiOnly || network.onWifi)
    }

    /// "Synced 4 min ago · Low data".
    var pillText: String {
        var parts: [String] = []
        if syncing {
            parts.append("Syncing…")
        } else if !auth.signedIn {
            parts.append("Not signed in")
        } else if lastError != nil {
            parts.append("Sync failed")
        } else {
            parts.append("Synced \(agoText(ms: settings.lastSyncMs))")
        }
        if totals.items > 0 {
            parts.append("\(totals.items) waiting")
        }
        if lowData { parts.append("Low data") }
        return parts.joined(separator: " · ")
    }

    // MARK: Scheduling

    func startForeground() {
        stopForeground()
        refreshOutbox()
        let minutes = settings.syncMinutes
        guard minutes > 0 else { return }
        if nowMs() - min(nowMs(), settings.lastSyncMs) > UInt64(minutes) * 60_000 {
            Task { await self.syncNow(reason: "open") }
        }
        let t = Timer(timeInterval: TimeInterval(minutes * 60), repeats: true) { [weak self] _ in
            Task { @MainActor in
                await self?.syncNow(reason: "timer")
            }
        }
        RunLoop.main.add(t, forMode: .common)
        timer = t
    }

    func stopForeground() {
        timer?.invalidate()
        timer = nil
    }

    /// Restart the timer after the interval changes.
    func intervalChanged() {
        startForeground()
        SyncEngine.scheduleBackgroundRefresh(minutes: settings.syncMinutes)
    }

    nonisolated static func scheduleBackgroundRefresh(minutes: Int) {
        let request = BGAppRefreshTaskRequest(identifier: backgroundIdentifier)
        // iOS decides when; ask for no sooner than the chosen interval (at
        // least 15 minutes, as iOS wouldn't give it sooner anyway).
        let wait = max(15, minutes == 0 ? 60 : minutes)
        request.earliestBeginDate = Date(timeIntervalSinceNow: TimeInterval(wait * 60))
        do {
            try BGTaskScheduler.shared.submit(request)
        } catch {
            // The simulator and some states refuse; foreground sync still works.
        }
    }

    // MARK: Outbox

    func refreshOutbox() {
        do {
            outbox = try store.pending(includePhotos: true, maxBytes: 0)
            totals = try store.pendingTotals(includePhotos: true)
        } catch {
            lastError = describe(error)
        }
    }

    func enqueue(key: String, kind: String, payload: Data) {
        do {
            _ = try store.enqueue(key: key, kind: kind, payload: payload, nowMs: nowMs())
        } catch {
            lastError = describe(error)
        }
        refreshOutbox()
    }

    func dismissNotices() {
        notices = []
    }

    // MARK: Sync

    func syncNow(reason: String) async {
        guard !syncing else { return }
        guard auth.signedIn else {
            lastError = nil
            return
        }
        guard network.isOnline else {
            lastError = "Offline: will sync when there's a connection."
            return
        }
        syncing = true
        lastError = nil
        defer {
            syncing = false
            refreshOutbox()
        }
        let api = auth.server
        let low = lowData
        let includePhotos = photosAllowed
        let budget = low ? SyncEngine.lowDataBudget : SyncEngine.normalBudget
        do {
            let items = try store.pending(includePhotos: includePhotos, maxBytes: budget)
            var sent: [String] = []
            sent += try await sendGames(items.filter { $0.kind == "game" }, api: api)
            sent += await sendConfirmations(items.filter { $0.kind == "confirm" }, api: api)
            if includePhotos {
                sent += await sendPhotos(items.filter { $0.kind == "photo" }, api: api)
            }
            if !sent.isEmpty {
                try store.markSent(keys: sent, nowMs: nowMs())
            }
            // Keep a month of sent keys' payloads; after that only the keys.
            try store.pruneSent(beforeMs: nowMs() - min(nowMs(), 30 * 24 * 3_600_000))
            try await pull(api: api)
            settings.lastSyncMs = nowMs()
        } catch {
            if let e = error as? APIError, e.status == 401 {
                lastError = "Your sign-in has expired: sign in again."
            } else {
                lastError = describe(error)
            }
        }
    }

    /// `POST /api/games/offline`; every result except a failed request
    /// counts as sent ("rejected" too, with its reason shown).
    private func sendGames(_ items: [OutboxItem], api: APIClient) async throws -> [String] {
        guard !items.isEmpty else { return [] }
        var games: [Any] = []
        var keys: [String] = []
        for item in items {
            if let obj = try? JSONSerialization.jsonObject(with: item.payload) {
                games.append(obj)
                keys.append(item.key)
            }
        }
        guard !games.isEmpty else { return [] }
        let data = try await api.postJSON("/api/games/offline", object: ["games": games])
        let results = (try? JSONDecoder().decode(OfflineResults.self, from: data))?.results ?? []
        var sent: [String] = []
        for r in results where keys.contains(r.clientId) {
            sent.append(r.clientId)
            if r.status == "rejected" {
                notices.append("A game wasn't accepted: \(r.reason ?? "no reason given").")
            } else if r.status == "rated", let d = r.ratingDiff {
                notices.append("Game rated (\(d >= 0 ? "+" : "")\(d)).")
            }
        }
        return sent
    }

    /// Queued confirmations: payload `{"gameId": "...", "accept": true}`.
    private func sendConfirmations(_ items: [OutboxItem], api: APIClient) async -> [String] {
        var sent: [String] = []
        for item in items {
            guard let obj = JSONAny.object(item.payload),
                  let id = obj["gameId"] as? String else {
                sent.append(item.key)
                continue
            }
            let accept = (obj["accept"] as? Bool) ?? true
            do {
                try await api.postJSON("/api/games/\(APIClient.pathComponent(id))/confirm", object: ["accept": accept])
                sent.append(item.key)
            } catch {
                if let e = error as? APIError, e.status == 404 || e.status == 409 || e.status == 400 {
                    // Gone or already answered: don't retry forever.
                    sent.append(item.key)
                } else {
                    lastError = describe(error)
                }
            }
        }
        return sent
    }

    /// Queued photos: payload is the JSON body for `POST /api/photos`.
    private func sendPhotos(_ items: [OutboxItem], api: APIClient) async -> [String] {
        var sent: [String] = []
        for item in items {
            do {
                try await api.postJSON("/api/photos", body: item.payload)
                sent.append(item.key)
            } catch {
                if let e = error as? APIError, e.status == 400 || e.status == 413 {
                    notices.append("A photo wasn't accepted: \(e.message)")
                    sent.append(item.key)
                } else {
                    lastError = describe(error)
                }
            }
        }
        return sent
    }

    /// `GET /api/sync?since=`: ratings, games in play, confirmations, challenges.
    private func pull(api: APIClient) async throws {
        let data = try await api.get("/api/sync", query: ["since": String(settings.syncSince)])
        let s = try JSONDecoder().decode(SyncResponse.self, from: data)
        let now = nowMs()
        for (speed, r) in s.ratings {
            let v = RatingValue(rating: r.rating, deviation: r.deviation, volatility: r.volatility)
            try store.setServerRating(speed: speed, value: v, nowMs: now)
        }
        playing = s.playing
        confirmations = s.confirmations.filter { c in
            // Hide ones answered and still in the outbox.
            !outbox.contains { $0.key == "confirm-" + c.id }
        }
        challenges = s.challenges
        if s.now > 0 { settings.syncSince = s.now }
        onApplied?()
    }

    /// Answer a confirmation: queue it, then try to send at once.
    func confirm(_ c: Confirmation, accept: Bool) {
        let body: [String: Any] = ["gameId": c.id, "accept": accept]
        let payload = JSONAny.data(body)
        enqueue(key: "confirm-" + c.id, kind: "confirm", payload: payload)
        confirmations.removeAll { $0.id == c.id }
        Task { await self.syncNow(reason: "confirm") }
    }
}
