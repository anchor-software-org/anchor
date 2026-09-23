import SwiftUI
import AVFoundation

/// UIViewRepresentable wrapper for AVSampleBufferDisplayLayer.
/// Reconnects the plugin's display layer on every update so it survives
/// SwiftUI view recreation (screen switches, drawer open/close).
struct VideoDisplayView: UIViewRepresentable {
    let videoPlugin: VideoPlugin

    func makeUIView(context: Context) -> VideoLayerView {
        let view = VideoLayerView()
        videoPlugin.displayLayer = view.displayLayer
        videoPlugin.requestKeyframe()
        return view
    }

    func updateUIView(_ uiView: VideoLayerView, context: Context) {
        // Reconnect if the plugin's layer got swapped or cleared.
        if videoPlugin.displayLayer !== uiView.displayLayer {
            videoPlugin.displayLayer = uiView.displayLayer
            videoPlugin.requestKeyframe()
        }
    }
}

class VideoLayerView: UIView {
    let displayLayer = AVSampleBufferDisplayLayer()

    override init(frame: CGRect) {
        super.init(frame: frame)
        displayLayer.videoGravity = .resizeAspect
        displayLayer.backgroundColor = UIColor.black.cgColor
        layer.addSublayer(displayLayer)
    }

    required init?(coder: NSCoder) {
        fatalError("init(coder:) not implemented")
    }

    override func layoutSubviews() {
        super.layoutSubviews()
        displayLayer.frame = bounds
    }
}
