import AuthenticationServices
import SwiftUI

/// Apple's own "Sign in with Apple" button (white, on the dark theme),
/// the "Choose a username" sheet for a new account, and the last problem.
/// `link`: add the Apple ID to the signed-in account instead.
@MainActor
struct AppleSignInButton: View {
    var link = false

    @EnvironmentObject var auth: AuthManager
    @State private var origin = UUID()

    var body: some View {
        VStack(alignment: .leading, spacing: 6) {
            SignInWithAppleButton(link ? .continue : .signIn) { request in
                auth.prepareApple(request)
            } onCompletion: { result in
                let o = origin
                let l = link
                Task { await auth.completeApple(result, link: l, origin: o) }
            }
            .signInWithAppleButtonStyle(.white)
            .frame(maxWidth: 375)
            .frame(height: 50)
            .disabled(auth.appleBusy)
            .accessibilityLabel(link ? "Link your Apple ID" : "Sign in with Apple")
            if auth.appleBusy {
                ProgressView().controlSize(.small)
            }
            if let e = auth.appleError, e.origin == origin {
                Text(e.message)
                    .font(OCFont.text(12))
                    .foregroundStyle(Color.ocDanger)
                    .fixedSize(horizontal: false, vertical: true)
            }
        }
        .sheet(item: Binding(
            get: { auth.appleUsername?.origin == origin ? auth.appleUsername : nil },
            set: { value in
                if value == nil && auth.appleUsername?.origin == origin {
                    auth.cancelAppleUsername()
                }
            }
        )) { request in
            ChooseUsernameSheet(request: request)
                .environmentObject(auth)
                .ocAppearance()
        }
    }
}

/// A new Apple account's username: prefilled with the server's suggestion.
@MainActor
struct ChooseUsernameSheet: View {
    let request: AppleUsernameRequest
    @EnvironmentObject var auth: AuthManager
    @State private var name = ""

    var body: some View {
        NavigationStack {
            Form {
                Section {
                    TextField("Username", text: $name)
                        .textInputAutocapitalization(.never)
                        .autocorrectionDisabled()
                        .onSubmit { submit() }
                    if let m = request.message {
                        Text(m).font(OCFont.text(13)).foregroundStyle(Color.ocDanger)
                    }
                } header: {
                    SectionTitle(text: "Your open-chess name")
                } footer: {
                    Text("Other players see it on games and ratings.")
                }
                .listRowBackground(Color.ocPanel)
            }
            .ocScreen()
            .navigationTitle("Choose a username")
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .cancellationAction) {
                    Button("Cancel") { auth.cancelAppleUsername() }
                }
                ToolbarItem(placement: .confirmationAction) {
                    if auth.appleBusy {
                        ProgressView()
                    } else {
                        Button("Continue") { submit() }
                            .disabled(name.trimmed.isEmpty)
                    }
                }
            }
            .onAppear { name = request.suggestion }
        }
    }

    private func submit() {
        let n = name.trimmed
        guard !n.isEmpty else { return }
        Task { await auth.submitAppleUsername(n) }
    }
}
