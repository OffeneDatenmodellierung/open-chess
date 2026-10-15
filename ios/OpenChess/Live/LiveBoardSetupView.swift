import SwiftUI

/// "Play on a real board": who plays which colour, the speed, rated or
/// not, where the moves come from, and the extras (training frames,
/// streaming, the overlay link).
@MainActor
struct LiveBoardSetupView: View {
    @EnvironmentObject var model: AppModel
    @EnvironmentObject var settings: AppSettings
    @EnvironmentObject var auth: AuthManager
    @EnvironmentObject var chessnut: ChessnutManager
    @Environment(\.dismiss) var dismiss

    @State var opponent = ""
    @State var iAmWhite = true
    @State var speed = "rapid"
    @State var rated = true
    @State var source: LiveSource = .manual
    @State var helpTrain = false
    @State var stream = false
    @State var overlay = false
    @State var busy = false
    @State var error: String?
    @State var showLicence = false
    @State var readerMissing = false

    var body: some View {
        NavigationStack {
            Form {
                if !auth.signedIn {
                    Section {
                        SignInPrompt(message: "Sign in to record a real-board game on your account (and to share it live).", inList: true)
                    }
                    .listRowBackground(Color.ocPanel)
                }
                Section {
                    Picker("You play", selection: $iAmWhite) {
                        Text("White").tag(true)
                        Text("Black").tag(false)
                    }
                    .pickerStyle(.segmented)
                    TextField("Opponent's open-chess username", text: $opponent)
                        .textInputAutocapitalization(.never)
                        .autocorrectionDisabled()
                } header: {
                    SectionTitle(text: "Players")
                } footer: {
                    Text("White: \(iAmWhite ? me : (opponent.trimmed.isEmpty ? "…" : opponent.trimmed)) · Black: \(iAmWhite ? (opponent.trimmed.isEmpty ? "…" : opponent.trimmed) : me)")
                }
                .listRowBackground(Color.ocPanel)

                Section {
                    Picker("Speed", selection: $speed) {
                        Text("Blitz").tag("blitz")
                        Text("Rapid").tag("rapid")
                        Text("Classical").tag("classical")
                    }
                    .pickerStyle(.segmented)
                    Toggle("Rated", isOn: $rated)
                } header: {
                    SectionTitle(text: "Game")
                } footer: {
                    Text(rated
                         ? "A rated game counts once the other player confirms the result in their app. Hints are off in rated games."
                         : "Casual: not rated. Hints are allowed, and the game shows as assisted if you use them.")
                }
                .listRowBackground(Color.ocPanel)

                Section {
                    Picker("Moves from", selection: $source) {
                        Text(LiveSource.chessnut.label).tag(LiveSource.chessnut)
                        Text(LiveSource.photos.label).tag(LiveSource.photos)
                        Text(LiveSource.manual.label).tag(LiveSource.manual)
                    }
                    .pickerStyle(.inline)
                    .labelsHidden()
                    if source == .chessnut && !chessnut.isConnected {
                        NavigationLink("Connect the Chessnut board") { ChessnutView(returnOnConnect: true) }
                    }
                    if source == .photos && readerMissing {
                        Text("This build has no board reader, so you'll tap each move on the board instead.")
                            .font(OCFont.text(12)).foregroundStyle(Color.ocAmber)
                    }
                } header: {
                    SectionTitle(text: "Board")
                } footer: {
                    Text(sourceFooter)
                }
                .listRowBackground(Color.ocPanel)

                Section {
                    Toggle("Photograph the board as you play", isOn: $helpTrain)
                    Toggle("Stream (Twitch, YouTube…)", isOn: $stream)
                    if stream {
                        NavigationLink("Streaming settings") { StreamSettingsView() }
                    }
                    Toggle("Share a live board overlay link", isOn: $overlay)
                } header: {
                    SectionTitle(text: "Extras")
                } footer: {
                    Text("Photos: the camera takes a picture of the board after each move, labelled with the position, and sends them with your syncs to help train the board reader (under the photo licence). Streaming sends the camera's video with the board drawn in a corner. The overlay link shows the board in OBS or a browser.")
                }
                .listRowBackground(Color.ocPanel)

                if let error {
                    Text(error).foregroundStyle(Color.ocDanger)
                        .listRowBackground(Color.ocPanel)
                }
            }
            .ocScreen()
            .navigationTitle("Play on a real board")
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .cancellationAction) {
                    Button("Cancel") { dismiss() }
                }
                ToolbarItem(placement: .confirmationAction) {
                    if busy {
                        ProgressView()
                    } else {
                        Button("Start") { Task { await start() } }
                            .disabled(!auth.signedIn)
                    }
                }
            }
            .onChange(of: helpTrain) { _, on in
                if on && !settings.photoConsentGiven {
                    showLicence = true
                }
            }
            .sheet(isPresented: $showLicence) {
                PhotoLicenceSheet { accepted in
                    showLicence = false
                    helpTrain = accepted
                }
                .environmentObject(settings)
                .environmentObject(auth)
                .ocAppearance()
            }
            .task {
                if chessnut.isConnected { source = .chessnut }
                let (reader, _) = await model.boardReader()
                readerMissing = reader == nil
            }
        }
    }

    private var me: String {
        settings.username.isEmpty ? "you" : settings.username
    }

    private var sourceFooter: String {
        switch source {
        case .chessnut: return "Moves made on the board are recorded as you play. A position the game can't reach is shown in red with what to put right."
        case .photos: return "After each move, take a photo (two angles help). The app finds the move; if it isn't sure, you pick it."
        case .manual: return "Tap each move on the phone's board as it's played."
        }
    }

    private func start() async {
        error = nil
        let name = opponent.trimmed
        guard auth.signedIn else {
            error = "Sign in under Account first."
            return
        }
        guard !name.isEmpty else {
            error = "Who are you playing? Enter their open-chess username."
            return
        }
        if stream && StreamTarget.url(
            service: settings.streamService, custom: settings.streamURL,
            key: Keychain.get(AppSettings.streamKeyAccount) ?? ""
        ) == nil {
            error = "Add a stream key in Streaming settings first (or turn Stream off)."
            return
        }
        if helpTrain && !settings.photoConsentGiven {
            helpTrain = false
        }
        busy = true
        defer { busy = false }
        if settings.username.isEmpty {
            await auth.refreshAccount()
        }
        let me = settings.username
        guard !me.isEmpty else {
            error = "Couldn't read your username from the server; try again."
            return
        }
        guard name.lowercased() != me.lowercased() else {
            error = "Enter the other player's username, not yours."
            return
        }
        do {
            _ = try await auth.server.get("/api/user/\(APIClient.pathComponent(name))")
        } catch {
            if let e = error as? APIError, e.status == 404 {
                self.error = "There's no open-chess player called \(name)."
            } else {
                self.error = describe(error)
            }
            return
        }
        let setup = LiveSetup(
            white: iAmWhite ? me : name,
            black: iAmWhite ? name : me,
            me: me,
            speed: speed,
            rated: rated,
            source: source,
            helpTrain: helpTrain,
            stream: stream,
            overlay: overlay
        )
        do {
            let data = try await auth.server.postJSON("/api/otb", object: setup.startBody)
            guard let obj = JSONAny.object(data), let id = obj["id"] as? String, !id.isEmpty else {
                error = "The server didn't start the game."
                return
            }
            let live = try LiveBoardModel(
                app: model,
                setup: setup,
                gameId: id,
                url: obj["url"] as? String ?? "",
                broadcastURL: obj["broadcastUrl"] as? String
            )
            dismiss()
            let app = model
            Task { @MainActor in
                try? await Task.sleep(nanoseconds: 350_000_000)
                app.liveBoard = live
            }
        } catch {
            self.error = describe(error)
        }
    }
}
