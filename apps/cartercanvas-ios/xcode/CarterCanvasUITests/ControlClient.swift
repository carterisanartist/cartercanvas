import Foundation

/// A blocking client for the app's JSON-lines control channel (docs/control-protocol.md), used
/// by the UI tests to set up documents and check what touches did.
final class ControlClient {
    enum Failure: Error, CustomStringConvertible {
        case connect(String)
        case io(String)
        case reply(String)

        var description: String {
            switch self {
            case let .connect(m), let .io(m), let .reply(m): return m
            }
        }
    }

    private let fd: Int32
    private var buffer = Data()
    private var nextID = 0

    private init(fd: Int32) {
        self.fd = fd
    }

    deinit {
        close()
    }

    func close() {
        Darwin.close(fd)
    }

    /// Connects to 127.0.0.1:`port` (retrying while the app starts) and authenticates.
    static func connect(port: UInt16, token: String, timeout: TimeInterval) throws -> ControlClient {
        let deadline = Date().addingTimeInterval(timeout)
        var lastError = "never tried"
        while Date() < deadline {
            let fd = socket(AF_INET, SOCK_STREAM, 0)
            guard fd >= 0 else { throw Failure.connect("socket: \(errno)") }
            var addr = sockaddr_in()
            addr.sin_family = sa_family_t(AF_INET)
            addr.sin_port = port.bigEndian
            addr.sin_addr.s_addr = inet_addr("127.0.0.1")
            let ok = withUnsafePointer(to: &addr) {
                $0.withMemoryRebound(to: sockaddr.self, capacity: 1) { Darwin.connect(fd, $0, socklen_t(MemoryLayout<sockaddr_in>.size)) }
            }
            if ok == 0 {
                var tv = timeval(tv_sec: 60, tv_usec: 0)
                setsockopt(fd, SOL_SOCKET, SO_RCVTIMEO, &tv, socklen_t(MemoryLayout<timeval>.size))
                let client = ControlClient(fd: fd)
                let auth = try client.request("auth", ["token": token])
                guard auth["ok"] as? Bool == true else { throw Failure.connect("authentication refused: \(auth)") }
                return client
            }
            lastError = "connect: errno \(errno)"
            Darwin.close(fd)
            Thread.sleep(forTimeInterval: 0.25)
        }
        throw Failure.connect("the app's control server didn't answer within \(Int(timeout)) s (\(lastError))")
    }

    /// One request; the whole reply object.
    func request(_ method: String, _ params: [String: Any] = [:]) throws -> [String: Any] {
        nextID += 1
        let frame: [String: Any] = ["id": nextID, "method": method, "params": params]
        var data = try JSONSerialization.data(withJSONObject: frame)
        data.append(0x0A)
        try data.withUnsafeBytes { raw in
            var sent = 0
            while sent < raw.count {
                let n = send(fd, raw.baseAddress!.advanced(by: sent), raw.count - sent, 0)
                guard n > 0 else { throw Failure.io("send: errno \(errno)") }
                sent += n
            }
        }
        let line = try readLine()
        guard let reply = try JSONSerialization.jsonObject(with: line) as? [String: Any] else { throw Failure.reply("not a JSON object") }
        return reply
    }

    /// A method's `result`, failing on an error reply.
    @discardableResult
    func call(_ method: String, _ params: [String: Any] = [:]) throws -> [String: Any] {
        let reply = try request(method, params)
        guard reply["ok"] as? Bool == true else { throw Failure.reply("\(method): \(reply["error"] ?? "error")") }
        return reply["result"] as? [String: Any] ?? [:]
    }

    /// An engine command's result (`engine.execute`).
    @discardableResult
    func run(_ command: String, _ params: [String: Any] = [:]) throws -> [String: Any] {
        try call("engine.execute", ["command": command, "params": params])
    }

    private func readLine() throws -> Data {
        var chunk = [UInt8](repeating: 0, count: 65536)
        while true {
            if let nl = buffer.firstIndex(of: 0x0A) {
                let line = buffer[buffer.startIndex..<nl]
                buffer.removeSubrange(buffer.startIndex...nl)
                return Data(line)
            }
            let n = recv(fd, &chunk, chunk.count, 0)
            guard n > 0 else { throw Failure.io(n == 0 ? "the app closed the connection" : "recv: errno \(errno)") }
            buffer.append(contentsOf: chunk[0..<n])
        }
    }
}
