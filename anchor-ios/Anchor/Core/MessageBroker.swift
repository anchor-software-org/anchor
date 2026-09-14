import Foundation
import Combine

/// Central event bus — mirrors Kotlin's MessageBroker (MutableSharedFlow).
/// All plugins subscribe to the shared `events` publisher.
class MessageBroker {
    let events = PassthroughSubject<AnchorEvent, Never>()

    func send(_ event: AnchorEvent) {
        events.send(event)
    }
}
