import AuthenticationServices
import Foundation
import UIKit

/// Signing in to the open-chess server and to Lichess (OAuth 2 PKCE
/// through `ASWebAuthenticationSession`; a pasted Lichess token still
/// works), and deleting the account.
@MainActor
final class AuthManager: ObservableObject {
    static let clientId = "open-chess-ios"
    static let callbackScheme = "org.offenedatenmodellierung.openchess"
    static let redirectURI = "org.offenedatenmodellierung.openchess:/oauth"
    static let scope = "board:play challenge:read challenge:write"
    /// Lichess's OAuth needs no registered client: any id, and a redirect
    /// whose scheme has a dot.
    static let lichessRedirectURI = "org.offenedatenmodellierung.openchess:/lichess"
    static let lichessBase = URL(string: "https://lichess.org")!

    private static let tokenKey = "server-token"
    private static let lichessKey = "lichess-token"

    @Published private(set) var token: String?
    @Published private(set) var lichessToken: String?
    @Published var busy = false
    @Published var error: String?
    @Published var lichessBusy = false
    @Published var lichessError: String?

    let settings: AppSettings
    private var webSession: ASWebAuthenticationSession?
    private var webAnchor: WebAuthAnchor?

    init(settings: AppSettings) {
        self.settings = settings
        token = Keychain.get(AuthManager.tokenKey)
        lichessToken = Keychain.get(AuthManager.lichessKey)
    }

    var signedIn: Bool { (token ?? "").isEmpty == false }
    var hasLichess: Bool { (lichessToken ?? "").isEmpty == false }

    /// A client for the open-chess server, with the token when signed in.
    var server: APIClient {
        APIClient(base: settings.serverBase, token: token)
    }

    /// A client for lichess.org, with the Lichess token.
    var lichess: APIClient {
        APIClient(base: AuthManager.lichessBase, token: lichessToken)
    }

    // MARK: Sign in

    func signIn() async {
        error = nil
        busy = true
        defer { busy = false }
        let pkce = PKCE()
        let client = APIClient(base: settings.serverBase, token: nil)
        let authorize = client.url("/oauth", query: [
            "response_type": "code",
            "client_id": AuthManager.clientId,
            "redirect_uri": AuthManager.redirectURI,
            "code_challenge": pkce.challenge,
            "code_challenge_method": "S256",
            "scope": AuthManager.scope,
            "state": pkce.state,
        ])
        do {
            let callback = try await authenticate(url: authorize)
            let code = try AuthManager.code(from: callback, state: pkce.state)
            let data = try await client.postForm("/api/token", form: [
                "grant_type": "authorization_code",
                "code": code,
                "code_verifier": pkce.verifier,
                "redirect_uri": AuthManager.redirectURI,
                "client_id": AuthManager.clientId,
            ])
            let t = try JSONDecoder().decode(TokenResponse.self, from: data)
            guard !t.accessToken.isEmpty else {
                throw APIError(status: 0, message: "The server sent no token.")
            }
            setToken(t.accessToken)
            await refreshAccount()
        } catch {
            if let e = error as? ASWebAuthenticationSessionError, e.code == .canceledLogin {
                return
            }
            self.error = describe(error)
        }
    }

    /// The authorization code from the callback, checking the state.
    private static func code(from callback: URL, state expected: String) throws -> String {
        let items = URLComponents(url: callback, resolvingAgainstBaseURL: false)?.queryItems ?? []
        let code = items.first { $0.name == "code" }?.value ?? ""
        let state = items.first { $0.name == "state" }?.value ?? ""
        if let e = items.first(where: { $0.name == "error" })?.value {
            throw APIError(status: 0, message: "Sign-in was refused: \(e)")
        }
        guard state == expected else {
            throw APIError(status: 0, message: "Sign-in came back with the wrong state; try again.")
        }
        guard !code.isEmpty else {
            throw APIError(status: 0, message: "Sign-in came back without a code.")
        }
        return code
    }

    private func authenticate(url: URL) async throws -> URL {
        try await withCheckedThrowingContinuation { (cont: CheckedContinuation<URL, Error>) in
            let session = ASWebAuthenticationSession(
                url: url,
                callbackURLScheme: AuthManager.callbackScheme
            ) { callbackURL, error in
                if let error {
                    cont.resume(throwing: error)
                } else if let callbackURL {
                    cont.resume(returning: callbackURL)
                } else {
                    cont.resume(throwing: APIError(status: 0, message: "Sign-in ended without an answer."))
                }
            }
            let anchor = WebAuthAnchor.current()
            self.webAnchor = anchor
            session.presentationContextProvider = anchor
            session.prefersEphemeralWebBrowserSession = false
            self.webSession = session
            if !session.start() {
                cont.resume(throwing: APIError(status: 0, message: "Couldn't open the sign-in page."))
            }
        }
    }

    func refreshAccount() async {
        guard signedIn else { return }
        do {
            let data = try await server.get("/api/account")
            let account = try JSONDecoder().decode(AccountInfo.self, from: data)
            settings.username = account.username.isEmpty ? account.id : account.username
        } catch {
            if let e = error as? APIError, e.status == 401 {
                setToken(nil)
                settings.username = ""
            }
            self.error = describe(error)
        }
    }

    private func setToken(_ value: String?) {
        Keychain.set(AuthManager.tokenKey, value)
        token = value
    }

    func signOut() async {
        if signedIn {
            // Revoke the token; failing to reach the server still signs out here.
            _ = try? await server.delete("/api/token")
        }
        setToken(nil)
        settings.username = ""
    }

    /// `DELETE /api/account`, then sign out.
    func deleteAccount() async -> Bool {
        error = nil
        busy = true
        defer { busy = false }
        do {
            try await server.delete("/api/account")
            setToken(nil)
            settings.username = ""
            return true
        } catch {
            self.error = describe(error)
            return false
        }
    }

    // MARK: Lichess

    /// Sign in on lichess.org (PKCE; no client registration needed).
    func signInLichess() async {
        lichessError = nil
        lichessBusy = true
        defer { lichessBusy = false }
        let pkce = PKCE()
        let client = APIClient(base: AuthManager.lichessBase, token: nil)
        let authorize = client.url("/oauth", query: [
            "response_type": "code",
            "client_id": AuthManager.clientId,
            "redirect_uri": AuthManager.lichessRedirectURI,
            "code_challenge": pkce.challenge,
            "code_challenge_method": "S256",
            "scope": AuthManager.scope,
            "state": pkce.state,
        ])
        do {
            let callback = try await authenticate(url: authorize)
            let code = try AuthManager.code(from: callback, state: pkce.state)
            let data = try await client.postForm("/api/token", form: [
                "grant_type": "authorization_code",
                "code": code,
                "code_verifier": pkce.verifier,
                "redirect_uri": AuthManager.lichessRedirectURI,
                "client_id": AuthManager.clientId,
            ])
            let t = try JSONDecoder().decode(TokenResponse.self, from: data)
            guard !t.accessToken.isEmpty else {
                throw APIError(status: 0, message: "Lichess sent no token.")
            }
            setLichessToken(t.accessToken)
        } catch {
            if let e = error as? ASWebAuthenticationSessionError, e.code == .canceledLogin {
                return
            }
            lichessError = "Lichess: " + describe(error)
        }
    }

    /// Revoke the token on Lichess (`DELETE /api/token`) and forget it.
    func signOutLichess() async {
        if hasLichess {
            _ = try? await lichess.delete("/api/token")
        }
        setLichessToken(nil)
    }

    func setLichessToken(_ value: String?) {
        let v = value?.trimmed
        Keychain.set(AuthManager.lichessKey, v)
        lichessToken = (v ?? "").isEmpty ? nil : v
    }

    /// Check the pasted Lichess token by asking for the account.
    func lichessUsername() async -> String? {
        guard hasLichess else { return nil }
        do {
            let data = try await lichess.get("/api/account")
            let a = try JSONDecoder().decode(AccountInfo.self, from: data)
            return a.username.isEmpty ? a.id : a.username
        } catch {
            self.error = "Lichess: " + describe(error)
            return nil
        }
    }
}

/// The window sign-in is shown over, captured on the main actor when
/// sign-in starts.
final class WebAuthAnchor: NSObject, ASWebAuthenticationPresentationContextProviding {
    let anchor: ASPresentationAnchor

    init(anchor: ASPresentationAnchor) {
        self.anchor = anchor
    }

    func presentationAnchor(for session: ASWebAuthenticationSession) -> ASPresentationAnchor {
        anchor
    }

    @MainActor
    static func current() -> WebAuthAnchor {
        let scenes = UIApplication.shared.connectedScenes.compactMap { $0 as? UIWindowScene }
        for scene in scenes {
            if let w = scene.windows.first(where: { $0.isKeyWindow }) {
                return WebAuthAnchor(anchor: w)
            }
        }
        if let w = scenes.first?.windows.first {
            return WebAuthAnchor(anchor: w)
        }
        return WebAuthAnchor(anchor: ASPresentationAnchor())
    }
}
