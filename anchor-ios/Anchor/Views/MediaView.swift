import SwiftUI
import MediaPlayer
import AnchorSDK

struct MediaView: View {
    @ObservedObject var mediaPlugin: MediaPlugin
    @State private var scrubPosition: Double?
    @State private var localScrubPosition: Double?

    var body: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 24) {
                section(title: "Desktop") { desktopCard }
                section(title: "This iPad · Apple Music") { localCard }
            }
            .frame(maxWidth: 720)
            .padding(20)
            .frame(maxWidth: .infinity)
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity)
        .background(Color.charcoalBlack)
    }

    private func section<Content: View>(title: String, @ViewBuilder content: () -> Content) -> some View {
        VStack(alignment: .leading, spacing: 9) {
            Text(title)
                .font(.system(size: 12, weight: .semibold))
                .foregroundStyle(Color.anchorGray)
            content()
        }
    }

    @ViewBuilder private var desktopCard: some View {
        if !mediaPlugin.isAvailable {
            unavailable("This desktop did not advertise media controls.")
        } else if let state = mediaPlugin.state {
            playbackCard(
                title: state.title,
                artist: state.artist,
                album: state.album,
                playing: state.playing,
                duration: state.durationMilliseconds,
                artwork: mediaPlugin.artwork,
                currentPosition: { mediaPlugin.displayedPositionMilliseconds(at: $0) },
                scrubPosition: $scrubPosition,
                send: mediaPlugin.send
            )
        } else {
            unavailable("Start playback on the desktop to see it here.")
        }
    }

    @ViewBuilder private var localCard: some View {
        switch mediaPlugin.localAuthorization {
        case .notDetermined:
            permissionCard
        case .denied, .restricted:
            unavailable("Apple Music access is disabled in Settings.")
        case .authorized:
            if let state = mediaPlugin.localState {
                playbackCard(
                    title: state.title,
                    artist: state.artist,
                    album: state.album,
                    playing: state.playing,
                    duration: state.durationMilliseconds,
                    artwork: mediaPlugin.localArtwork,
                    currentPosition: { mediaPlugin.localDisplayedPositionMilliseconds(at: $0) },
                    scrubPosition: $localScrubPosition,
                    send: mediaPlugin.sendLocal
                )
            } else {
                unavailable("Nothing is playing in Apple Music on this iPad.")
            }
        @unknown default:
            unavailable("Apple Music playback is unavailable.")
        }
    }

    private var permissionCard: some View {
        HStack(spacing: 16) {
            VStack(alignment: .leading, spacing: 4) {
                Text("Show local playback")
                    .font(.system(size: 15, weight: .semibold))
                    .foregroundStyle(Color.offWhite)
                Text("Allow access to playback from Apple Music on this iPad.")
                    .font(.system(size: 12))
                    .foregroundStyle(Color.anchorGray)
            }
            Spacer()
            Button("Allow") { mediaPlugin.requestLocalMediaAccess() }
                .buttonStyle(.bordered)
                .tint(.offWhite)
        }
        .padding(18)
        .background(Color.darkGray.opacity(0.45))
    }

    private func unavailable(_ message: String) -> some View {
        HStack(spacing: 14) {
            Image("MaterialMusicNote")
                .renderingMode(.template)
                .resizable()
                .scaledToFit()
                .frame(width: 30, height: 30)
                .foregroundStyle(Color.anchorGray.opacity(0.6))
            Text(message)
                .font(.system(size: 14))
                .foregroundStyle(Color.anchorGray)
            Spacer()
        }
        .padding(22)
        .background(Color.darkGray.opacity(0.35))
    }

    private func artwork(_ image: UIImage?) -> some View {
        Group {
            if let image {
                Image(uiImage: image).resizable().scaledToFill()
            } else {
                ZStack {
                    Color.white.opacity(0.06)
                    Image("MaterialMusicNote")
                        .renderingMode(.template)
                        .resizable()
                        .scaledToFit()
                        .frame(width: 34, height: 34)
                        .foregroundStyle(.secondary)
                }
            }
        }
        .frame(width: 88, height: 88)
        .clipped()
        .accessibilityHidden(true)
    }

    private func playbackCard(
        title: String,
        artist: String,
        album: String,
        playing: Bool,
        duration: UInt64,
        artwork image: UIImage?,
        currentPosition: @escaping (Date) -> UInt64,
        scrubPosition: Binding<Double?>,
        send: @escaping (AnchorMediaCommand) -> Void
    ) -> some View {
        VStack(alignment: .leading, spacing: 14) {
            HStack(alignment: .center, spacing: 14) {
                artwork(image)
                VStack(alignment: .leading, spacing: 4) {
                    Text(title.isEmpty ? "Unknown title" : title)
                        .font(.system(size: 17, weight: .semibold)).lineLimit(2)
                    Text([artist, album].filter { !$0.isEmpty }.joined(separator: " · "))
                        .font(.system(size: 12)).foregroundStyle(.secondary).lineLimit(2)
                }
                Spacer(minLength: 0)
            }

            VStack(alignment: .leading, spacing: 5) {
                TimelineView(.periodic(from: .now, by: 0.5)) { context in
                let current = scrubPosition.wrappedValue ?? Double(currentPosition(context.date))
                VStack(spacing: 6) {
                    Slider(
                        value: Binding(
                            get: { current },
                            set: { scrubPosition.wrappedValue = $0 }
                        ),
                        in: 0...Double(max(1, duration)),
                        onEditingChanged: { editing in
                            if !editing, let value = scrubPosition.wrappedValue {
                                send(.seek(positionMilliseconds: UInt64(value)))
                                scrubPosition.wrappedValue = nil
                            }
                        }
                    )
                    .tint(.offWhite)
                    .disabled(duration == 0)
                    .accessibilityLabel("Playback position")
                    HStack {
                        Text(formatTime(UInt64(current)))
                        Spacer()
                        Text(formatTime(duration))
                    }
                    .font(.caption.monospacedDigit()).foregroundStyle(.secondary)
                }
            }
            }

            HStack(spacing: 14) {
                mediaButton("backward.end.fill", label: "Previous") { send(.previous) }
                mediaButton(playing ? "pause.fill" : "play.fill", label: playing ? "Pause" : "Play") {
                    send(playing ? .pause : .play)
                }
                mediaButton("forward.end.fill", label: "Next") { send(.next) }
            }
            .frame(maxWidth: .infinity, alignment: .center)
        }
        .padding(16)
        .background(Color.darkGray.opacity(0.5))
    }

    private func mediaButton(_ icon: String, label: String, action: @escaping () -> Void) -> some View {
        Button(action: action) {
            Image(systemName: icon)
                .font(.system(size: 20, weight: .semibold))
                .frame(width: 48, height: 48)
                .background(Color.white.opacity(0.08))
                .foregroundStyle(Color.offWhite)
                .clipShape(Circle())
        }
        .accessibilityLabel(label)
    }

    private func formatTime(_ milliseconds: UInt64) -> String {
        let seconds = milliseconds / 1_000
        return String(format: "%d:%02d", seconds / 60, seconds % 60)
    }
}
