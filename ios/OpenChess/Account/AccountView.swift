import OpenChessCore
import SwiftUI

/// Sign in and out, the server, the photo licence, appearance, deleting
/// the account, and credits.
@MainActor
struct AccountView: View {
    @EnvironmentObject var model: AppModel
    @EnvironmentObject var settings: AppSettings
    @EnvironmentObject var auth: AuthManager
    @State var serverDraft = ""
    @State var confirmDelete = false
    @State var showLicence = false
    @State var deleted = false
    @State var confirmUnlink = false
    @State var appleNote: String?

    var body: some View {
        Form {
            Section {
                if auth.signedIn {
                    HStack {
                        Text("Signed in as")
                        Spacer()
                        Text(settings.username.isEmpty ? "…" : settings.username)
                            .font(OCFont.text(15, weight: .semibold))
                    }
                    Button("Sign out") {
                        Task { await auth.signOut() }
                    }
                    if auth.appleLinked {
                        HStack {
                            Image(systemName: "apple.logo")
                            Text("Apple ID")
                            Spacer()
                            Text("Linked").foregroundStyle(Color.ocMuted)
                        }
                        Button("Unlink Apple ID", role: .destructive) { confirmUnlink = true }
                    } else {
                        VStack(alignment: .leading, spacing: 6) {
                            Text("Link Apple ID").font(OCFont.text(15, weight: .semibold))
                            Text("Then you can also sign in with Apple.")
                                .font(OCFont.text(12)).foregroundStyle(Color.ocMuted)
                            AppleSignInButton(link: true)
                        }
                    }
                    if let n = appleNote {
                        Text(n).font(OCFont.text(13)).foregroundStyle(Color.ocDanger)
                    }
                } else {
                    AppleSignInButton()
                    Button {
                        Task { await auth.signIn() }
                    } label: {
                        HStack {
                            Text("Sign in to open-chess")
                            Spacer()
                            if auth.busy { ProgressView() }
                        }
                    }
                    .disabled(auth.busy)
                }
                if let e = auth.error {
                    Text(e).font(OCFont.text(13)).foregroundStyle(Color.ocDanger)
                }
            } header: {
                SectionTitle(text: "Account")
            } footer: {
                Text("Games play offline without an account; signing in sends them to the server to be rated.")
            }
            .listRowBackground(Color.ocPanel)

            Section {
                TextField("https://your-server", text: $serverDraft)
                    .keyboardType(.URL)
                    .textInputAutocapitalization(.never)
                    .autocorrectionDisabled()
                    .onSubmit { saveServer() }
                if serverDraft.trimmed != settings.serverURL {
                    Button("Use this server") { saveServer() }
                }
                if !AppSettings.defaultServer.isEmpty && settings.serverURL != AppSettings.defaultServer {
                    Button("Reset to default") {
                        serverDraft = AppSettings.defaultServer
                        saveServer()
                    }
                }
            } header: {
                SectionTitle(text: "Server")
            } footer: {
                Text("Changing the server signs you out.")
            }
            .listRowBackground(Color.ocPanel)

            Section {
                Picker("Appearance", selection: $settings.appearance) {
                    Text("Dark").tag("dark")
                    Text("Light").tag("light")
                    Text("Match system").tag("system")
                }
                Toggle("Turn the board in pass and play", isOn: $settings.flipPassAndPlay)
                Toggle("Sounds", isOn: $settings.sounds)
                Toggle("Haptics", isOn: $settings.haptics)
                Toggle("Speak moves", isOn: $settings.speakMoves)
                    .accessibilityHint("Reads the opponent's or computer's moves aloud, and every move recorded on a real board.")
                Toggle("Moves in plain English", isOn: $settings.plainMoves)
                    .accessibilityLabel("Moves in plain English")
                    .accessibilityHint("Shows each move in words, like Knight from b1 to c3, next to the chess notation.")
                NavigationLink("Streaming") { StreamSettingsView() }
                NavigationLink("Sync and data") { SyncSettingsView() }
                NavigationLink("Chessnut board") { ChessnutView() }
            } header: {
                SectionTitle(text: "Settings")
            }
            .listRowBackground(Color.ocPanel)

            Section {
                HStack {
                    Text("Photo licence")
                    Spacer()
                    Text(consentText).foregroundStyle(Color.ocMuted)
                }
                Button("Read and answer") { showLicence = true }
                if settings.photoConsentGiven {
                    Button("Withdraw", role: .destructive) { withdraw() }
                }
            } header: {
                SectionTitle(text: "Photos")
            }
            .listRowBackground(Color.ocPanel)

            Section {
                NavigationLink("About and credits") { AboutView() }
            }
            .listRowBackground(Color.ocPanel)

            if auth.signedIn {
                Section {
                    Button("Delete my account", role: .destructive) { confirmDelete = true }
                } footer: {
                    Text("Deletes your account and its games on \(settings.serverBase.host ?? "the server"). This can't be undone.")
                }
                .listRowBackground(Color.ocPanel)
            }
        }
        .ocScreen()
        .navigationTitle("Account")
        // Inline: the large title left an empty band above the page.
        .navigationBarTitleDisplayMode(.inline)
        .onAppear {
            serverDraft = settings.serverURL
            Task { await auth.refreshAccount() }
        }
        .alert("Delete your account?", isPresented: $confirmDelete) {
            Button("Delete", role: .destructive) {
                Task { deleted = await auth.deleteAccount() }
            }
            Button("Cancel", role: .cancel) {}
        } message: {
            Text("Your account, ratings and games on the server are deleted. Games on this device stay until you delete them.")
        }
        .alert("Account deleted", isPresented: $deleted) {
            Button("OK", role: .cancel) {}
        }
        .alert("Unlink your Apple ID?", isPresented: $confirmUnlink) {
            Button("Unlink", role: .destructive) {
                Task { appleNote = await auth.unlinkApple() }
            }
            Button("Cancel", role: .cancel) {}
        } message: {
            Text("You won't be able to sign in with Apple any more; sign in the usual way instead.")
        }
        .sheet(isPresented: $showLicence) {
            PhotoLicenceSheet { _ in showLicence = false }
                .environmentObject(settings)
                .environmentObject(auth)
                .ocAppearance()
        }
    }

    private var consentText: String {
        switch settings.photoConsent {
        case "accepted": return settings.photoPublic ? "Accepted · public" : "Accepted"
        case "declined": return "Declined"
        default: return "Not answered"
        }
    }

    private func saveServer() {
        let s = serverDraft.trimmed
        guard AppSettings.isServerAddress(s) else {
            auth.error = "That isn't a server address (https://…)."
            return
        }
        guard s != settings.serverURL else { return }
        Task {
            await auth.signOut()
            settings.serverURL = s
            settings.syncSince = 0
        }
    }

    private func withdraw() {
        settings.photoConsent = "declined"
        settings.photoPublic = false
        if auth.signedIn {
            let api = auth.server
            Task { _ = try? await api.postJSON("/api/esl/photo-terms", object: ["accept": false, "public": false]) }
        }
    }
}

@MainActor
struct AboutView: View {
    @EnvironmentObject var model: AppModel
    @State var card: ModelCardInfo?
    @State var readerNote: String?

    var body: some View {
        List {
            Section {
                Text("open-chess for iPhone and iPad. The rules, the computer, the hints, the ratings, the Chessnut board and the board reader are the open-chess Rust core, the same the server uses.")
                    .font(OCFont.text(14))
                Link("Source and licences (MIT OR Apache-2.0)", destination: URL(string: "https://github.com/OffeneDatenmodellierung/open-chess")!)
            }
            .listRowBackground(Color.ocPanel)
            Section {
                if let card {
                    LabeledContent("Model", value: [card.name, card.version].filter { !$0.isEmpty }.joined(separator: " "))
                    LabeledContent("Licence", value: card.licence)
                    if !card.base.isEmpty {
                        LabeledContent("Based on", value: card.base)
                    }
                    ForEach(card.datasets) { d in
                        VStack(alignment: .leading, spacing: 2) {
                            Text(d.name).font(OCFont.text(14, weight: .semibold))
                            Text(d.licence).font(OCFont.text(12)).foregroundStyle(Color.ocMuted)
                            if !d.attribution.isEmpty {
                                Text(d.attribution).font(OCFont.text(12)).foregroundStyle(Color.ocMuted)
                            }
                        }
                    }
                } else {
                    Text(readerNote ?? "Loading…").foregroundStyle(Color.ocMuted)
                }
            } header: {
                SectionTitle(text: "Board reader")
            }
            .listRowBackground(Color.ocPanel)
        }
        .ocScreen()
        .navigationTitle("About")
        .task {
            let (reader, note) = await model.boardReader()
            card = reader.flatMap { ModelCardInfo.decode($0.cardJson()) }
            readerNote = note
        }
    }
}
