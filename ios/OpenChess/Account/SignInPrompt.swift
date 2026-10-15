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
