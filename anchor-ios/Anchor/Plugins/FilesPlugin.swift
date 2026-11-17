import Combine
import CryptoKit
import Foundation
import AnchorSDK

final class FilesPlugin: Plugin, ObservableObject {
    let pluginId = "files"

    @Published private(set) var isAvailable = false
    @Published private(set) var transfers: [FileTransferEntry] = []

    private let broker: MessageBroker
    private var cancellables = Set<AnyCancellable>()

    init(broker: MessageBroker) {
        self.broker = broker
    }

    func start() {
        broker.events
            .receive(on: DispatchQueue.main)
            .filter { $0.target == .service("files") }
            .sink { [weak self] event in
                guard case .files(let message) = event.message else { return }
                self?.handle(message)
            }
            .store(in: &cancellables)
    }

    func stop() {
        cancellables.removeAll()
        isAvailable = false
    }

    func send(_ url: URL) {
        guard isAvailable else { return }
        broker.send(AnchorEvent(target: .device, message: .files(.send(url))))
    }

    func clearCompleted() {
        transfers.removeAll { entry in
            if case .transferring = entry.status { return false }
            return true
        }
    }

    private func handle(_ message: FileWireMessage) {
        switch message {
        case .availability(let available):
            isAvailable = available
        case .began(let entry):
            transfers.removeAll { $0.id == entry.id }
            transfers.insert(entry, at: 0)
            if transfers.count > 50 { transfers.removeLast(transfers.count - 50) }
        case .progress(let id, let transferredBytes):
            guard let index = transfers.firstIndex(where: { $0.id == id }) else { return }
            transfers[index].transferredBytes = min(transferredBytes, transfers[index].totalBytes)
        case .completed(let id, let localURL):
            guard let index = transfers.firstIndex(where: { $0.id == id }) else { return }
            transfers[index].transferredBytes = transfers[index].totalBytes
            transfers[index].status = .completed
            transfers[index].localURL = localURL
        case .failed(let id, let message):
            guard let index = transfers.firstIndex(where: { $0.id == id }) else { return }
            transfers[index].status = .failed(message)
        case .send:
            break
        }
    }
}

enum FileTransferError: LocalizedError, Equatable {
    case fileTooLarge
    case emptyFile
    case unexpectedLength
    case checksumMismatch
    case rejected
    case timedOut

    var errorDescription: String? {
        switch self {
        case .fileTooLarge: return "File exceeds the 256 MB transfer limit."
        case .emptyFile: return "Empty files are not supported."
        case .unexpectedLength: return "The received file length did not match its offer."
        case .checksumMismatch: return "The received file failed its integrity check."
        case .rejected: return "The desktop rejected this file."
        case .timedOut: return "The transfer timed out."
        }
    }
}

final class FileReceiveAssembler {
    let temporaryURL: URL
    let expectedBytes: UInt64
    private let expectedHash: Data
    private let handle: FileHandle
    private var hasher = SHA256()
    private(set) var receivedBytes: UInt64 = 0
    private var closed = false

    init(temporaryURL: URL, expectedBytes: UInt64, expectedHash: Data) throws {
        self.temporaryURL = temporaryURL
        self.expectedBytes = expectedBytes
        self.expectedHash = expectedHash
        FileManager.default.createFile(atPath: temporaryURL.path, contents: nil)
        handle = try FileHandle(forWritingTo: temporaryURL)
    }

    deinit { try? handle.close() }

    func append(_ data: Data) throws {
        guard !closed,
              receivedBytes <= expectedBytes,
              UInt64(data.count) <= expectedBytes - receivedBytes else {
            throw FileTransferError.unexpectedLength
        }
        try handle.write(contentsOf: data)
        hasher.update(data: data)
        receivedBytes += UInt64(data.count)
    }

    func finish() throws {
        guard !closed else { return }
        try handle.close()
        closed = true
        guard receivedBytes == expectedBytes else { throw FileTransferError.unexpectedLength }
        guard Data(hasher.finalize()) == expectedHash else { throw FileTransferError.checksumMismatch }
    }
}

actor FileTransferCoordinator {
    static let maximumFileBytes: UInt64 = 256 * 1024 * 1024

    private struct Incoming {
        let offer: AnchorFileOffer
        var streamID: UInt64?
        var started = false
        var temporaryURL: URL?
        var contentReady = false
        var completionHash: Data?
    }

    private var incoming = [Data: Incoming]()
    private var streams = [UInt64: any AnchorReliableStream]()
    private var transferForStream = [UInt64: Data]()
    private var decisions = [Data: Bool]()
    private var openedStreams = [UInt64: UInt64]()
    private let emit: (FileWireMessage) -> Void

    init(emit: @escaping (FileWireMessage) -> Void) {
        self.emit = emit
    }

    func reset() {
        for state in incoming.values {
            if let url = state.temporaryURL { try? FileManager.default.removeItem(at: url) }
        }
        incoming.removeAll()
        streams.removeAll()
        transferForStream.removeAll()
        decisions.removeAll()
        openedStreams.removeAll()
    }

    func accept(_ offer: AnchorFileOffer) -> Bool {
        guard offer.byteLength > 0,
              offer.byteLength <= Self.maximumFileBytes,
              incoming[offer.transferID] == nil else { return false }
        incoming[offer.transferID] = Incoming(offer: offer)
        emit(.began(FileTransferEntry(
            id: Self.identifier(offer.transferID),
            name: offer.filename,
            direction: .received,
            totalBytes: offer.byteLength,
            transferredBytes: 0,
            status: .transferring,
            localURL: nil,
            startedAt: Date()
        )))
        return true
    }

    func register(_ stream: any AnchorReliableStream) {
        streams[stream.streamIdentifier] = stream
        startIfReady(streamID: stream.streamIdentifier)
    }

    func bind(_ start: AnchorFileContentStart) {
        guard var state = incoming[start.transferID], !state.started else { return }
        state.streamID = start.quicStreamID
        incoming[start.transferID] = state
        transferForStream[start.quicStreamID] = start.transferID
        startIfReady(streamID: start.quicStreamID)
    }

    func complete(_ completion: AnchorFileComplete) {
        guard var state = incoming[completion.transferID],
              completion.sha256 == state.offer.sha256 else {
            return
        }
        state.completionHash = completion.sha256
        incoming[completion.transferID] = state
        finalizeIfReady(transferID: completion.transferID)
    }

    func recordDecision(_ decision: AnchorFileDecision) {
        decisions[decision.transferID] = decision.accepted
    }

    func waitForDecision(transferID: Data, timeoutNanoseconds: UInt64 = 5_000_000_000) async throws {
        let deadline = DispatchTime.now().uptimeNanoseconds &+ timeoutNanoseconds
        while DispatchTime.now().uptimeNanoseconds < deadline {
            if let accepted = decisions.removeValue(forKey: transferID) {
                if accepted { return }
                throw FileTransferError.rejected
            }
            try await Task.sleep(nanoseconds: 20_000_000)
        }
        throw FileTransferError.timedOut
    }

    func recordOpened(responseTo: UInt64, streamID: UInt64) {
        openedStreams[responseTo] = streamID
    }

    func waitForOpened(requestID: UInt64, streamID: UInt64,
                       timeoutNanoseconds: UInt64 = 5_000_000_000) async throws {
        let deadline = DispatchTime.now().uptimeNanoseconds &+ timeoutNanoseconds
        while DispatchTime.now().uptimeNanoseconds < deadline {
            if let opened = openedStreams.removeValue(forKey: requestID) {
                guard opened == streamID else { throw AnchorFeatureCodecError.streamBindingMismatch }
                return
            }
            try await Task.sleep(nanoseconds: 20_000_000)
        }
        throw FileTransferError.timedOut
    }

    private func startIfReady(streamID: UInt64) {
        guard let transferID = transferForStream[streamID],
              let stream = streams.removeValue(forKey: streamID),
              var state = incoming[transferID],
              !state.started else { return }
        state.started = true
        incoming[transferID] = state
        Task { await receive(transferID: transferID, offer: state.offer, stream: stream) }
    }

    private func receive(transferID: Data, offer: AnchorFileOffer,
                         stream: any AnchorReliableStream) async {
        let id = Self.identifier(transferID)
        var tempURL: URL?
        do {
            let directory = try Self.receivedDirectory()
            let temporary = directory.appendingPathComponent(".anchor-incoming-\(id).part")
            try? FileManager.default.removeItem(at: temporary)
            let assembler = try FileReceiveAssembler(
                temporaryURL: temporary,
                expectedBytes: offer.byteLength,
                expectedHash: offer.sha256
            )
            tempURL = temporary
            if var state = incoming[transferID] {
                state.temporaryURL = temporary
                incoming[transferID] = state
            }
            var lastReported: UInt64 = 0
            while assembler.receivedBytes < offer.byteLength {
                let remaining = offer.byteLength - assembler.receivedBytes
                let chunk = try await stream.receive(maximumLength: Int(min(remaining, 256 * 1024)))
                guard !chunk.isEmpty else { throw FileTransferError.unexpectedLength }
                try assembler.append(chunk)
                if assembler.receivedBytes - lastReported >= 256 * 1024 || assembler.receivedBytes == offer.byteLength {
                    lastReported = assembler.receivedBytes
                    emit(.progress(id: id, transferredBytes: assembler.receivedBytes))
                }
            }
            try assembler.finish()
            guard var state = incoming[transferID] else { return }
            state.contentReady = true
            incoming[transferID] = state
            finalizeIfReady(transferID: transferID)
        } catch {
            if let tempURL { try? FileManager.default.removeItem(at: tempURL) }
            incoming.removeValue(forKey: transferID)
            emit(.failed(id: id, message: error.localizedDescription))
        }
    }

    private func finalizeIfReady(transferID: Data) {
        guard let state = incoming[transferID], state.contentReady,
              state.completionHash == state.offer.sha256,
              let temporaryURL = state.temporaryURL else { return }
        do {
            let finalURL = try Self.uniqueDestination(for: state.offer.filename)
            try FileManager.default.moveItem(at: temporaryURL, to: finalURL)
            incoming.removeValue(forKey: transferID)
            if let streamID = state.streamID { transferForStream.removeValue(forKey: streamID) }
            emit(.completed(id: Self.identifier(transferID), localURL: finalURL))
        } catch {
            try? FileManager.default.removeItem(at: temporaryURL)
            incoming.removeValue(forKey: transferID)
            emit(.failed(id: Self.identifier(transferID), message: error.localizedDescription))
        }
    }

    static func identifier(_ data: Data) -> String {
        data.map { String(format: "%02x", $0) }.joined()
    }

    static func receivedDirectory() throws -> URL {
        let documents = try FileManager.default.url(
            for: .documentDirectory, in: .userDomainMask, appropriateFor: nil, create: true
        )
        let directory = documents.appendingPathComponent("Anchor Received", isDirectory: true)
        try FileManager.default.createDirectory(at: directory, withIntermediateDirectories: true)
        return directory
    }

    static func uniqueDestination(for filename: String) throws -> URL {
        let directory = try receivedDirectory()
        let source = URL(fileURLWithPath: filename)
        let base = source.deletingPathExtension().lastPathComponent
        let ext = source.pathExtension
        var candidate = directory.appendingPathComponent(filename)
        var suffix = 2
        while FileManager.default.fileExists(atPath: candidate.path) {
            let name = ext.isEmpty ? "\(base) \(suffix)" : "\(base) \(suffix).\(ext)"
            candidate = directory.appendingPathComponent(name)
            suffix += 1
        }
        return candidate
    }
}
