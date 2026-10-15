import Foundation

/// Unix milliseconds now.
func nowMs() -> UInt64 {
    UInt64(max(0, Date().timeIntervalSince1970 * 1000))
}

/// A readable message for any error, including the core's `MobileError`
/// (whose case names we don't depend on).
func describe(_ error: Error) -> String {
    if let e = error as? APIError {
        return e.message
    }
    if let e = error as? URLError {
        return networkMessage(e)
    }
    if let e = error as? LocalizedError, let d = e.errorDescription {
        return d
    }
    let s = String(describing: error)
    return s.isEmpty ? "Something went wrong." : s
}

/// A network failure in words, naming the server it was trying.
func networkMessage(_ e: URLError) -> String {
    let host = e.failingURL?.host.map { " (\($0))" } ?? ""
    switch e.code {
    case .cannotFindHost, .dnsLookupFailed:
        return "Couldn't find the server\(host). Check the address under Account › Server."
    case .cannotConnectToHost:
        return "The server\(host) isn't answering. Is it running?"
    case .notConnectedToInternet, .dataNotAllowed:
        return "You're offline. Games still play; they sync when you're back online."
    case .networkConnectionLost:
        return "The connection dropped. Try again."
    case .timedOut:
        return "The server\(host) took too long to answer. Try again."
    case .secureConnectionFailed, .serverCertificateUntrusted, .serverCertificateHasBadDate,
         .serverCertificateNotYetValid, .serverCertificateHasUnknownRoot, .clientCertificateRejected:
        return "Couldn't make a secure connection to the server\(host): check its certificate."
    case .appTransportSecurityRequiresSecureConnection:
        return "The server\(host) needs an https:// address."
    default:
        return e.localizedDescription
    }
}

/// "just now", "4 min ago", "2 h ago", "3 d ago".
func agoText(ms: UInt64?) -> String {
    guard let ms, ms > 0 else { return "never" }
    let now = nowMs()
    let secs = now > ms ? (now - ms) / 1000 : 0
    if secs < 45 { return "just now" }
    let mins = (secs + 30) / 60
    if mins < 60 { return "\(mins) min ago" }
    let hours = mins / 60
    if hours < 24 { return "\(hours) h ago" }
    return "\(hours / 24) d ago"
}

func byteText(_ n: UInt64) -> String {
    let f = ByteCountFormatter()
    f.countStyle = .file
    return f.string(fromByteCount: Int64(clamping: n))
}

func newClientId() -> String {
    UUID().uuidString.lowercased()
}

extension Data {
    var hexString: String {
        map { String(format: "%02x", $0) }.joined()
    }

    /// base64url without padding (PKCE).
    var base64URL: String {
        base64EncodedString()
            .replacingOccurrences(of: "+", with: "-")
            .replacingOccurrences(of: "/", with: "_")
            .replacingOccurrences(of: "=", with: "")
    }
}

extension String {
    var trimmed: String { trimmingCharacters(in: .whitespacesAndNewlines) }
}

/// JSON helpers for loosely shaped server answers.
enum JSONAny {
    static func object(_ data: Data) -> [String: Any]? {
        (try? JSONSerialization.jsonObject(with: data)) as? [String: Any]
    }

    static func data(_ value: Any) -> Data {
        (try? JSONSerialization.data(withJSONObject: value)) ?? Data("{}".utf8)
    }
}
