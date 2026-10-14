import SwiftUI

@MainActor
final class OnlineViewModel: ObservableObject {
    @Published var opponent = ""
    @Published var days = 3
    @Published var rated = true
    @Published var colour = "random"
    @Published var time = TimeControl(minutes: 10, increment: 5)
    @Published var realTime = false
    @Published var busy = false
    @Published var note: String?
    @Published var lichessGames: [PlayingGame] = []
    @Published var lichessName: String?

    /// `POST /api/challenge/{username}`: correspondence (days) or a clock.
    func challenge(app: AppModel, onLichess: Bool) async {
        let name = opponent.trimmed
        guard !name.isEmpty else {
            note = "Who do you want to play? Enter their username."
            return
        }
        let api = onLichess ? app.auth.lichess : app.auth.server
        var form: [String: String] = ["rated": rated ? "true" : "false", "color": colour]
        if realTime && !app.sync.lowData {
            form["clock.limit"] = String(time.minutes * 60)
            form["clock.increment"] = String(time.increment)
        } else {
            form["days"] = String(days)
        }
        busy = true
        defer { busy = false }
        do {
            try await api.postForm("/api/challenge/\(APIClient.pathComponent(name))", form: form)
            note = "Challenge sent to \(name). The game appears below once they accept."
        } catch {
            note = describe(error)
        }
    }

    /// Lichess's games in play (`GET /api/account/playing`).
    func loadLichess(app: AppModel) async {
        guard app.auth.hasLichess else {
            lichessGames = []
            return
        }
        if lichessName == nil {
            lichessName = await app.auth.lichessUsername()
        }
        do {
            let data = try await app.auth.lichess.get("/api/account/playing", query: ["nb": "20"])
            guard let obj = JSONAny.object(data), let list = obj["nowPlaying"] as? [Any] else { return }
            let games = try JSONDecoder().decode([PlayingGame].self, from: JSONAny.data(list))
            lichessGames = games.map { (game: PlayingGame) -> PlayingGame in
                var copy = game
                copy.site = "lichess"
                return copy
            }
        } catch {
            note = "Lichess: \(describe(error))"
        }
    }

    func accept(_ c: IncomingChallenge, app: AppModel) async {
        do {
            try await app.auth.server.postForm("/api/challenge/\(APIClient.pathComponent(c.id))/accept", form: [:])
            await app.sync.syncNow(reason: "accept")
        } catch {
            note = describe(error)
        }
    }

    func decline(_ c: IncomingChallenge, app: AppModel) async {
        do {
            try await app.auth.server.postForm("/api/challenge/\(APIClient.pathComponent(c.id))/decline", form: [:])
            await app.sync.syncNow(reason: "decline")
        } catch {
            note = describe(error)
        }
    }
}

/// Play online: correspondence on open-chess first, Lichess second;
/// real-time games only when low data is off.
@MainActor
struct OnlineView: View {
    @EnvironmentObject var model: AppModel
    @EnvironmentObject var sync: SyncEngine
    @EnvironmentObject var auth: AuthManager
    @StateObject var vm = OnlineViewModel()
    @State var lichessToken = ""
    @State var error: String?

    var body: some View {
        Form {
            if !auth.signedIn {
                Section {
                    Text("Sign in to the open-chess server under Account to play online.")
                        .foregroundStyle(Color.ocMuted)
                }
                .listRowBackground(Color.ocPanel)
            }
            gamesSection
            if !sync.challenges.isEmpty {
                Section {
                    ForEach(sync.challenges) { c in
                        HStack {
                            VStack(alignment: .leading) {
                                Text(c.from).font(OCFont.text(15, weight: .semibold))
                                Text(c.daysPerTurn.map { "\($0) days per move" } ?? c.speed.capitalized)
                                    .font(OCFont.text(12)).foregroundStyle(Color.ocMuted)
                            }
                            Spacer()
                            Button("Accept") { Task { await vm.accept(c, app: model) } }
                                .buttonStyle(OCButtonStyle(kind: .primary, compact: true))
                            Button("Decline") { Task { await vm.decline(c, app: model) } }
                                .buttonStyle(OCButtonStyle(kind: .secondary, compact: true))
                        }
                    }
                } header: {
                    SectionTitle(text: "Challenges")
                }
                .listRowBackground(Color.ocPanel)
            }
            Section {
                TextField("Opponent's username", text: $vm.opponent)
                    .textInputAutocapitalization(.never)
                    .autocorrectionDisabled()
                Toggle("Real-time game", isOn: $vm.realTime)
                    .disabled(sync.lowData)
                if vm.realTime && !sync.lowData {
                    Picker("Clock", selection: $vm.time) {
                        ForEach(TimeControl.choices.filter { $0.isTimed }, id: \.self) { tc in
                            Text(tc.label).tag(tc)
                        }
                    }
                } else {
                    Picker("Days per move", selection: $vm.days) {
                        Text("1 day").tag(1)
                        Text("3 days").tag(3)
                        Text("7 days").tag(7)
                    }
                }
                Picker("Colour", selection: $vm.colour) {
                    Text("White").tag("white")
                    Text("Random").tag("random")
                    Text("Black").tag("black")
                }
                Toggle("Rated", isOn: $vm.rated)
                Button("Challenge on open-chess") {
                    Task { await vm.challenge(app: model, onLichess: false) }
                }
                .disabled(!auth.signedIn || vm.busy)
                if auth.hasLichess {
                    Button("Also play on Lichess") {
                        Task { await vm.challenge(app: model, onLichess: true) }
                    }
                    .disabled(vm.busy)
                }
                if let n = vm.note {
                    Text(n).font(OCFont.text(13)).foregroundStyle(Color.ocMuted)
                }
            } header: {
                SectionTitle(text: "New game")
            } footer: {
                Text(sync.lowData
                     ? "Low data is on: correspondence only. Moves go one request at a time."
                     : "Correspondence games suit a slow or costly connection.")
            }
            .listRowBackground(Color.ocPanel)
            lichessSection
        }
        .ocScreen()
        .navigationTitle("Play online")
        .navigationBarTitleDisplayMode(.inline)
        .task {
            await sync.syncNow(reason: "online")
            await vm.loadLichess(app: model)
        }
        .refreshable {
            await sync.syncNow(reason: "online")
            await vm.loadLichess(app: model)
        }
        .alert("Couldn't open the game", isPresented: Binding(
            get: { error != nil },
            set: { if !$0 { error = nil } }
        )) {
            Button("OK", role: .cancel) {}
        } message: {
            Text(error ?? "")
        }
    }

    private var allGames: [PlayingGame] {
        sync.playing + vm.lichessGames
    }

    @ViewBuilder
    private var gamesSection: some View {
        Section {
            if allGames.isEmpty {
                Text("No games in play.").foregroundStyle(Color.ocMuted)
            }
            ForEach(allGames) { g in
                Button {
                    openGame(g)
                } label: {
                    HStack {
                        VStack(alignment: .leading, spacing: 2) {
                            Text(opponentText(g))
                                .font(OCFont.text(15, weight: .semibold))
                                .foregroundStyle(Color.ocInk)
                            Text(detailText(g))
                                .font(OCFont.text(12))
                                .foregroundStyle(Color.ocMuted)
                        }
                        Spacer()
                        if g.isMyTurn {
                            OCBadge(text: "Your move", color: .ocAction)
                        }
                    }
                }
            }
        } header: {
            SectionTitle(text: "Your games")
        }
        .listRowBackground(Color.ocPanel)
    }

    @ViewBuilder
    private var lichessSection: some View {
        Section {
            if auth.hasLichess {
                HStack {
                    Text("Lichess")
                    Spacer()
                    Text(vm.lichessName ?? "token saved").foregroundStyle(Color.ocMuted)
                }
                Button("Remove Lichess token", role: .destructive) {
                    auth.setLichessToken(nil)
                    vm.lichessName = nil
                    vm.lichessGames = []
                }
            } else {
                SecureField("Paste a Lichess token", text: $lichessToken)
                    .textInputAutocapitalization(.never)
                    .autocorrectionDisabled()
                Button("Save token") {
                    auth.setLichessToken(lichessToken)
                    lichessToken = ""
                    Task { await vm.loadLichess(app: model) }
                }
                .disabled(lichessToken.trimmed.isEmpty)
                Link("Create a token on lichess.org",
                     destination: URL(string: "https://lichess.org/account/oauth/token/create?scopes%5B%5D=board:play&scopes%5B%5D=challenge:write&description=open-chess")!)
            }
        } header: {
            SectionTitle(text: "Also play on Lichess")
        } footer: {
            Text("Create a personal token at lichess.org/account/oauth/token with board:play and challenge:write, and paste it here. It stays in this device's Keychain.")
        }
        .listRowBackground(Color.ocPanel)
    }

    private func opponentText(_ g: PlayingGame) -> String {
        if let r = g.opponent.rating {
            return "vs \(g.opponent.username) (\(r))"
        }
        return "vs \(g.opponent.username)"
    }

    private func detailText(_ g: PlayingGame) -> String {
        let site = g.site == "lichess" ? "Lichess" : "open-chess"
        let pace: String
        if let d = g.daysPerTurn {
            pace = "\(d) d/move"
        } else {
            pace = g.speed.capitalized
        }
        let rated = g.rated ? "rated" : "casual"
        return "\(site) · \(pace) · \(rated)"
    }

    private func openGame(_ g: PlayingGame) {
        let me = model.settings.username.isEmpty ? "You" : model.settings.username
        let white = g.color == "white"
        let link = OnlineLink(
            site: g.site,
            gameId: g.gameId,
            myColorWhite: white,
            rated: g.rated,
            opponent: g.opponent.username,
            opponentRating: g.opponent.rating,
            speed: g.speed.isEmpty ? (g.daysPerTurn == nil ? "rapid" : "correspondence") : g.speed,
            daysPerTurn: g.daysPerTurn
        )
        do {
            // Start from the beginning; the stream's first line brings the moves.
            let game = try GameViewModel(
                app: model,
                kind: .online(link),
                fen: nil,
                time: .untimed,
                white: white ? me : g.opponent.username,
                black: white ? g.opponent.username : me,
                gameId: "\(g.site):\(g.gameId)"
            )
            model.showGame(game)
        } catch {
            self.error = describe(error)
        }
    }
}
