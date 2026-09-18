//! Application settings — loaded from `~/.config/anchor/settings.json`.
//!
//! On first run, writes a default config + a `settings.defaults.md` documenting
//! every field, valid values, and tuning advice.

use serde::{Deserialize, Serialize};
use std::fs;
use std::path::PathBuf;
use std::sync::OnceLock;

static SETTINGS: OnceLock<AppSettings> = OnceLock::new();

/// Initialize global settings. Call once at startup.
pub fn init_settings() {
    let s = load_settings();
    log::debug!(
        "Settings: bitrate={}bps fps={} gop={} max_b={} rc={} low_power={} preset={} tune={} hw_pool={} async_depth={} profile={} level={} scale_mode={} coder={} qp={} maxrate={} bufsize={} pacing={} frame_buf={} stats_interval={}",
        s.encoding.bitrate_bps,
        s.encoding.fps,
        s.encoding.gop_size,
        s.encoding.max_b_frames,
        s.encoding.rate_control,
        s.encoding.low_power,
        s.encoding.x264_preset,
        s.encoding.x264_tune,
        s.encoding.hw_pool_size,
        s.encoding.async_depth,
        s.encoding.profile,
        s.encoding.level,
        s.encoding.scale_mode,
        s.encoding.coder,
        s.encoding.qp,
        s.encoding.maxrate_bps,
        s.encoding.bufsize_bits,
        s.encoding.pacing_utilization_percent,
        s.network.frame_buffer_size,
        s.capture.stats_log_interval_frames
    );
    SETTINGS.set(s).expect("Settings already initialized");
}

/// Get a reference to the global settings. Panics if not initialized.
pub fn settings() -> &'static AppSettings {
    SETTINGS.get().expect("Settings not initialized — call init_settings() first")
}

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(default)]
#[derive(Default)]
pub struct AppSettings {
    pub capture: CaptureSettings,
    pub encoding: EncodingSettings,
    pub network: NetworkSettings,
    pub input: InputSettings,
    pub clipboard: ClipboardSettings,
    pub camera: CameraSettings,
    pub file_transfer: FileTransferSettings,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(default)]
pub struct CameraSettings {
    /// Automatically `pkexec modprobe v4l2loopback` at desktop startup.
    /// When false, the user must press "Create camera" in the Settings tab
    /// before the phone-camera-as-webcam path is available.
    pub auto_load: bool,

    /// Default streaming framerate pushed to the phone on connect.
    /// Range: 15–60. The Android Camera screen can override per-session.
    pub fps: u32,

    /// Default streaming bitrate in kilobits per second, pushed to the phone
    /// on connect. Range: 500–8000. The Android Camera screen can override
    /// per-session.
    pub bitrate_kbps: u32,
}

impl Default for CameraSettings {
    fn default() -> Self {
        Self { auto_load: false, fps: 30, bitrate_kbps: 2000 }
    }
}

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(default)]
pub struct CaptureSettings {
    /// "wayland_screencopy"
    pub backend: String,
    /// Index of the output to capture on startup (null = first available)
    pub default_output: Option<usize>,
    /// How often to check for display add/remove/resize (seconds)
    pub output_refresh_interval_secs: u64,
    /// How often to log encode timing stats (in frames). Lower = more data points.
    /// Range: 1 (every frame, very noisy) to 600 (every 10s at 60fps)
    pub stats_log_interval_frames: u64,
}

impl Default for CaptureSettings {
    fn default() -> Self {
        Self {
            backend: "wayland_screencopy".into(),
            default_output: None,
            output_refresh_interval_secs: 2,
            stats_log_interval_frames: 60,
        }
    }
}

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(default)]
pub struct EncodingSettings {
    /// Bitrate in bits per second. Higher = better quality, more bandwidth.
    /// Range: 1_000_000 (1 Mbps, very low) to 50_000_000 (50 Mbps, near-lossless)
    pub bitrate_bps: usize,

    /// Target framerate. Desktop capture will try to match this.
    /// Common values: 30, 60
    pub fps: u32,

    /// Group of Pictures size — frames between keyframes.
    /// Lower = faster recovery from artifacts, higher = better compression.
    /// Range: 1 (all keyframes, no compression) to 300 (10s at 30fps)
    /// Recommended: 15-30 for streaming, 1 for lowest latency
    pub gop_size: u32,

    /// Maximum B-frames. 0 = disabled (lowest latency). Higher = better compression.
    /// Range: 0-4. Recommended: 0 for real-time streaming.
    pub max_b_frames: i32,

    /// Rate control mode for VAAPI encoder.
    /// "CBR" = Constant Bitrate (stable bandwidth, predictable)
    /// "CQP" = Constant Quantization Parameter (consistent quality, variable bandwidth)
    /// "VBR" = Variable Bitrate (best quality per bit, unpredictable bandwidth)
    pub rate_control: String,

    /// Use low-power encoder hardware path (Intel EncSliceLP).
    /// true = lower latency, handles non-standard resolutions better
    /// false = full-feature encoder, may have issues with some resolutions
    pub low_power: bool,

    /// x264 software encoder preset (used when VAAPI unavailable).
    /// Fastest to slowest: "ultrafast", "superfast", "veryfast", "faster",
    /// "fast", "medium", "slow", "slower", "veryslow", "placebo"
    /// Faster = less CPU, worse compression. "ultrafast" recommended for streaming.
    pub x264_preset: String,

    /// x264 software encoder tuning.
    /// "zerolatency" = no encoder buffering (best for streaming)
    /// "film" = optimized for live-action content
    /// "animation" = optimized for cartoons/UI
    /// "grain" = preserves film grain
    /// "stillimage" = optimized for mostly-static content (desktop use)
    pub x264_tune: String,

    /// VAAPI hardware frame pool size. Higher = more buffering capacity.
    /// Range: 4-64. Default 20 is safe for most GPUs.
    pub hw_pool_size: u32,

    /// VAAPI encoder async depth — how many frames the encoder can buffer
    /// internally before draining. Higher = better GPU pipeline utilization.
    /// 0 = default (encoder decides), 1 = synchronous, 2-4 = pipelined.
    /// Range: 0-8. Recommended: 2-4 for streaming.
    pub async_depth: u32,

    /// H.264 profile. Controls feature set and decoder compatibility.
    /// "auto" = let encoder decide, "constrained_baseline" = widest compat,
    /// "main" = B-frames support, "high" = best compression (most decoders support it)
    pub profile: String,

    /// H.264 level. Controls max resolution/framerate/bitrate allowed.
    /// "auto" = let encoder decide. "4.1" = safe for 1080p60 on all iOS devices.
    /// "5.1" = 4K support.
    pub level: String,

    /// VAAPI scale filter mode. Controls GPU scaling algorithm quality.
    /// "hq" = high quality (default), "fast" = lower GPU latency, "default" = driver decides
    pub scale_mode: String,

    /// Entropy coder. "cabac" = better compression (default), "cavlc" = faster decode.
    /// cavlc can reduce iOS decode time at the cost of ~10-15% larger bitstream.
    pub coder: String,

    /// Quantization parameter for CQP/ICQ modes. Lower = better quality.
    /// Range: 0-52. 0 = auto/unused. 20-28 typical for desktop streaming.
    pub qp: u32,

    /// Max bitrate cap (bits/sec) for VBR/QVBR modes. 0 = no cap.
    /// Helps smooth bandwidth spikes during scene changes.
    pub maxrate_bps: usize,

    /// VBV buffer size in bits. Controls how much bitrate can burst.
    /// 0 = auto. Typically 1-2x the bitrate for streaming.
    pub bufsize_bits: usize,

    /// Hard per-frame size cap in bytes (VAAPI `max_frame_size`). 0 = the
    /// automatic 120 KB streaming cap. The encoder raises quantization within
    /// a frame to stay under this,
    /// which is critical on lossy/relayed UDP links: it flattens the large
    /// IDR keyframe spikes that would otherwise fragment into hundreds of
    /// packets, where a single lost fragment discards the whole frame.
    #[serde(default)]
    pub max_frame_size_bytes: usize,

    /// Percentage of the encoder target used by the SDK screen admission
    /// pacer. The remainder covers QUIC/Anchor overhead and path variation.
    /// Range: 50–100. 98 is the normal low-latency default.
    #[serde(default)]
    pub pacing_utilization_percent: usize,

    /// libx264 thread count. Under `tune=zerolatency` x264 uses *sliced*
    /// threads, so this is also the number of slices per frame. Left to x264
    /// it scales with the host's core count: a 32-core machine encodes 1080p
    /// into 17 slices, and every slice boundary resets intra prediction and
    /// CABAC context, which inflates keyframes for no latency benefit.
    /// Range: 0–16. 0 = let x264 decide. 4 keeps the parallelism that matters
    /// at 1080p without fragmenting the picture.
    #[serde(default = "default_x264_threads")]
    pub x264_threads: u32,

    /// DRM render node used for VAAPI encoding, or "auto" to probe every
    /// `/dev/dri/renderD*` node and select one with H.264 encoding support.
    #[serde(default = "default_vaapi_device")]
    pub vaapi_device: String,
}

fn default_x264_threads() -> u32 {
    4
}

fn default_vaapi_device() -> String {
    "auto".into()
}

impl Default for EncodingSettings {
    fn default() -> Self {
        Self {
            bitrate_bps: 15_000_000,
            fps: 60,
            // ~2s between forced keyframes at 60fps. Keyframes are ~10x the size of
            // a P-frame; a short GOP floods the link with large, fragment-heavy
            // frames that drive UDP loss. Decoder recovery after loss is handled
            // on demand via loss-triggered keyframe requests, so a long GOP is safe.
            gop_size: 120,
            max_b_frames: 0,
            rate_control: "CBR".into(),
            low_power: true,
            x264_preset: "ultrafast".into(),
            x264_tune: "zerolatency".into(),
            hw_pool_size: 20,
            async_depth: 0,
            profile: "auto".into(),
            level: "auto".into(),
            scale_mode: "hq".into(),
            coder: "cabac".into(),
            qp: 0,
            maxrate_bps: 0,
            bufsize_bits: 0,
            max_frame_size_bytes: 0,
            pacing_utilization_percent: 98,
            x264_threads: default_x264_threads(),
            vaapi_device: default_vaapi_device(),
        }
    }
}

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(default)]
pub struct NetworkSettings {
    /// Per-device frame buffer capacity. Frames are dropped if the device
    /// can't keep up. Higher = more tolerance for jitter, more memory.
    /// Range: 2-60. Recommended: 15.
    pub frame_buffer_size: usize,

    /// Time the user has to respond to a pairing request (seconds).
    pub pairing_timeout_secs: u64,

    /// Interval between latency ping/pong measurements (seconds). 0 = disabled.
    pub latency_ping_interval_secs: u64,
}

impl Default for NetworkSettings {
    fn default() -> Self {
        Self { frame_buffer_size: 15, pairing_timeout_secs: 60, latency_ping_interval_secs: 2 }
    }
}

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(default)]
pub struct InputSettings {
    /// Pointer motion multiplier. Higher = faster cursor movement.
    /// Range: 0.5 (slow) to 4.0 (fast). Default 1.5.
    pub default_sensitivity: f32,

    /// Scroll speed multiplier for two-finger scroll gestures.
    /// Range: 0.1 (slow) to 2.0 (fast). Default 0.3.
    pub scroll_multiplier: f32,

    /// Maximum touch duration for a tap (milliseconds).
    /// Touches longer than this are not registered as clicks.
    pub tap_max_duration_ms: u64,

    /// Hold duration before a touch becomes a drag (milliseconds).
    /// Used in fullscreen touchscreen mode.
    pub long_press_threshold_ms: u64,

    /// Maximum touch duration for a quick tap / click (milliseconds).
    pub quick_tap_threshold_ms: u64,
}

impl Default for InputSettings {
    fn default() -> Self {
        Self {
            default_sensitivity: 1.5,
            scroll_multiplier: 0.3,
            tap_max_duration_ms: 250,
            long_press_threshold_ms: 300,
            quick_tap_threshold_ms: 200,
        }
    }
}

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(default)]
pub struct ClipboardSettings {
    /// Enable clipboard sync between desktop and device.
    pub enabled: bool,

    /// Automatically share clipboard changes (if false, must be triggered manually).
    pub auto_share: bool,

    /// Maximum clipboard content size in bytes (text or decoded image).
    /// Range: 1024 (1KB) to 52_428_800 (50MB). Default: 1MB.
    pub max_size_bytes: usize,

    /// Number of clipboard entries to keep in history ring buffer.
    /// Range: 1 to 500. Default: 50.
    pub history_size: usize,

    /// Debounce interval in milliseconds. Rapid clipboard changes within this
    /// window are coalesced — only the final content is sent.
    /// Range: 0 (no debounce) to 2000. Default: 250.
    pub debounce_ms: u64,
}

impl Default for ClipboardSettings {
    fn default() -> Self {
        Self {
            enabled: true,
            auto_share: true,
            max_size_bytes: 1_048_576, // 1 MB
            history_size: 50,
            debounce_ms: 250,
        }
    }
}

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(default)]
pub struct FileTransferSettings {
    /// Enable the AirDrop-style shared folder. Files dropped into `folder` are
    /// sent to the connected phone; files the phone sends land in `folder`.
    pub enabled: bool,

    /// Shared folder path. Empty string resolves to `~/Anchor` (created on
    /// startup if missing).
    pub folder: String,

    /// Bytes of raw file data per chunk before base64 encoding. Deliberately
    /// small so a single chunk clears the shared control channel in a few ms
    /// even on a slow relay, keeping latency-sensitive input responsive during
    /// a transfer. Range: 4 KB to 4 MB. Default: 16 KB.
    pub chunk_size_bytes: usize,

    /// How often to scan the shared folder for newly dropped files (ms).
    /// Range: 250 to 5000. Default: 1000.
    pub poll_interval_ms: u64,
}

impl Default for FileTransferSettings {
    fn default() -> Self {
        Self {
            enabled: true,
            folder: String::new(),
            chunk_size_bytes: 16_384, // 16 KB
            poll_interval_ms: 1000,
        }
    }
}

impl FileTransferSettings {
    /// Resolve the configured folder to an absolute path, defaulting to
    /// `~/Anchor` when unset.
    pub fn resolve_folder(&self) -> std::path::PathBuf {
        if self.folder.trim().is_empty() {
            dirs::home_dir().unwrap_or_else(|| std::path::PathBuf::from(".")).join("Anchor")
        } else {
            std::path::PathBuf::from(shellexpand_tilde(&self.folder))
        }
    }
}

/// Minimal `~` expansion so a user can set `"~/Somewhere"` in settings.json.
fn shellexpand_tilde(path: &str) -> String {
    if let Some(rest) = path.strip_prefix("~/")
        && let Some(home) = dirs::home_dir()
    {
        return home.join(rest).to_string_lossy().into_owned();
    }
    path.to_string()
}

pub fn load_settings() -> AppSettings {
    match get_settings_path() {
        Ok(path) => {
            if path.exists() {
                match fs::read_to_string(&path) {
                    Ok(content) => match serde_json::from_str(&content) {
                        Ok(settings) => {
                            log::debug!("Loaded settings from {:?}", path);
                            let _ = save_settings(&settings);
                            settings
                        }
                        Err(e) => {
                            log::warn!("Failed to parse settings: {} — using defaults", e);
                            let defaults = AppSettings::default();
                            let _ = save_settings(&defaults);
                            defaults
                        }
                    },
                    Err(e) => {
                        log::warn!("Failed to read settings: {} — using defaults", e);
                        AppSettings::default()
                    }
                }
            } else {
                log::info!("No settings file, creating defaults");
                let defaults = AppSettings::default();
                let _ = save_settings(&defaults);
                let _ = write_defaults_doc();
                defaults
            }
        }
        Err(e) => {
            log::error!("Failed to get settings path: {} — using defaults", e);
            AppSettings::default()
        }
    }
}

/// Update the `camera.auto_load` flag on disk. Takes effect at next startup —
/// the in-memory `OnceLock` snapshot remains the boot-time value.
pub fn save_camera_auto_load(value: bool) -> Result<(), String> {
    let mut current = load_settings();
    current.camera.auto_load = value;
    save_settings(&current)
}

/// Update camera streaming parameters on disk. Returns the new persisted values.
pub fn save_camera_stream_params(fps: u32, bitrate_kbps: u32) -> Result<(u32, u32), String> {
    let fps = fps.clamp(1, 120);
    let bitrate_kbps = bitrate_kbps.clamp(100, 50_000);
    let mut current = load_settings();
    current.camera.fps = fps;
    current.camera.bitrate_kbps = bitrate_kbps;
    save_settings(&current)?;
    Ok((fps, bitrate_kbps))
}

pub fn save_settings(settings: &AppSettings) -> Result<(), String> {
    let path = get_settings_path().map_err(|e| format!("Settings path: {}", e))?;
    let content =
        serde_json::to_string_pretty(settings).map_err(|e| format!("Serialize: {}", e))?;
    fs::write(&path, &content).map_err(|e| format!("Write: {}", e))?;
    log::debug!("Saved settings to {:?}", path);
    Ok(())
}

fn get_settings_path() -> Result<PathBuf, String> {
    let dir = dirs::config_dir().ok_or("Could not determine config directory")?.join("anchor");
    if !dir.exists() {
        fs::create_dir_all(&dir).map_err(|e| format!("mkdir: {}", e))?;
    }
    Ok(dir.join("settings.json"))
}

fn write_defaults_doc() -> Result<(), String> {
    let dir = dirs::config_dir().ok_or("Could not determine config directory")?.join("anchor");
    let path = dir.join("settings.defaults.md");
    fs::write(&path, DEFAULTS_DOC).map_err(|e| format!("Write docs: {}", e))?;
    log::info!("Wrote settings documentation to {:?}", path);
    Ok(())
}

const DEFAULTS_DOC: &str = r#"# Anchor Settings Reference

Settings file: `~/.config/anchor/settings.json`
Delete the file to regenerate defaults. Restart the app after editing.

## capture

| Field | Default | Description |
|-------|---------|-------------|
| `backend` | `"wayland_screencopy"` | Capture method. Only `"wayland_screencopy"` supported. |
| `default_output` | `null` | Output index to capture on startup. `null` = first available. |
| `output_refresh_interval_secs` | `2` | How often to check for display changes (seconds). |
| `stats_log_interval_frames` | `60` | How often to log timing stats (frames). 1=every frame, 60=~1s at 60fps. |

## encoding

| Field | Default | Range | Description |
|-------|---------|-------|-------------|
| `bitrate_bps` | `15000000` | 1M–50M | Video bitrate (bits/sec). Higher = better quality. |
| `fps` | `60` | 1–240 | Target framerate. |
| `gop_size` | `120` | 1–300 | Frames between keyframes. Loss-triggered keyframe requests handle recovery, so a longer GOP reduces large keyframe-driven UDP loss. |
| `max_b_frames` | `0` | 0–4 | B-frame count. 0 = lowest latency. |
| `rate_control` | `"CBR"` | | `"CBR"` (constant bitrate), `"CQP"` (constant quality), `"VBR"` (variable). |
| `low_power` | `true` | | Use Intel low-power encoder. Fixes green artifacts on non-standard resolutions. |
| `x264_preset` | `"ultrafast"` | | `"ultrafast"` `"superfast"` `"veryfast"` `"faster"` `"fast"` `"medium"` `"slow"` `"slower"` `"veryslow"` |
| `x264_tune` | `"zerolatency"` | | `"zerolatency"` `"film"` `"animation"` `"grain"` `"stillimage"` |
| `hw_pool_size` | `20` | 4–64 | VAAPI hardware frame buffer pool. |
| `async_depth` | `0` | 0–8 | Encoder pipeline depth. 0=auto, 1=sync, 2-4=pipelined. |
| `profile` | `"auto"` | | `"auto"` `"constrained_baseline"` `"main"` `"high"`. High = best compression. |
| `level` | `"auto"` | | `"auto"` `"3.1"` `"4.1"` `"5.1"`. 4.1 = safe for 1080p60 iOS. |
| `scale_mode` | `"hq"` | | GPU scale quality. `"hq"` (default), `"fast"` (lower latency), `"default"` (driver). |
| `coder` | `"cabac"` | | Entropy coder. `"cabac"` (better compression) or `"cavlc"` (faster decode). |
| `qp` | `0` | 0–52 | Quantization for CQP/ICQ modes. 0=unused, 20-28=typical desktop. |
| `maxrate_bps` | `0` | 0–50M | Max bitrate cap for VBR. 0=no cap. |
| `bufsize_bits` | `0` | 0–100M | VBV buffer size in bits. 0=auto. |
| `pacing_utilization_percent` | `98` | 50–100 | SDK screen admission rate as a percentage of the configured bitrate. Lower values leave more path margin and coalesce more frames instead of building queue latency. |
| `vaapi_device` | `"auto"` | | Probe all DRM render nodes for H.264 VAAPI support, or use an explicit `/dev/dri/renderD*` path. |

## network

| Field | Default | Description |
|-------|---------|-------------|
| `frame_buffer_size` | `15` | Per-device frame buffer. Higher = more jitter tolerance. |
| `pairing_timeout_secs` | `60` | Seconds to respond to pairing request. |
| `latency_ping_interval_secs` | `2` | Ping/pong interval. 0 = disabled. |

## input

| Field | Default | Range | Description |
|-------|---------|-------|-------------|
| `default_sensitivity` | `1.5` | 0.5–4.0 | Pointer motion multiplier. |
| `scroll_multiplier` | `0.3` | 0.1–2.0 | Scroll speed. |
| `tap_max_duration_ms` | `250` | 50–1000 | Max touch time for a tap. |
| `long_press_threshold_ms` | `300` | 100–2000 | Hold time before drag starts. |
| `quick_tap_threshold_ms` | `200` | 50–500 | Max time for a quick-tap click. |
"#;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_are_valid() {
        let s = AppSettings::default();
        assert_eq!(s.encoding.bitrate_bps, 15_000_000);
        assert_eq!(s.encoding.fps, 60);
        assert_eq!(s.encoding.gop_size, 120);
        assert_eq!(s.encoding.pacing_utilization_percent, 98);
        assert_eq!(s.network.frame_buffer_size, 15);
        assert_eq!(s.input.default_sensitivity, 1.5);
    }

    #[test]
    fn serialization_roundtrip() {
        let s = AppSettings::default();
        let json = serde_json::to_string_pretty(&s).unwrap();
        let d: AppSettings = serde_json::from_str(&json).unwrap();
        assert_eq!(s.encoding.bitrate_bps, d.encoding.bitrate_bps);
        assert_eq!(s.encoding.pacing_utilization_percent, d.encoding.pacing_utilization_percent);
        assert_eq!(s.input.scroll_multiplier, d.input.scroll_multiplier);
    }

    #[test]
    fn partial_json_fills_defaults() {
        let json = r#"{"encoding": {"bitrate_bps": 5000000}}"#;
        let s: AppSettings = serde_json::from_str(json).unwrap();
        assert_eq!(s.encoding.bitrate_bps, 5_000_000);
        assert_eq!(s.encoding.fps, 60); // default
    }

    #[test]
    fn empty_json_gives_defaults() {
        let s: AppSettings = serde_json::from_str("{}").unwrap();
        assert_eq!(s.encoding.bitrate_bps, 15_000_000);
    }
}
