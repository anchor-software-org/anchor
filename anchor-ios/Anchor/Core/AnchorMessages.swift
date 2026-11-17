import Foundation
import AnchorSDK

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
    case clipboard(ClipboardWireMessage)
    case media(MediaWireMessage)
    case notification(NotificationWireMessage)
    case commands(CommandWireMessage)
    case files(FileWireMessage)
}

enum ClipboardWireContent: Equatable {
    case text(String)
    case png(Data)
}

enum ClipboardWireMessage: Equatable {
    /// Outbound publications omit origin/revision; NetworkPlugin owns the
    /// local wire sequence and fills those fields exactly once.
    case publishLocal(ClipboardWireContent)
    case clearLocal
    case publishRemote(originNodeID: Data, revision: UInt64, content: ClipboardWireContent)
    case clearRemote(originNodeID: Data, revision: UInt64)
}

enum MediaWireMessage: Equatable {
    case availability(Bool)
    case state(AnchorMediaState)
    case command(AnchorMediaCommand)
}

struct DesktopNotification: Identifiable, Equatable {
    let id: String
    let applicationName: String
    let title: String
    let body: String
    let postedAt: Date
}

enum NotificationWireMessage: Equatable {
    case availability(Bool)
    case posted(DesktopNotification)
}

struct RemoteCommandDefinition: Identifiable, Equatable {
    let id: String
    let name: String
    let description: String
    let detached: Bool
}

struct RemoteCommandResult: Equatable {
    enum Status: String, Equatable { case started, done, failed }
    let commandID: String
    let executionID: String
    let status: Status
    let exitCode: Int32
    let error: String
}

enum CommandWireMessage: Equatable {
    case availability(Bool)
    case list([RemoteCommandDefinition])
    case result(RemoteCommandResult)
    case run(commandID: String)
    case kill(executionID: String)
}

enum FileTransferDirection: Equatable {
    case sent
    case received
}

enum FileTransferStatus: Equatable {
    case transferring
    case completed
    case failed(String)
}

struct FileTransferEntry: Identifiable, Equatable {
    let id: String
    let name: String
    let direction: FileTransferDirection
    let totalBytes: UInt64
    var transferredBytes: UInt64
    var status: FileTransferStatus
    var localURL: URL?
    let startedAt: Date
}

enum FileWireMessage: Equatable {
    case availability(Bool)
    case send(URL)
    case began(FileTransferEntry)
    case progress(id: String, transferredBytes: UInt64)
    case completed(id: String, localURL: URL?)
    case failed(id: String, message: String)
}

/// A routable event — mirrors Rust's AnchorEvent struct.
/// All inter-plugin communication goes through these.
struct AnchorEvent {
    let target: AnchorTarget
    let message: AnchorMessage
}
