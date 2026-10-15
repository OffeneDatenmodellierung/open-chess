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
    /// Sign in with Apple: whether the account has an Apple ID linked, a
    /// sign-in waiting for a username, and the last problem (with the
    /// button that started it).
    @Published private(set) var appleLinked = false
    @Published var appleUsername: AppleUsernameRequest?
    @Published private(set) var appleBusy = false
    @Published var appleError: (origin: UUID, message: String)?
    private var appleNonce = ""
    private var applePending: ApplePending?

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

    static let needServer = "Enter your open-chess server under Account › Server first."

    func signIn() async {
        error = nil
        guard settings.hasServer else { error = AuthManager.needServer; return }
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
            appleLinked = account.apple
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
        if value == nil { appleLinked = false }
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

    // MARK: Sign in with Apple

    /// Apple's sheet is about to show: ask for name and email, with a
    /// fresh nonce (Apple gets its SHA-256; the server gets it raw).
    func prepareApple(_ request: ASAuthorizationAppleIDRequest) {
        appleNonce = AppleNonce.make()
        request.requestedScopes = [.fullName, .email]
        request.nonce = AppleNonce.sha256Hex(appleNonce)
    }

    /// Apple's sheet answered. `link`: add the Apple ID to the signed-in
    /// account (sent with the bearer token) instead of signing in.
    func completeApple(_ result: Result<ASAuthorization, Error>, link: Bool, origin: UUID) async {
        appleError = nil
        switch result {
        case .failure(let error):
            if let e = error as? ASAuthorizationError, e.code == .canceled { return }
            appleError = (origin: origin, message: "Sign in with Apple didn't finish: \(describe(error))")
        case .success(let authorization):
            guard let credential = authorization.credential as? ASAuthorizationAppleIDCredential,
                  let tokenData = credential.identityToken,
                  let identityToken = String(data: tokenData, encoding: .utf8),
                  let codeData = credential.authorizationCode,
                  let code = String(data: codeData, encoding: .utf8) else {
                appleError = (origin: origin, message: "Apple sent no identity token; try again.")
                return
            }
            var name: String?
            if let parts = credential.fullName {
                let formatted = PersonNameComponentsFormatter().string(from: parts).trimmed
                name = formatted.isEmpty ? nil : formatted
            }
            let pending = ApplePending(
                identityToken: identityToken,
                authorizationCode: code,
                nonce: appleNonce,
                name: name,
                email: credential.email,
                link: link,
                origin: origin,
                at: Date()
            )
            applePending = pending
            await sendApple(pending, username: nil)
        }
    }

    /// The username chosen for a new account: the same Apple answer again,
    /// with the name (while Apple's code is still fresh).
    func submitAppleUsername(_ username: String) async {
        guard let pending = applePending else {
            appleUsername = nil
            return
        }
        if Date().timeIntervalSince(pending.at) > 270 {
            appleUsername = nil
            applePending = nil
            appleError = (origin: pending.origin, message: "That took a while, so Apple's sign-in ran out. Sign in with Apple again.")
            return
        }
        await sendApple(pending, username: username.trimmed)
    }

    func cancelAppleUsername() {
        appleUsername = nil
        applePending = nil
    }

    private func sendApple(_ pending: ApplePending, username: String?) async {
        guard settings.hasServer else {
            appleError = (origin: pending.origin, message: AuthManager.needServer)
            return
        }
        appleBusy = true
        defer { appleBusy = false }
        var body: [String: Any] = [
            "identityToken": pending.identityToken,
            "authorizationCode": pending.authorizationCode,
            "nonce": pending.nonce,
            "clientId": "open-chess iOS",
            "scopes": AuthManager.scope.split(separator: " ").map(String.init) + ["email:read"],
        ]
        if let username, !username.isEmpty { body["username"] = username }
        if let name = pending.name { body["name"] = name }
        if let email = pending.email { body["email"] = email }
        let client = pending.link ? server : APIClient(base: settings.serverBase, token: nil)
        var request = client.makeRequest("POST", "/api/auth/apple")
        request.setValue("application/json", forHTTPHeaderField: "Content-Type")
        request.httpBody = JSONAny.data(body)
        let status: Int
        let obj: [String: Any]
        do {
            let (data, response) = try await URLSession.shared.data(for: request)
            status = (response as? HTTPURLResponse)?.statusCode ?? 0
            obj = JSONAny.object(data) ?? [:]
        } catch {
            appleError = (origin: pending.origin, message: describe(error))
            return
        }
        let code = obj["error"] as? String ?? ""
        let message = obj["message"] as? String ?? (code.isEmpty ? "The server answered \(status)." : code)
        if (200..<300).contains(status) {
            applePending = nil
            appleUsername = nil
            if !pending.link {
                let token = obj["access_token"] as? String ?? ""
                guard !token.isEmpty else {
                    appleError = (origin: pending.origin, message: "The server sent no token.")
                    return
                }
                setToken(token)
            }
            if let name = obj["username"] as? String, !name.isEmpty {
                settings.username = name
            }
            appleLinked = true
            await refreshAccount()
            return
        }
        if status == 409 && code == "choose_username" {
            let suggestion = obj["suggestion"] as? String ?? (username ?? "")
            appleUsername = AppleUsernameRequest(origin: pending.origin, suggestion: suggestion, message: nil)
            return
        }
        if status == 400 && code == "bad_username" {
            // Same sheet (same id), now with the reason.
            if var r = appleUsername {
                r.message = message
                appleUsername = r
            } else {
                appleUsername = AppleUsernameRequest(origin: pending.origin, suggestion: username ?? "", message: message)
            }
            return
        }
        // apple_linked_elsewhere, already_linked, email_in_use, 403 and the rest.
        applePending = nil
        appleUsername = nil
        appleError = (origin: pending.origin, message: message)
    }

    /// `DELETE /api/auth/apple`: nil when done, else why not.
    func unlinkApple() async -> String? {
        var request = server.makeRequest("DELETE", "/api/auth/apple")
        request.timeoutInterval = 30
        do {
            let (data, response) = try await URLSession.shared.data(for: request)
            let status = (response as? HTTPURLResponse)?.statusCode ?? 0
            if (200..<300).contains(status) {
                appleLinked = false
                return nil
            }
            let obj = JSONAny.object(data) ?? [:]
            return obj["message"] as? String ?? obj["error"] as? String ?? "The server answered \(status)."
        } catch {
            return describe(error)
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

/// Apple's answer, kept while a new account picks its username (sent
/// again unchanged, with the name).
struct ApplePending {
    let identityToken: String
    let authorizationCode: String
    let nonce: String
    let name: String?
    let email: String?
    let link: Bool
    let origin: UUID
    let at: Date
}

/// "Choose a username" for a new Apple account.
struct AppleUsernameRequest: Identifiable {
    let id = UUID()
    let origin: UUID
    var suggestion: String
    var message: String?
}
