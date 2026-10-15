import Foundation
import Network

/// The network path as `NWPathMonitor` sees it: Low Data Mode
/// (`isConstrained`), cellular or a hotspot (`isExpensive`), and Wi-Fi.
final class NetworkMonitor: ObservableObject {
    @Published private(set) var isOnline = true
    @Published private(set) var isConstrained = false
    @Published private(set) var isExpensive = false
    @Published private(set) var onWifi = false

    private let monitor = NWPathMonitor()
    private let queue = DispatchQueue(label: "org.offenedatenmodellierung.openchess.path")

    init() {
        monitor.pathUpdateHandler = { [weak self] path in
            let online = path.status == .satisfied
            let constrained = path.isConstrained
            let expensive = path.isExpensive
            let wifi = path.usesInterfaceType(.wifi) || path.usesInterfaceType(.wiredEthernet)
            DispatchQueue.main.async {
                guard let self else { return }
                self.isOnline = online
                self.isConstrained = constrained
                self.isExpensive = expensive
                self.onWifi = wifi
            }
        }
        monitor.start(queue: queue)
    }

    deinit {
        monitor.cancel()
    }

    var summary: String {
        if !isOnline { return "Offline" }
        if isConstrained { return "Low Data Mode is on for this network" }
        if isExpensive { return "Cellular or hotspot" }
        return onWifi ? "Wi-Fi" : "Online"
    }
}
