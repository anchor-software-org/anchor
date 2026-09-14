import Foundation

/// Connection status — mirrors Android's ConnectionStatus enum.
enum ConnectionStatus: String {
    case disconnected
    case connecting
    case connected
}

/// Current connection state — mirrors Android's ConnectionState.
struct ConnectionState {
    var status: ConnectionStatus = .disconnected
    var host: String = ""
    var port: Int = 0
    var error: String? = nil
}
