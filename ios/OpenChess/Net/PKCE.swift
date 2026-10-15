import CryptoKit
import Foundation
import Security

/// OAuth 2 PKCE (S256) values.
struct PKCE {
    let verifier: String
    let challenge: String
    let state: String

    init() {
        verifier = PKCE.random(32)
        challenge = Data(SHA256.hash(data: Data(verifier.utf8))).base64URL
        state = PKCE.random(16)
    }

    static func random(_ count: Int) -> String {
        var bytes = [UInt8](repeating: 0, count: count)
        if SecRandomCopyBytes(kSecRandomDefault, count, &bytes) != errSecSuccess {
            for i in 0..<count { bytes[i] = UInt8.random(in: 0...255) }
        }
        return Data(bytes).base64URL
    }
}
