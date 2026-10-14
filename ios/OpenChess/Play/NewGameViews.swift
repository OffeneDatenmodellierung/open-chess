import OpenChessCore
import SwiftUI

/// Play the computer: level 1–8, colour, time control.
@MainActor
struct NewComputerGameView: View {
    @EnvironmentObject var model: AppModel
    @EnvironmentObject var settings: AppSettings
    @Environment(\.dismiss) var dismiss

    /// A position to start from (from the board editor), or nil.
    var fen: String?
    var training = false

    @State var level = 3
    @State var colour = "random"
    @State var time = TimeControl.untimed
    @State var error: String?

    var body: some View {
        NavigationStack {
            Form {
                Section("Level") {
                    Stepper(value: $level, in: 1...8) {
                        HStack {
                            Text("Level \(level)")
                            Spacer()
                            Text("\(Int(botRating(level: UInt8(level)).rating))")
                                .font(OCFont.mono(15))
                                .foregroundStyle(Color.ocMuted)
                        }
                    }
                }
                Section("Colour") {
                    Picker("Colour", selection: $colour) {
                        Text("White").tag("white")
                        Text("Random").tag("random")
                        Text("Black").tag("black")
                    }
                    .pickerStyle(.segmented)
                }
                Section {
                    Picker("Time", selection: $time) {
                        ForEach(TimeControl.choices, id: \.self) { tc in
                            Text(tc.label).tag(tc)
                        }
                    }
                } header: {
                    Text("Time control")
                } footer: {
                    Text(training
                         ? "Training games have hints on and are never rated."
                         : "Rated as \(time.speed) when you use no hints or takebacks. Your rating updates on the device now and on the server at the next sync.")
                }
                if let error {
                    Text(error).foregroundStyle(Color.ocDanger)
                }
            }
            .ocScreen()
            .navigationTitle(training ? "Train with the computer" : "Play the computer")
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .cancellationAction) {
                    Button("Cancel") { dismiss() }
                }
                ToolbarItem(placement: .confirmationAction) {
                    Button("Play") { start() }
                }
            }
            .onAppear { level = min(8, max(1, settings.botLevel)) }
        }
    }

    private func start() {
        settings.botLevel = level
        let white: Bool
        switch colour {
        case "white": white = true
        case "black": white = false
        default: white = Bool.random()
        }
        let me = settings.username.isEmpty ? "You" : settings.username
        let bot = "ai-level-\(level)"
        do {
            let g = try GameViewModel(
                app: model,
                kind: .computer(level: level, playerWhite: white),
                fen: fen,
                time: time,
                white: white ? me : bot,
                black: white ? bot : me,
                training: training
            )
            dismiss()
            // Let the sheet go before the full-screen game appears.
            let app = model
            Task { @MainActor in
                try? await Task.sleep(nanoseconds: 350_000_000)
                app.showGame(g)
            }
        } catch {
            self.error = describe(error)
        }
    }
}

/// Pass and play: the other player's username goes with the record (the
/// server asks them to confirm).
@MainActor
struct NewPassAndPlayView: View {
    @EnvironmentObject var model: AppModel
    @EnvironmentObject var settings: AppSettings
    @Environment(\.dismiss) var dismiss

    var fen: String?

    @State var opponent = ""
    @State var iAmWhite = true
    @State var time = TimeControl.untimed
    @State var error: String?

    var body: some View {
        NavigationStack {
            Form {
                Section {
                    TextField("Their username (optional)", text: $opponent)
                        .textInputAutocapitalization(.never)
                        .autocorrectionDisabled()
                } header: {
                    Text("The other player")
                } footer: {
                    Text("With a username the game is sent to the server, which asks them to confirm it before it's rated. Without one it stays on this device.")
                }
                Section("You play") {
                    Picker("You play", selection: $iAmWhite) {
                        Text("White").tag(true)
                        Text("Black").tag(false)
                    }
                    .pickerStyle(.segmented)
                }
                Section("Time control") {
                    Picker("Time", selection: $time) {
                        ForEach(TimeControl.choices, id: \.self) { tc in
                            Text(tc.label).tag(tc)
                        }
                    }
                }
                Section {
                    Toggle("Turn the board after each move", isOn: $settings.flipPassAndPlay)
                } footer: {
                    Text("On iPhone, so each player sees their side at the bottom.")
                }
                if let error {
                    Text(error).foregroundStyle(Color.ocDanger)
                }
            }
            .ocScreen()
            .navigationTitle("Pass and play")
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .cancellationAction) {
                    Button("Cancel") { dismiss() }
                }
                ToolbarItem(placement: .confirmationAction) {
                    Button("Start") { start() }
                }
            }
        }
    }

    private func start() {
        let me = settings.username.isEmpty ? "Player 1" : settings.username
        let other = opponent.trimmed.isEmpty ? "Player 2" : opponent.trimmed
        do {
            let g = try GameViewModel(
                app: model,
                kind: .passAndPlay,
                fen: fen,
                time: time,
                white: iAmWhite ? me : other,
                black: iAmWhite ? other : me,
                // No account on either side: nothing for the server to rate.
                keepLocal: settings.username.isEmpty || opponent.trimmed.isEmpty
            )
            dismiss()
            // Let the sheet go before the full-screen game appears.
            let app = model
            Task { @MainActor in
                try? await Task.sleep(nanoseconds: 350_000_000)
                app.showGame(g)
            }
        } catch {
            self.error = describe(error)
        }
    }
}
