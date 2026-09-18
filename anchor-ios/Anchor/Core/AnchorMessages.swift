import Foundation

/// Routing targets — mirrors Rust's AnchorTarget enum.
/// Determines which plugin(s) receive the event.
enum AnchorTarget: Equatable {
    case gui
    case network
    case device          // Forward to connected device via network plugin
    case service(String) // Route to a specific plugin by ID
    case broadcast
}

/// Message payload — mirrors Rust's AnchorMessage enum.
/// Json is the primary format for structured data between plugins.
enum AnchorMessage {
    case generic(String)
    case json(String)
    case binary(Data)
}

/// A routable event — mirrors Rust's AnchorEvent struct.
/// All inter-plugin communication goes through these.
struct AnchorEvent {
    let target: AnchorTarget
    let message: AnchorMessage
}
