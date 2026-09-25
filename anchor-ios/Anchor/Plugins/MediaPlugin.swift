import Foundation
import Combine
import UIKit
import MediaPlayer
import AnchorSDK

struct LocalMediaState: Equatable {
    let title: String
    let artist: String
    let album: String
    let playing: Bool
    let positionMilliseconds: UInt64
    let durationMilliseconds: UInt64
}

final class MediaPlugin: Plugin, ObservableObject {
    let pluginId = "media"

    @Published private(set) var isAvailable = false
    @Published private(set) var state: AnchorMediaState?
    @Published private(set) var artwork: UIImage?
    @Published private(set) var localAuthorization = MPMediaLibrary.authorizationStatus()
    @Published private(set) var localState: LocalMediaState?
    @Published private(set) var localArtwork: UIImage?

    private let broker: MessageBroker
    private var cancellables = Set<AnyCancellable>()
    private var stateReceivedAt = Date()
    private var artworkFingerprint: Int?
    private let localPlayer = MPMusicPlayerController.systemMusicPlayer
    private var localNotificationsStarted = false
    private var localStateReceivedAt = Date()

    init(broker: MessageBroker) {
        self.broker = broker
    }

    func start() {
        broker.events
            .receive(on: DispatchQueue.main)
            .filter { $0.target == .service("media") }
            .sink { [weak self] event in
                guard case .media(let message) = event.message else { return }
                self?.handle(message)
            }
            .store(in: &cancellables)
        configureLocalMedia()
    }

    func stop() {
        cancellables.removeAll()
        isAvailable = false
        state = nil
        artwork = nil
        localState = nil
        localArtwork = nil
        if localNotificationsStarted {
            localPlayer.endGeneratingPlaybackNotifications()
            localNotificationsStarted = false
        }
    }

    func send(_ command: AnchorMediaCommand) {
        guard isAvailable else { return }
        broker.send(AnchorEvent(target: .device, message: .media(.command(command))))
    }

    func displayedPositionMilliseconds(at date: Date = Date()) -> UInt64 {
        guard let state else { return 0 }
        let elapsed = state.playing ? max(0, date.timeIntervalSince(stateReceivedAt) * 1_000) : 0
        let advanced = state.positionMilliseconds.addingReportingOverflow(UInt64(elapsed.rounded(.down)))
        let value = advanced.overflow ? UInt64.max : advanced.partialValue
        return state.durationMilliseconds == 0 ? value : min(value, state.durationMilliseconds)
    }

    func requestLocalMediaAccess() {
        MPMediaLibrary.requestAuthorization { [weak self] status in
            DispatchQueue.main.async {
                self?.localAuthorization = status
                self?.configureLocalMedia()
            }
        }
    }

    func sendLocal(_ command: AnchorMediaCommand) {
        guard localAuthorization == .authorized else { return }
        switch command {
        case .play: localPlayer.play()
        case .pause: localPlayer.pause()
        case .next: localPlayer.skipToNextItem()
        case .previous: localPlayer.skipToPreviousItem()
        case .seek(let positionMilliseconds):
            localPlayer.currentPlaybackTime = TimeInterval(positionMilliseconds) / 1_000
        }
        DispatchQueue.main.async { [weak self] in self?.refreshLocalMedia() }
    }

    func localDisplayedPositionMilliseconds(at date: Date = Date()) -> UInt64 {
        guard let localState else { return 0 }
        let elapsed = localState.playing ? max(0, date.timeIntervalSince(localStateReceivedAt) * 1_000) : 0
        let value = localState.positionMilliseconds + UInt64(elapsed.rounded(.down))
        return localState.durationMilliseconds == 0 ? value : min(value, localState.durationMilliseconds)
    }

    private func configureLocalMedia() {
        localAuthorization = MPMediaLibrary.authorizationStatus()
        guard localAuthorization == .authorized else {
            localState = nil
            localArtwork = nil
            return
        }
        if !localNotificationsStarted {
            localPlayer.beginGeneratingPlaybackNotifications()
            localNotificationsStarted = true
            NotificationCenter.default.publisher(for: .MPMusicPlayerControllerNowPlayingItemDidChange)
                .receive(on: DispatchQueue.main)
                .sink { [weak self] _ in self?.refreshLocalMedia() }
                .store(in: &cancellables)
            NotificationCenter.default.publisher(for: .MPMusicPlayerControllerPlaybackStateDidChange)
                .receive(on: DispatchQueue.main)
                .sink { [weak self] _ in self?.refreshLocalMedia() }
                .store(in: &cancellables)
        }
        refreshLocalMedia()
    }

    private func refreshLocalMedia() {
        guard localAuthorization == .authorized,
              let item = localPlayer.nowPlayingItem else {
            localState = nil
            localArtwork = nil
            return
        }
        let duration = item.playbackDuration.isFinite ? max(0, item.playbackDuration) : 0
        let position = localPlayer.currentPlaybackTime.isFinite ? max(0, localPlayer.currentPlaybackTime) : 0
        localState = LocalMediaState(
            title: item.title ?? "",
            artist: item.artist ?? "",
            album: item.albumTitle ?? "",
            playing: localPlayer.playbackState == .playing,
            positionMilliseconds: UInt64(position * 1_000),
            durationMilliseconds: UInt64(duration * 1_000)
        )
        localStateReceivedAt = Date()
        localArtwork = item.artwork?.image(at: CGSize(width: 240, height: 240))
    }

    private func handle(_ message: MediaWireMessage) {
        switch message {
        case .availability(let available):
            isAvailable = available
            if !available { state = nil; artwork = nil }
        case .state(let newState):
            isAvailable = true
            state = newState
            stateReceivedAt = Date()
            updateArtwork(newState.artworkJPEG)
        case .command:
            break
        }
    }

    private func updateArtwork(_ data: Data) {
        var hasher = Hasher()
        hasher.combine(data)
        let fingerprint = hasher.finalize()
        guard fingerprint != artworkFingerprint else { return }
        artworkFingerprint = fingerprint
        if data.isEmpty {
            artwork = nil
            return
        }
        Task { [weak self] in
            let image = await Task.detached(priority: .utility) { UIImage(data: data) }.value
            guard self?.artworkFingerprint == fingerprint else { return }
            self?.artwork = image
        }
    }
}
