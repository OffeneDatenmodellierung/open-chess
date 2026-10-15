import Foundation
import OpenChessCore

/// Sends a real-board game's moves, take-backs and result to the server,
/// one at a time and in order. The queue is kept in the store's settings,
/// so nothing is lost offline or if the app closes; it's retried every few
/// seconds until sent. A call the server refuses (an illegal move, a game
/// already over) is dropped and reported, so the rest can go.
@MainActor
final class OTBSender: ObservableObject {
    private static let storeKey = "otbQueue"
    private static let retrySeconds: TimeInterval = 5

    @Published private(set) var ops: [OTBOp] = []
    @Published private(set) var lastError: String?

    /// Each answered call, with the server's JSON (or `["error": …]`).
    var onResult: ((OTBOp, [String: Any]) -> Void)?

    private let store: Store
    private let auth: AuthManager
    private var inFlight = false
    private var retryTimer: Timer?

    init(store: Store, auth: AuthManager) {
        self.store = store
        self.auth = auth
        load()
    }

    private func load() {
        guard let s = try? store.setting(key: OTBSender.storeKey),
              let list = try? JSONDecoder().decode([OTBOp].self, from: Data(s.utf8)) else { return }
        ops = list
    }

    private func persist() {
        let data = (try? JSONEncoder().encode(ops)) ?? Data("[]".utf8)
        let text = String(data: data, encoding: .utf8) ?? "[]"
        try? store.setSetting(key: OTBSender.storeKey, value: text)
    }

    func waiting(gameId: String) -> Int {
        ops.filter { $0.gameId == gameId }.count
    }

    func enqueue(_ op: OTBOp) {
        ops.append(op)
        persist()
        kick()
    }

    /// Take back plies: unsent moves are dropped, the rest sent as an undo.
    func enqueueUndo(gameId: String, plies: Int) {
        var list = ops
        OTBQueueLogic.appendUndo(&list, gameId: gameId, plies: plies, firstInFlight: inFlight)
        ops = list
        persist()
        kick()
    }

    /// Send what's waiting, if anything and not already sending.
    func kick() {
        guard !inFlight, !ops.isEmpty else { return }
        guard auth.signedIn else {
            // Try again shortly: signing in (from the prompt) carries on.
            scheduleRetry()
            return
        }
        retryTimer?.invalidate()
        retryTimer = nil
        inFlight = true
        Task {
            await self.drain()
            self.inFlight = false
        }
    }

    private func scheduleRetry() {
        retryTimer?.invalidate()
        let t = Timer(timeInterval: OTBSender.retrySeconds, repeats: false) { [weak self] _ in
            Task { @MainActor in
                self?.kick()
            }
        }
        RunLoop.main.add(t, forMode: .common)
        retryTimer = t
    }

    private func drain() async {
        while let op = ops.first {
            do {
                let data = try await send(op)
                remove(op)
                lastError = nil
                onResult?(op, JSONAny.object(data) ?? [:])
            } catch {
                let status = (error as? APIError)?.status ?? 0
                if status == 401 {
                    lastError = "Sign in again to send the game to the server."
                    scheduleRetry()
                    return
                }
                if status == 0 || status == 429 || status >= 500 {
                    lastError = "Waiting to send (\(ops.count)): \(describe(error))"
                    scheduleRetry()
                    return
                }
                remove(op)
                lastError = "The server refused \(op.summary): \(describe(error))"
                onResult?(op, ["error": describe(error)])
            }
        }
    }

    private func remove(_ op: OTBOp) {
        if let i = ops.firstIndex(where: { $0.id == op.id }) {
            ops.remove(at: i)
        }
        persist()
    }

    private func send(_ op: OTBOp) async throws -> Data {
        let path = "/api/otb/\(APIClient.pathComponent(op.gameId))/\(op.kind)"
        return try await auth.server.postJSON(path, object: op.body)
    }
}
