import Foundation

/// Plugin protocol — mirrors Kotlin's Plugin interface.
/// Each plugin has an ID for routing and lifecycle methods.
protocol Plugin: AnyObject {
    var pluginId: String { get }
    func start()
    func stop()
}
