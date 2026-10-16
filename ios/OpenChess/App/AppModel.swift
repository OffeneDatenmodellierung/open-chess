import OpenChessCore
import SwiftUI

/// Everything the screens share: the core's store, settings, the network
/// state, sign-in, sync, the Chessnut board and the game on screen.
@MainActor
final class AppModel: ObservableObject {
    /// The running model, for the app delegate's push callbacks.
    static var current: AppModel?

    let store: Store
    let settings: AppSettings
    let network: NetworkMonitor
    let auth: AuthManager
    let sync: SyncEngine
    let chessnut: ChessnutManager
    let push: PushManager
    /// A real-board game's moves on their way to the server.
    let otb: OTBSender

    /// The game on screen (shown full screen over the tabs).
    @Published var activeGame: GameViewModel?
    /// The real-board game on screen (shown full screen over the tabs).
    @Published var liveBoard: LiveBoardModel?
    /// Saved games, most recent first (Games tab).
    @Published private(set) var savedGames: [SavedGame] = []
    /// Each speed's rating (server or provisional).
    @Published private(set) var ratings: [SpeedRating] = []
    @Published var storeError: String?

    private var readerCache: BoardReader?
    private var readerError: String?
    private var readerLoaded = false

    init() {
        TabBarStyle.apply()
        let s = AppModel.openStore()
        let settings = AppSettings(store: s.store)
        let network = NetworkMonitor()
        let auth = AuthManager(settings: settings)
        store = s.store
        self.settings = settings
        self.network = network
        self.auth = auth
        sync = SyncEngine(store: s.store, settings: settings, network: network, auth: auth)
        chessnut = ChessnutManager()
        push = PushManager(settings: settings, auth: auth)
        otb = OTBSender(store: s.store, auth: auth)
        storeError = s.error
        sync.onApplied = { [weak self] in self?.reloadRatings() }
        AppModel.current = self
        AppSettings.current = settings
        Feedback.settings = settings
        otb.kick()
        reloadGames()
        reloadRatings()
    }

    private static func openStore() -> (store: Store, error: String?) {
        let dir = FileManager.default.urls(for: .applicationSupportDirectory, in: .userDomainMask).first
            ?? FileManager.default.temporaryDirectory
        try? FileManager.default.createDirectory(at: dir, withIntermediateDirectories: true)
        let path = dir.appendingPathComponent("open-chess.sqlite").path
        do {
            return (try Store.open(path: path), nil)
        } catch {
            // Keep the app usable: an in-memory store, and say so.
            let message = "Couldn't open the saved data (\(describe(error))); nothing will be kept this session."
            if let mem = try? Store.inMemory() {
                return (mem, message)
            }
            fatalError("open-chess: no store at all: \(describe(error))")
        }
    }

    // MARK: Games

    func reloadGames() {
        do {
            let ids = try store.gameIds(limit: 200)
            var out: [SavedGame] = []
            for id in ids {
                if let s = try store.loadGame(id: id), let g = SavedGame.decode(s) {
                    out.append(g)
                }
            }
            savedGames = out
        } catch {
            storeError = describe(error)
        }
    }

    func deleteGame(_ id: String) {
        do {
            try store.deleteGame(id: id)
        } catch {
            storeError = describe(error)
        }
        reloadGames()
    }

    func showGame(_ game: GameViewModel) {
        activeGame = game
    }

    // MARK: Ratings

    func reloadRatings() {
        do {
            ratings = try store.ratings()
        } catch {
            storeError = describe(error)
        }
    }

    #if DEBUG
    /// App Store screenshots (`ScreenshotMode`): settled ratings to show.
    func showScreenshotRatings() {
        let values: [(String, Double)] = [
            ("bullet", 1612), ("blitz", 1748), ("rapid", 1835), ("classical", 1902), ("correspondence", 1786),
        ]
        ratings = values.map {
            SpeedRating(speed: $0.0, value: RatingValue(rating: $0.1, deviation: 62, volatility: 0.06), local: false)
        }
    }
    #endif

    func rating(for speed: String) -> RatingValue {
        if let r = ratings.first(where: { $0.speed == speed }) {
            return r.value
        }
        return newRating()
    }

    // MARK: Board reader

    /// The board reader from `board-reader.safetensors` in the bundle,
    /// loaded once off the main thread.
    func boardReader() async -> (BoardReader?, String?) {
        if readerLoaded { return (readerCache, readerError) }
        guard let url = Bundle.main.url(forResource: "board-reader", withExtension: "safetensors") else {
            readerLoaded = true
            readerError = "This build has no board reader (board-reader.safetensors). Set up the position by hand."
            return (nil, readerError)
        }
        let result: (BoardReader?, String?) = await Task.detached(priority: .userInitiated) { () -> (BoardReader?, String?) in
            do {
                let data = try Data(contentsOf: url, options: .mappedIfSafe)
                return (try BoardReader(weights: data), nil)
            } catch {
                return (nil, "The board reader couldn't load: \(describe(error))")
            }
        }.value
        readerCache = result.0
        readerError = result.1
        readerLoaded = true
        return result
    }

    /// The model card, if the reader is loaded.
    var readerCard: ModelCardInfo? {
        guard let r = readerCache else { return nil }
        return ModelCardInfo.decode(r.cardJson())
    }

    // MARK: Lifecycle

    func scenePhaseChanged(_ phase: ScenePhase) {
        switch phase {
        case .active:
            sync.startForeground()
            push.refreshRegistration()
            otb.kick()
            settings.refreshAppearance()
        case .background:
            sync.stopForeground()
            SyncEngine.scheduleBackgroundRefresh(minutes: settings.syncMinutes)
        default:
            break
        }
    }
}
