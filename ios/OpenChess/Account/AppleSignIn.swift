import CryptoKit
import Foundation
import Security

/// Sign in with Apple's nonce: 32 random bytes as a URL-safe string (sent
/// raw to the server), and its SHA-256 as lowercase hex (set on Apple's
/// request, so the identity token carries it). Pure Swift: the logic tests
/// compile this file.
enum AppleNonce {
    /// A fresh raw nonce (43 characters of base64url).
    static func make() -> String {
        var bytes = [UInt8](repeating: 0, count: 32)
        if SecRandomCopyBytes(kSecRandomDefault, bytes.count, &bytes) != errSecSuccess {
            for i in 0..<bytes.count { bytes[i] = UInt8.random(in: 0...255) }
        }
        return Data(bytes).base64EncodedString()
            .replacingOccurrences(of: "+", with: "-")
            .replacingOccurrences(of: "/", with: "_")
            .replacingOccurrences(of: "=", with: "")
    }

    /// SHA-256 of the string's UTF-8, lowercase hex.
    static func sha256Hex(_ s: String) -> String {
        SHA256.hash(data: Data(s.utf8)).map { String(format: "%02x", $0) }.joined()
    }
}
