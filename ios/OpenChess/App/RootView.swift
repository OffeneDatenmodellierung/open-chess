import SwiftUI

enum AppTab: Hashable {
    case play, games, train, account
}

/// iPhone and iPad both use the tab bar (Play / Games / Train / Account);
/// on iPad the game screen puts the board left and the panel right.
@MainActor
struct RootView: View {
    @EnvironmentObject var model: AppModel
    @EnvironmentObject var settings: AppSettings
    @State var tab: AppTab = .play

    var body: some View {
        TabView(selection: $tab) {
            NavigationStack { PlayHomeView(tab: $tab) }
                .tabItem { Label("Play", systemImage: "play.square") }
                .tag(AppTab.play)
            NavigationStack { GamesListView() }
                .tabItem { Label("Games", systemImage: "list.bullet.rectangle") }
                .tag(AppTab.games)
            NavigationStack { TrainView() }
                .tabItem { Label("Train", systemImage: "graduationcap") }
                .tag(AppTab.train)
            NavigationStack { AccountView() }
                .tabItem { Label("Account", systemImage: "person.crop.square") }
                .tag(AppTab.account)
        }
        .preferredColorScheme(settings.colorScheme)
        .fullScreenCover(item: $model.activeGame) { game in
            GameScreen(game: game)
                .environmentObject(model)
                .environmentObject(settings)
                .environmentObject(model.sync)
                .environmentObject(model.auth)
                .environmentObject(model.chessnut)
                .environmentObject(model.network)
                .environmentObject(model.push)
                .environmentObject(model.otb)
                .preferredColorScheme(settings.colorScheme)
        }
        .fullScreenCover(item: $model.liveBoard) { live in
            LiveBoardScreen(live: live)
                .environmentObject(model)
                .environmentObject(settings)
                .environmentObject(model.sync)
                .environmentObject(model.auth)
                .environmentObject(model.chessnut)
                .environmentObject(model.network)
                .environmentObject(model.push)
                .environmentObject(model.otb)
                .preferredColorScheme(settings.colorScheme)
        }
        .alert("Saved data", isPresented: Binding(
            get: { model.storeError != nil },
            set: { if !$0 { model.storeError = nil } }
        )) {
            Button("OK", role: .cancel) {}
        } message: {
            Text(model.storeError ?? "")
        }
    }
}
