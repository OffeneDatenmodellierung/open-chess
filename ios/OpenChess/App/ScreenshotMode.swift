#if DEBUG
import OpenChessCore
import SwiftUI

/// The App Store screenshots: launched with `-OCScreenshot <scene>`
/// (scripts/ios-screenshots.sh), the app opens that scene with sample
/// data and stays offline. Debug builds only.
enum ScreenshotScene: String, CaseIterable {
    /// Play: ratings, the ways to play.
    case home
    /// A training game against the computer: the board, the best move and
    /// the top lines.
    case game
    /// The analysis board on a famous game.
    case analysis
    /// Train: hints, lines and odds.
    case train
    /// The real-board game's setup: a Chessnut, photos or by hand.
    case realboard
}

enum ScreenshotMode {
    static let scene: ScreenshotScene? = {
        let args = ProcessInfo.processInfo.arguments
        guard let i = args.firstIndex(of: "-OCScreenshot"), i + 1 < args.count else { return nil }
        return ScreenshotScene(rawValue: args[i + 1])
    }()

    /// A Ruy Lopez, Closed (after 9.h3), White to move: a position with
    /// plenty for the advice to say.
    static let gameFen = "r1bq1rk1/2p1bppp/p1np1n2/1p2p3/4P3/1BP2N1P/PP1P1PP1/RNBQR1K1 b - - 0 9"

    /// The Opera Game (Morphy v the Duke of Brunswick and Count Isouard,
    /// Paris 1858) to just before its finish.
    static let analysisMoves = [
        "e2e4", "e7e5", "g1f3", "d7d6", "d2d4", "c8g4", "d4e5", "g4f3", "d1f3", "d6e5",
        "f1c4", "g8f6", "f3b3", "d8e7", "b1c3", "c7c6", "c1g5", "b7b5", "c3b5", "c6b5",
        "c4b5", "b8d7", "e1c1", "a8d8", "d1d7", "d8d7", "h1d1", "e7e6", "b5d7", "f6d7",
    ]

    /// Opens `scene` once the app is up.
    @MainActor
    static func open(_ scene: ScreenshotScene, model: AppModel, tab: Binding<AppTab>,
                     analysis: Binding<AnalysisBoardModel?>, realBoard: Binding<Bool>) {
        model.showScreenshotRatings()
        switch scene {
        case .home:
            tab.wrappedValue = .play
        case .train:
            tab.wrappedValue = .train
        case .game:
            if let g = try? GameViewModel(
                app: model, kind: .computer(level: 6, playerWhite: true), fen: gameFen,
                time: .untimed, white: "You", black: "Computer · level 6", training: true
            ) {
                model.showGame(g)
            }
        case .analysis:
            analysis.wrappedValue = try? AnalysisBoardModel(
                fen: nil, moves: analysisMoves, title: "Analysis · Morphy v Duke Karl, 1858"
            )
        case .realboard:
            realBoard.wrappedValue = true
        }
    }
}
#endif
