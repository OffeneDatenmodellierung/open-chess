import SwiftUI

/// Where sign-in is needed: a short reason and a button that signs in
/// right here (the same web sign-in as Account). The caller shows it only
/// while signed out, so it goes away on success; cancelling leaves it.
@MainActor
struct SignInPrompt: View {
    enum Service {
        case openChess, lichess
    }

    let message: String
    var service: Service = .openChess
    /// Inside a Form/List (no card of its own).
    var inList = false

    @EnvironmentObject var auth: AuthManager
    @EnvironmentObject var settings: AppSettings
    @State private var serverDraft = ""
    @State private var serverProblem: String?

    var body: some View {
        if inList {
            content
        } else {
            content.ocCard(padding: 12)
        }
    }

    private var content: some View {
        VStack(alignment: .leading, spacing: 8) {
            Text(message)
                .font(OCFont.text(14))
                .foregroundStyle(Color.ocMuted)
            if service == .openChess && !settings.hasServer {
                serverEntry
            } else {
                if service == .openChess {
                    AppleSignInButton()
                }
                Button {
                    Task {
                        switch service {
                        case .openChess: await auth.signIn()
                        case .lichess: await auth.signInLichess()
                        }
                    }
                } label: {
                    HStack {
                        Text(buttonTitle)
                        if busy {
                            Spacer()
                            ProgressView()
                        }
                    }
                }
                .buttonStyle(OCButtonStyle(kind: .primary, compact: true))
                .disabled(busy)
                .accessibilityLabel(buttonTitle)
                .accessibilityHint(service == .lichess
                                   ? "Opens lichess.org to sign in, then comes back here."
                                   : "Opens the open-chess sign-in page, then comes back here.")
                if let e = error {
                    Text(e).font(OCFont.text(12)).foregroundStyle(Color.ocDanger)
                }
            }
        }
    }

    /// No server yet (a build made without one): ask for it here, then
    /// the sign-in buttons appear.
    private var serverEntry: some View {
        VStack(alignment: .leading, spacing: 8) {
            Text("Your open-chess server")
                .font(OCFont.text(13, weight: .semibold))
            TextField("https://your-server", text: $serverDraft)
                .keyboardType(.URL)
                .textContentType(.URL)
                .textInputAutocapitalization(.never)
                .autocorrectionDisabled()
                .submitLabel(.done)
                .onSubmit { useServer() }
                .padding(10)
                .background(Color.ocPanel, in: RoundedRectangle(cornerRadius: 10))
            Button("Use this server") { useServer() }
                .buttonStyle(OCButtonStyle(kind: .primary, compact: true))
                .disabled(serverDraft.trimmed.isEmpty)
            if let p = serverProblem {
                Text(p).font(OCFont.text(12)).foregroundStyle(Color.ocDanger)
            }
        }
    }

    private func useServer() {
        var s = serverDraft.trimmed
        if !s.isEmpty && !s.contains("://") { s = "https://" + s }
        guard AppSettings.isServerAddress(s) else {
            serverProblem = "That isn't a server address (https://…)."
            return
        }
        serverProblem = nil
        auth.error = nil
        settings.serverURL = s
        settings.syncSince = 0
    }

    private var buttonTitle: String {
        service == .lichess ? "Sign in with Lichess" : "Sign in to open-chess"
    }

    private var busy: Bool {
        service == .lichess ? auth.lichessBusy : auth.busy
    }

    private var error: String? {
        service == .lichess ? auth.lichessError : auth.error
    }
}
