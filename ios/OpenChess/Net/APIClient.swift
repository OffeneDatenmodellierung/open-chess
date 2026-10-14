import Foundation

struct APIError: Error {
    let status: Int
    let message: String

    static func transport(_ e: Error) -> APIError {
        APIError(status: 0, message: e.localizedDescription)
    }
}

/// A small client for the open-chess server and the Lichess-compatible
/// Board API. Bearer token auth; JSON or form bodies.
struct APIClient {
    let base: URL
    let token: String?
    var session: URLSession = .shared

    func url(_ path: String, query: [String: String] = [:]) -> URL {
        var trimmedPath = path
        if trimmedPath.hasPrefix("/") { trimmedPath.removeFirst() }
        let baseString = base.absoluteString.hasSuffix("/") ? base.absoluteString : base.absoluteString + "/"
        var comps = URLComponents(string: baseString + trimmedPath) ?? URLComponents()
        if !query.isEmpty {
            comps.queryItems = query.sorted { $0.key < $1.key }.map { URLQueryItem(name: $0.key, value: $0.value) }
        }
        return comps.url ?? base
    }

    func makeRequest(_ method: String, _ path: String, query: [String: String] = [:]) -> URLRequest {
        var r = URLRequest(url: url(path, query: query))
        r.httpMethod = method
        r.timeoutInterval = 30
        if let token, !token.isEmpty {
            r.setValue("Bearer \(token)", forHTTPHeaderField: "Authorization")
        }
        r.setValue("application/json", forHTTPHeaderField: "Accept")
        return r
    }

    @discardableResult
    func send(_ request: URLRequest) async throws -> Data {
        let result: (Data, URLResponse)
        do {
            result = try await session.data(for: request)
        } catch {
            throw APIError.transport(error)
        }
        let data = result.0
        let response = result.1
        let status = (response as? HTTPURLResponse)?.statusCode ?? 0
        guard (200..<300).contains(status) else {
            throw APIError(status: status, message: APIClient.errorMessage(status: status, data: data))
        }
        return data
    }

    static func errorMessage(status: Int, data: Data) -> String {
        if let obj = JSONAny.object(data) {
            if let e = obj["error"] as? String { return e }
            if let e = obj["message"] as? String { return e }
        }
        let body = String(data: data.prefix(200), encoding: .utf8)?.trimmed ?? ""
        switch status {
        case 401: return "Not signed in (the server said 401)."
        case 403: return "Not allowed (403)."
        case 404: return "Not found (404)."
        case 429: return "Too many requests; try again shortly."
        default: return body.isEmpty ? "The server answered \(status)." : "\(status): \(body)"
        }
    }

    // MARK: Conveniences

    func get(_ path: String, query: [String: String] = [:]) async throws -> Data {
        try await send(makeRequest("GET", path, query: query))
    }

    @discardableResult
    func postJSON(_ path: String, body: Data) async throws -> Data {
        var r = makeRequest("POST", path)
        r.setValue("application/json", forHTTPHeaderField: "Content-Type")
        r.httpBody = body
        return try await send(r)
    }

    @discardableResult
    func postJSON(_ path: String, object: [String: Any]) async throws -> Data {
        try await postJSON(path, body: JSONAny.data(object))
    }

    @discardableResult
    func postForm(_ path: String, form: [String: String]) async throws -> Data {
        var r = makeRequest("POST", path)
        r.setValue("application/x-www-form-urlencoded", forHTTPHeaderField: "Content-Type")
        r.httpBody = Data(APIClient.formEncode(form).utf8)
        return try await send(r)
    }

    @discardableResult
    func delete(_ path: String) async throws -> Data {
        try await send(makeRequest("DELETE", path))
    }

    static func formEncode(_ form: [String: String]) -> String {
        var allowed = CharacterSet.alphanumerics
        allowed.insert(charactersIn: "-._~")
        return form.sorted { $0.key < $1.key }.map { key, value in
            let k = key.addingPercentEncoding(withAllowedCharacters: allowed) ?? key
            let v = value.addingPercentEncoding(withAllowedCharacters: allowed) ?? value
            return "\(k)=\(v)"
        }.joined(separator: "&")
    }

    static func pathComponent(_ s: String) -> String {
        var allowed = CharacterSet.alphanumerics
        allowed.insert(charactersIn: "-._~")
        return s.addingPercentEncoding(withAllowedCharacters: allowed) ?? s
    }
}
