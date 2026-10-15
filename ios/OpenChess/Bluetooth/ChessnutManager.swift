import CoreBluetooth
import Foundation
import OpenChessCore

/// A Chessnut board over CoreBluetooth. The core's `ChessnutBoard` speaks
/// the protocol; this moves its bytes: notifications in through
/// `receive`, a 100 ms `poll`, and `takeWrites` out without response.
@MainActor
final class ChessnutManager: NSObject, ObservableObject {
    enum State: Equatable {
        case off
        case unauthorized
        case idle
        case scanning
        case connecting(String)
        case connected(String)
        case failed(String)
    }

    @Published private(set) var state: State = .idle
    @Published private(set) var found: [CBPeripheral] = []
    @Published private(set) var battery: (percent: UInt8, charging: Bool)?
    @Published private(set) var lastEvent: String = ""

    /// Board events go here (the game on screen sets it).
    var onEvent: ((BoardEvent) -> Void)?

    private var central: CBCentralManager?
    private var peripheral: CBPeripheral?
    private var readChar: CBCharacteristic?
    private var writeChar: CBCharacteristic?
    private var timer: Timer?
    private var board: ChessnutBoard?
    private var wantScan = false

    private let readUUID = CBUUID(string: chessnutReadCharacteristic())
    private let writeUUID = CBUUID(string: chessnutWriteCharacteristic())

    var isConnected: Bool {
        if case .connected = state { return true }
        return false
    }

    var stateText: String {
        switch state {
        case .off: return "Bluetooth is off"
        case .unauthorized: return "Bluetooth not allowed"
        case .idle: return "Not connected"
        case .scanning: return "Looking for boards…"
        case .connecting(let n): return "Connecting to \(n)…"
        case .connected(let n): return "Connected · \(n)"
        case .failed(let m): return m
        }
    }

    // MARK: Following a game

    /// Follow a game: its starting FEN (nil for the start), its moves, and
    /// whether Black sits at the near side.
    func follow(fen: String?, moves: [String], rotated: Bool) {
        do {
            board = try ChessnutBoard(fen: fen, moves: moves.joined(separator: " "), rotated: rotated)
            if isConnected {
                board?.start()
                flushWrites()
            }
        } catch {
            state = .failed("The board couldn't follow this game: \(describe(error))")
        }
    }

    func stopFollowing() {
        board?.lightsOff()
        flushWrites()
        board = nil
        onEvent = nil
    }

    /// The other side's move: lights its squares until it's made on the board.
    func opponentMove(_ uci: String) {
        guard let board else { return }
        do {
            _ = try board.opponentMove(uci: uci)
        } catch {
            lastEvent = "Board: \(describe(error))"
        }
        flushWrites()
    }

    func light(_ squares: [String]) {
        board?.light(squares: squares)
        flushWrites()
    }

    func lightsOff() {
        board?.lightsOff()
        flushWrites()
    }

    /// What the board shows now (for labelling frames, and for spotting a
    /// position the game can't reach).
    func boardNow() -> BoardNow? {
        board?.boardNow()
    }

    // MARK: Scanning and connecting

    func startScan() {
        wantScan = true
        found = []
        if central == nil {
            central = CBCentralManager(delegate: self, queue: nil)
        } else {
            beginScanIfReady()
        }
    }

    func stopScan() {
        wantScan = false
        central?.stopScan()
        if state == .scanning { state = .idle }
    }

    private func beginScanIfReady() {
        guard let central, wantScan else { return }
        switch central.state {
        case .poweredOn:
            state = .scanning
            central.scanForPeripherals(withServices: nil, options: [CBCentralManagerScanOptionAllowDuplicatesKey: false])
        case .unauthorized:
            state = .unauthorized
        case .poweredOff:
            state = .off
        default:
            break
        }
    }

    func connect(_ p: CBPeripheral) {
        guard let central else { return }
        central.stopScan()
        wantScan = false
        peripheral = p
        p.delegate = self
        state = .connecting(p.name ?? "Chessnut")
        central.connect(p, options: nil)
    }

    func disconnect() {
        timer?.invalidate()
        timer = nil
        if let p = peripheral {
            central?.cancelPeripheralConnection(p)
        }
        peripheral = nil
        readChar = nil
        writeChar = nil
        state = .idle
    }

    // MARK: Bytes

    private func startPolling() {
        timer?.invalidate()
        let t = Timer(timeInterval: 0.1, repeats: true) { [weak self] _ in
            Task { @MainActor in self?.tick() }
        }
        RunLoop.main.add(t, forMode: .common)
        timer = t
    }

    private func tick() {
        guard let board else { return }
        handle(board.poll())
        flushWrites()
    }

    private func handle(_ events: [BoardEvent]) {
        for e in events {
            switch e {
            case .move(let uci, let san):
                lastEvent = "Board: \(san)"
                _ = uci
            case .takeback(let plies):
                lastEvent = "Board: took back \(plies)"
            case .newGame:
                lastEvent = "Board: new game set up"
            case .caught:
                lastEvent = "Board caught up"
            case .waiting:
                break
            case .battery(let percent, let charging):
                battery = (percent: percent, charging: charging)
            }
            onEvent?(e)
        }
    }

    private func flushWrites() {
        guard let board else { return }
        let writes = board.takeWrites()
        guard let p = peripheral, let w = writeChar else { return }
        for bytes in writes {
            p.writeValue(bytes, for: w, type: .withoutResponse)
        }
    }

    fileprivate func received(_ data: Data) {
        guard let board else { return }
        handle(board.receive(bytes: data))
        flushWrites()
    }

    fileprivate func characteristicsReady() {
        guard readChar != nil, writeChar != nil else { return }
        state = .connected(peripheral?.name ?? "Chessnut")
        board?.start()
        flushWrites()
        startPolling()
    }
}

// MARK: - CoreBluetooth delegates (main queue, as the central was created with queue nil)

extension ChessnutManager: CBCentralManagerDelegate {
    nonisolated func centralManagerDidUpdateState(_ central: CBCentralManager) {
        MainActor.assumeIsolated {
            switch central.state {
            case .poweredOn:
                if case .off = self.state { self.state = .idle }
                self.beginScanIfReady()
            case .poweredOff:
                self.state = .off
            case .unauthorized:
                self.state = .unauthorized
            default:
                break
            }
        }
    }

    nonisolated func centralManager(
        _ central: CBCentralManager,
        didDiscover peripheral: CBPeripheral,
        advertisementData: [String: Any],
        rssi RSSI: NSNumber
    ) {
        let advName = advertisementData[CBAdvertisementDataLocalNameKey] as? String
        let name = peripheral.name ?? advName ?? ""
        guard name.localizedCaseInsensitiveContains("chessnut") else { return }
        MainActor.assumeIsolated {
            if !self.found.contains(where: { $0.identifier == peripheral.identifier }) {
                self.found.append(peripheral)
            }
        }
    }

    nonisolated func centralManager(_ central: CBCentralManager, didConnect peripheral: CBPeripheral) {
        MainActor.assumeIsolated {
            peripheral.discoverServices(nil)
        }
    }

    nonisolated func centralManager(_ central: CBCentralManager, didFailToConnect peripheral: CBPeripheral, error: Error?) {
        MainActor.assumeIsolated {
            self.state = .failed("Couldn't connect: \(error?.localizedDescription ?? "unknown error")")
        }
    }

    nonisolated func centralManager(_ central: CBCentralManager, didDisconnectPeripheral peripheral: CBPeripheral, error: Error?) {
        MainActor.assumeIsolated {
            self.timer?.invalidate()
            self.timer = nil
            self.readChar = nil
            self.writeChar = nil
            self.peripheral = nil
            if let error {
                self.state = .failed("Disconnected: \(error.localizedDescription)")
            } else {
                self.state = .idle
            }
        }
    }
}

extension ChessnutManager: CBPeripheralDelegate {
    nonisolated func peripheral(_ peripheral: CBPeripheral, didDiscoverServices error: Error?) {
        MainActor.assumeIsolated {
            for s in peripheral.services ?? [] {
                peripheral.discoverCharacteristics([self.readUUID, self.writeUUID], for: s)
            }
        }
    }

    nonisolated func peripheral(_ peripheral: CBPeripheral, didDiscoverCharacteristicsFor service: CBService, error: Error?) {
        MainActor.assumeIsolated {
            for c in service.characteristics ?? [] {
                if c.uuid == self.readUUID {
                    self.readChar = c
                    peripheral.setNotifyValue(true, for: c)
                } else if c.uuid == self.writeUUID {
                    self.writeChar = c
                }
            }
            self.characteristicsReady()
        }
    }

    nonisolated func peripheral(_ peripheral: CBPeripheral, didUpdateValueFor characteristic: CBCharacteristic, error: Error?) {
        guard error == nil, let data = characteristic.value else { return }
        MainActor.assumeIsolated {
            if characteristic.uuid == self.readUUID {
                self.received(data)
            }
        }
    }
}
