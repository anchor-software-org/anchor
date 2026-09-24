use crate::anchorapp::device::FrameBroadcaster;
use crate::anchorapp::event::{AnchorEvent, AnchorMessage, AnchorTarget};
use crate::anchorapp::plugin::{Plugin, SharedFrameBuffer};
use crate::anchorwayland::capture_backend::CaptureBackend;
use crate::anchorwayland::capture_backend::OutputInfo;
use crate::anchorwayland::frame_trace::TraceRing;
use crate::anchorwayland::screencopy::WaylandScreencopyBackend;
use crate::anchorwayland::vaapi_encoder::{VaapiDeviceCtx, VaapiEncoder};
use crate::anchorwayland::wayland_objects::DrmBufParams;
use serde_json::json;
use std::os::unix::io::AsRawFd;
use std::sync::atomic::Ordering;
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::thread;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

#[derive(Debug, Clone)]
pub enum WaylandPluginMsg {
    Ready,
    /// Begin capture + stream atomically.
    Start,
    /// Stop capture + stream atomically → idle.
    Stop,
    /// Pick a screen; auto-restarts if currently streaming.
    SelectOutput(usize),
    OutputList(Vec<OutputInfo>),
    /// Backend failed to initialize (missing protocol, no GPU, etc.).
    /// Carries a human-readable reason for the GUI.
    Unavailable(String),
    /// GUI tab visibility — capture thread skips preview when not on Sideboat tab.
    TabVisible(bool),
    /// Re-send the current outputs to a frontend that attached after startup.
    RequestOutputs,
    /// Move capture away from an output before the compositor unplugs it.
    /// The response is sent only after no capture request can still reference it.
    PrepareOutputRemoval {
        name: String,
        response_tx: mpsc::SyncSender<Result<(), String>>,
    },
}

pub struct AnchorPluginWayland {
    pub plugin_rx: Option<Receiver<AnchorEvent>>,
    pub broker_tx: Option<Sender<AnchorEvent>>,
    pub frame_buffer: Arc<Mutex<Option<SharedFrameBuffer>>>,
    pub broadcaster: Option<Arc<FrameBroadcaster>>,
}

/// What the message handler tells the capture loop to do next.
enum CaptureAction {
    Continue,
    /// Stop capture + stream → return to idle.
    Stop,
    /// Switch to a different output mid-stream (auto-restarts encoder).
    SelectOutput(usize),
    RequestKeyframe,
    /// Phone reconnected mid-stream — re-broadcast current state so it syncs up.
    ResyncToPhone,
    /// GUI switched to/away from Sideboat tab — toggle preview.
    SetTabVisible(bool),
    /// Re-send the current output list without changing capture state.
    RequestOutputs,
    /// Stop or move capture away from an output, then acknowledge its safe removal.
    PrepareOutputRemoval {
        name: String,
        response_tx: mpsc::SyncSender<Result<(), String>>,
    },
}

/// Timing for a single captured frame.
#[derive(Default)]
struct FrameTiming {
    encode_us: u128,
    encoded_size: usize,
    /// Time spent in concat_packets (alloc + memcpy of encode output)
    concat_us: u128,
    /// Time spent in FrameBroadcaster::publish (Arc::new + lock + try_send)
    broadcast_us: u128,
    /// Time inside encode_dmabuf receive_packets loop (clear + extend + take)
    dmabuf_recv_us: u128,
    cpu_map_us: u128,
    cpu_copy_us: u128,
    cpu_convert_us: u128,
    cpu_codec_us: u128,
}

/// Mutable state carried through the capture loop.
struct CaptureState {
    /// True once the H.264 encoder is running and frames are being sent.
    /// Separate from `h264_encoder` so tests can set it without GPU hardware.
    streaming: bool,
    /// H.264 encoder; None = encoder not yet created (starting or switching).
    h264_encoder: Option<VaapiEncoder>,
    /// Shared VAAPI device context. Kept for the whole capture session so
    /// encoder recreations (IDR requests, output changes, resolution changes)
    /// don't each open a fresh /dev/dri/renderD128 fd — which used to leak
    /// under FFmpeg's internal ref chain and eventually exhaust
    /// `RLIMIT_NOFILE` on long runs. `None` when VAAPI isn't available;
    /// the encoder then falls back to software paths.
    vaapi_device_ctx: Option<VaapiDeviceCtx>,
    selected_output_index: usize,
    /// Wayland output name (e.g. "eDP-1") for the currently streamed output.
    selected_output_name: String,
    /// Compositor position of the streamed output — sent in stream_info so the
    /// input plugin maps touches to the right screen without its own output discovery.
    output_x: i32,
    output_y: i32,
    iters: u64,
    encoded_frames: u64,
    stats_encoded_frames: u64,
    stats_started_at: Instant,
    next_encode_deadline: Option<Instant>,
    backend: Box<dyn CaptureBackend>,
    last_encoded_frame_time: Option<Instant>,
    /// When the last recovery IDR was forced. Rate-limits keyframe honoring
    /// so sustained backpressure can't turn into an IDR-per-frame storm —
    /// each 300KB+ keyframe competes with the media that is already behind.
    last_forced_idr: Option<Instant>,
    frame_drops: u64,
    total_frame_drops: u64,
    trace_ring: Arc<TraceRing>,
    /// Whether the GUI's Sideboat/Wayland tab is visible. Preview only runs when true.
    wayland_tab_visible: bool,
}

impl Plugin for AnchorPluginWayland {
    fn run(&mut self) -> Option<JoinHandle<()>> {
        log::debug!("Starting wayland capture thread");
        crate::anchorwayland::frame_trace::announce_run();

        let tx = self.broker_tx.clone().expect("broker_tx not initialized");
        let plugin_rx = self.plugin_rx.take().expect("plugin_rx not initialized");
        let frame_buffer = self.frame_buffer.clone();
        let broadcaster = self.broadcaster.clone();

        // Attempt backend init on the calling thread so we can bail early without
        // spawning a thread that immediately panics.
        let backend: Box<dyn CaptureBackend> = match WaylandScreencopyBackend::try_new() {
            Ok(mut b) => match b.init() {
                Ok(_) => Box::new(b),
                Err(e) => {
                    log::error!("Wayland backend unavailable: {}", e);
                    send_unavailable(&tx, &e);
                    return None;
                }
            },
            Err(e) => {
                log::error!("Wayland backend unavailable: {}", e);
                send_unavailable(&tx, &e);
                return None;
            }
        };

        let handle = thread::spawn(move || {
            let mut backend = backend;

            log::info!("Backend initialized successfully");

            tx.send(AnchorEvent {
                target: AnchorTarget::Broadcast,
                message: AnchorMessage::Wayland(WaylandPluginMsg::Ready),
            })
            .ok();

            let mut last_output_list = backend.get_outputs();
            send_output_list(&tx, &last_output_list);

            let mut selected_output_index: usize = 0;
            // A list index is only a capture-backend cursor and can change on
            // hotplug. The compositor output name is the stable selection ID.
            let mut selected_output_name =
                last_output_list.first().map(|output| output.name.clone()).unwrap_or_default();
            let mut wayland_tab_visible = true;
            let mut preview_sequence = 0_u64;
            let mut last_idle_output_check = Instant::now();

            'outer: loop {
                // ── Idle: wait for Start ──────────────────────────────────────────
                let wait_result = loop {
                    let timeout = if wayland_tab_visible {
                        Duration::from_millis(500)
                    } else {
                        Duration::from_secs(2)
                    };
                    match plugin_rx.recv_timeout(timeout) {
                        Ok(event) => match event.message {
                            AnchorMessage::Wayland(WaylandPluginMsg::Start) => {
                                log::info!("Start received from desktop UI");
                                break WaitResult::Start;
                            }
                            AnchorMessage::Wayland(WaylandPluginMsg::SelectOutput(idx)) => {
                                if let Some(output) = last_output_list.get(idx) {
                                    selected_output_index = idx;
                                    selected_output_name = output.name.clone();
                                }
                            }
                            AnchorMessage::Wayland(WaylandPluginMsg::TabVisible(visible)) => {
                                wayland_tab_visible = visible;
                            }
                            AnchorMessage::Wayland(WaylandPluginMsg::RequestOutputs) => {
                                let old_index = selected_output_index;
                                backend.refresh_outputs();
                                last_output_list = backend.get_outputs();
                                reconcile_output_selection(
                                    &last_output_list,
                                    &mut selected_output_index,
                                    &mut selected_output_name,
                                );
                                send_output_list(&tx, &last_output_list);
                                if selected_output_index != old_index {
                                    send_stream_status(&tx, "idle", selected_output_index, None);
                                }
                            }
                            AnchorMessage::Wayland(WaylandPluginMsg::PrepareOutputRemoval {
                                name,
                                response_tx,
                            }) => {
                                let result = if selected_output_name == name {
                                    match fallback_output_index(&last_output_list, &name) {
                                        Some(index) => {
                                            selected_output_index = index;
                                            selected_output_name =
                                                last_output_list[index].name.clone();
                                            send_stream_status(&tx, "idle", index, None);
                                            Ok(())
                                        }
                                        None => {
                                            Err("Cannot remove the only available display.".into())
                                        }
                                    }
                                } else {
                                    Ok(())
                                };
                                response_tx.send(result).ok();
                            }
                            AnchorMessage::Json(ref json_str) => {
                                if let Ok(json) =
                                    serde_json::from_str::<serde_json::Value>(json_str)
                                {
                                    // New device connected — tell it we're idle and send outputs.
                                    if json.get("type").and_then(|v| v.as_str())
                                        == Some("device_connected")
                                    {
                                        log::info!("device_connected (idle) — sending state");
                                        send_output_list(&tx, &last_output_list);
                                        send_stream_status(
                                            &tx,
                                            "idle",
                                            selected_output_index,
                                            None,
                                        );
                                    }
                                    match json.get("command").and_then(|v| v.as_str()) {
                                        Some("start") => {
                                            log::info!("start from phone");
                                            let outputs_json: Vec<serde_json::Value> =
                                                last_output_list
                                                    .iter()
                                                    .map(|o| {
                                                        json!({
                                                            "id": o.id,
                                                            "name": o.name,
                                                            "width": o.width,
                                                            "height": o.height,
                                                        })
                                                    })
                                                    .collect();
                                            log::info!(
                                                "sending output_list: {} displays",
                                                outputs_json.len()
                                            );
                                            tx.send(AnchorEvent {
                                                target: AnchorTarget::Device,
                                                message: AnchorMessage::Json(
                                                    json!({
                                                        "plugin_id": "video",
                                                        "type": "output_list",
                                                        "outputs": outputs_json,
                                                    })
                                                    .to_string(),
                                                ),
                                            })
                                            .ok();
                                            break WaitResult::Start;
                                        }
                                        Some("select_output") => {
                                            if let Some(idx) =
                                                json.get("index").and_then(|v| v.as_u64())
                                            {
                                                if let Some(output) =
                                                    last_output_list.get(idx as usize)
                                                {
                                                    selected_output_index = idx as usize;
                                                    selected_output_name = output.name.clone();
                                                    log::info!(
                                                        "select_output({}) from phone (idle)",
                                                        idx
                                                    );
                                                }
                                            }
                                        }
                                        _ => {}
                                    }
                                }
                            }
                            _ => {}
                        },
                        Err(mpsc::RecvTimeoutError::Timeout) => {
                            if wayland_tab_visible {
                                preview_sequence = preview_sequence.wrapping_add(1);
                                capture_idle_preview(
                                    &mut backend,
                                    selected_output_index,
                                    preview_sequence,
                                    &frame_buffer,
                                );
                            }
                            if last_idle_output_check.elapsed() >= Duration::from_secs(2) {
                                let old_index = selected_output_index;
                                let removed = check_output_changes(
                                    &mut backend,
                                    &mut last_output_list,
                                    &tx,
                                    Some(&selected_output_name),
                                );
                                reconcile_output_selection(
                                    &last_output_list,
                                    &mut selected_output_index,
                                    &mut selected_output_name,
                                );
                                if removed || selected_output_index != old_index {
                                    send_stream_status(&tx, "idle", selected_output_index, None);
                                }
                                last_idle_output_check = Instant::now();
                            }
                        }
                        Err(mpsc::RecvTimeoutError::Disconnected) => break WaitResult::Shutdown,
                    }
                };
                match wait_result {
                    WaitResult::Start => {}
                    WaitResult::Shutdown => break 'outer,
                }

                let (resolved_output_name, output_x, output_y) = last_output_list
                    .get(selected_output_index)
                    .map(|o| (o.name.clone(), o.x, o.y))
                    .unwrap_or_default();

                let mut state = CaptureState {
                    streaming: false,
                    h264_encoder: None,
                    vaapi_device_ctx: VaapiDeviceCtx::try_new(
                        &crate::anchorapp::settings::settings().encoding.vaapi_device,
                    ),
                    selected_output_index,
                    selected_output_name: resolved_output_name,
                    output_x,
                    output_y,
                    iters: 1,
                    encoded_frames: 0,
                    stats_encoded_frames: 0,
                    stats_started_at: Instant::now(),
                    next_encode_deadline: None,
                    backend,
                    last_encoded_frame_time: None,
                    last_forced_idr: None,
                    frame_drops: 0,
                    total_frame_drops: 0,
                    trace_ring: Arc::new(TraceRing::new()),
                    wayland_tab_visible,
                };

                // Encoder will be created on first captured frame.
                send_stream_status(&tx, "starting", state.selected_output_index, None);

                // ── Capture + stream loop ─────────────────────────────────────────
                'screenloop: loop {
                    state.trace_ring.begin(state.iters);
                    let captured = match state.backend.capture_frame(state.selected_output_index) {
                        Ok(frame) => frame,
                        Err(e) => {
                            // Check if the output was unplugged — stop cleanly instead of erroring.
                            state.backend.refresh_outputs();
                            last_output_list = state.backend.get_outputs();
                            send_output_list(&tx, &last_output_list);
                            let still_exists = last_output_list
                                .iter()
                                .any(|o| o.name == state.selected_output_name);
                            if !still_exists {
                                log::info!(
                                    "Streaming output '{}' removed — stopping stream",
                                    state.selected_output_name
                                );
                            } else {
                                log::error!("Backend capture error: {}", e);
                            }
                            let next_index = last_output_list
                                .iter()
                                .position(|output| output.name == state.selected_output_name)
                                .unwrap_or(0);
                            apply_output_selection(&mut state, &last_output_list, next_index);
                            state.streaming = false;
                            state.h264_encoder = None;
                            send_stream_status(&tx, "idle", state.selected_output_index, None);
                            break 'screenloop;
                        }
                    };

                    state.trace_ring.mark_capture_ready(state.iters);

                    let w = captured.width;
                    let h = captured.height;
                    let fourcc = captured.format.to_drm_fourcc();

                    let mut timing = FrameTiming::default();

                    state.trace_ring.mark_encode_start(state.iters);
                    let encoded = process_frame(
                        &captured,
                        &mut state,
                        &mut timing,
                        &broadcaster,
                        &tx,
                        w,
                        h,
                        fourcc,
                    );
                    state.trace_ring.mark_encode_done(state.iters, timing.encoded_size);
                    state.trace_ring.mark_queued(state.iters);

                    if encoded {
                        let now = Instant::now();
                        let ifi_us = state
                            .last_encoded_frame_time
                            .map(|last| now.duration_since(last).as_micros() as u64)
                            .unwrap_or(0);
                        state.last_encoded_frame_time = Some(now);
                        state.encoded_frames += 1;
                        state.stats_encoded_frames += 1;
                        log_frame_timing(&timing, &state, ifi_us);
                    }

                    let action = poll_messages(&plugin_rx, &state);

                    match action {
                        CaptureAction::Stop => {
                            send_stream_status(&tx, "stopping", state.selected_output_index, None);
                            state.streaming = false;
                            state.h264_encoder = None;
                            send_stream_status(&tx, "idle", state.selected_output_index, None);
                            break 'screenloop;
                        }
                        CaptureAction::SelectOutput(idx) => {
                            if try_apply_output_selection(&mut state, &last_output_list, idx) {
                                log::info!("SelectOutput({}) mid-stream — switching", idx);
                                // Update absolute input before the next encoded frame. A pen
                                // event can arrive as soon as the viewer selects the output.
                                if let Some(output) =
                                    last_output_list.get(state.selected_output_index)
                                {
                                    send_stream_info(
                                        &tx,
                                        output.width,
                                        output.height,
                                        &state.selected_output_name,
                                        state.output_x,
                                        state.output_y,
                                    );
                                }
                                state.streaming = false;
                                state.h264_encoder = None;
                                send_stream_status(&tx, "switching", idx, None);
                                // Encoder re-created on next frame via process_frame.
                            } else {
                                // A device can race an output deletion and send an index from
                                // its previous output list. Keep the valid stream alive and
                                // immediately repair the device's view of topology/state.
                                log::warn!(
                                    "Ignoring stale SelectOutput({}); only {} outputs remain",
                                    idx,
                                    last_output_list.len()
                                );
                                send_output_list(&tx, &last_output_list);
                                send_stream_status(
                                    &tx,
                                    if state.streaming { "streaming" } else { "idle" },
                                    state.selected_output_index,
                                    None,
                                );
                            }
                        }
                        CaptureAction::RequestKeyframe => {
                            // The receiver re-requests while awaiting IDR, so
                            // honor at most one per interval; the next request
                            // arrives before the limit could stall recovery.
                            if idr_request_due(&state, Instant::now()) {
                                state.last_forced_idr = Some(Instant::now());
                                request_idr(&mut state, w, h, fourcc, &tx);
                            }
                        }
                        CaptureAction::ResyncToPhone => {
                            let s = if state.streaming { "streaming" } else { "starting" };
                            send_stream_status(&tx, s, state.selected_output_index, None);
                            send_output_list(&tx, &last_output_list);
                            if state.streaming {
                                send_stream_info(
                                    &tx,
                                    w,
                                    h,
                                    &state.selected_output_name,
                                    state.output_x,
                                    state.output_y,
                                );
                            }
                        }
                        CaptureAction::RequestOutputs => {
                            let old_index = state.selected_output_index;
                            state.backend.refresh_outputs();
                            last_output_list = state.backend.get_outputs();
                            send_output_list(&tx, &last_output_list);
                            let selected_still_present = last_output_list
                                .iter()
                                .any(|output| output.name == state.selected_output_name);
                            if selected_still_present {
                                if let Some(index) = last_output_list
                                    .iter()
                                    .position(|output| output.name == state.selected_output_name)
                                {
                                    apply_output_selection(&mut state, &last_output_list, index);
                                    if index != old_index {
                                        send_stream_status(&tx, "streaming", index, None);
                                    }
                                }
                            } else {
                                log::warn!(
                                    "Streaming output '{}' disappeared during topology refresh",
                                    state.selected_output_name
                                );
                                state.streaming = false;
                                state.h264_encoder = None;
                                apply_output_selection(&mut state, &last_output_list, 0);
                                send_stream_status(&tx, "idle", state.selected_output_index, None);
                                break 'screenloop;
                            }
                        }
                        CaptureAction::PrepareOutputRemoval { name, response_tx } => {
                            let selected = state.selected_output_name == name;
                            if selected {
                                match fallback_output_index(&last_output_list, &name) {
                                    Some(index) => {
                                        log::info!(
                                            "Stopping stream before removing output '{}'",
                                            name
                                        );
                                        send_stream_status(
                                            &tx,
                                            "stopping",
                                            state.selected_output_index,
                                            None,
                                        );
                                        state.streaming = false;
                                        state.h264_encoder = None;
                                        apply_output_selection(
                                            &mut state,
                                            &last_output_list,
                                            index,
                                        );
                                        send_stream_status(&tx, "idle", index, None);
                                        response_tx.send(Ok(())).ok();
                                        break 'screenloop;
                                    }
                                    None => {
                                        response_tx
                                            .send(Err(
                                                "Cannot remove the only available display.".into()
                                            ))
                                            .ok();
                                    }
                                }
                            } else {
                                response_tx.send(Ok(())).ok();
                            }
                        }
                        CaptureAction::Continue => {}
                        CaptureAction::SetTabVisible(visible) => {
                            state.wayland_tab_visible = visible;
                            wayland_tab_visible = visible;
                        }
                    }

                    state.iters += 1;
                    report_stats(&mut state, w as usize, h as usize, &tx);

                    if state.iters.is_multiple_of(120) {
                        let removed = check_output_changes(
                            &mut state.backend,
                            &mut last_output_list,
                            &tx,
                            Some(&state.selected_output_name),
                        );
                        if removed {
                            log::info!(
                                "Streaming output '{}' removed — stopping stream",
                                state.selected_output_name
                            );
                            state.streaming = false;
                            state.h264_encoder = None;
                            apply_output_selection(&mut state, &last_output_list, 0);
                            send_stream_status(&tx, "idle", state.selected_output_index, None);
                            break 'screenloop;
                        } else if let Some(index) = last_output_list
                            .iter()
                            .position(|output| output.name == state.selected_output_name)
                        {
                            let old_index = state.selected_output_index;
                            // Refresh geometry even when the stable output kept
                            // the same cursor position in the new list.
                            apply_output_selection(&mut state, &last_output_list, index);
                            if index != old_index {
                                send_stream_status(&tx, "streaming", index, None);
                            }
                        }
                    }
                }

                backend = state.backend;
                selected_output_index = state.selected_output_index;
                selected_output_name = state.selected_output_name;
            }

            log::debug!("ENDING WAYLAND THREAD");
        });

        Some(handle)
    }

    fn init(
        plugin_rx: Option<Receiver<AnchorEvent>>,
        broker_tx: Option<Sender<AnchorEvent>>,
        frame_buffer: Arc<Mutex<Option<SharedFrameBuffer>>>,
    ) -> Self {
        AnchorPluginWayland { plugin_rx, broker_tx, frame_buffer, broadcaster: None }
    }
}

// ── Idle loop ─────────────────────────────────────────────────────────────────

enum WaitResult {
    Start,
    Shutdown,
}

fn capture_idle_preview(
    backend: &mut Box<dyn CaptureBackend>,
    output_index: usize,
    sequence: u64,
    frame_buffer: &Arc<Mutex<Option<SharedFrameBuffer>>>,
) {
    let captured = match backend.capture_frame(output_index) {
        Ok(captured) => captured,
        Err(error) => {
            log::debug!("Idle preview capture failed: {error}");
            return;
        }
    };
    let Some(ref dma_buf) = captured.dma_buf else {
        return;
    };
    capture_preview_frame(
        dma_buf,
        captured.width as usize,
        captured.height as usize,
        captured.stride as usize,
        sequence,
        frame_buffer,
    );
}

/// Process a single captured frame and encode it for the phone. Idle previews
/// are captured separately, so preview work never enters the streaming path.
#[allow(clippy::too_many_arguments)]
fn process_frame(
    captured: &crate::anchorwayland::capture_backend::CapturedFrame,
    state: &mut CaptureState,
    timing: &mut FrameTiming,
    broadcaster: &Option<Arc<FrameBroadcaster>>,
    tx: &Sender<AnchorEvent>,
    w: u32,
    h: u32,
    fourcc: u32,
) -> bool {
    let stride = captured.stride as usize;

    // ── Create encoder on first frame (or after a screen switch) ─────────────
    if state.h264_encoder.is_none() {
        start_streaming(state, w, h, fourcc, tx);
        // Fall through — encoder now exists (if init succeeded), encode this frame.
    }

    // ── Congestion backpressure ──────────────────────────────────────────────
    // A subscriber queue was full on the last publish. Skip ENCODING entirely
    // rather than producing an access unit that gets dropped post-encode: a
    // frame the encoder never saw cannot break the decoder's reference chain,
    // so congestion costs framerate instead of a ~350KB recovery IDR (which
    // then stalls the pacer and causes the next drop — the IDR treadmill).
    // Pending keyframe requests stay queued and are honored once a drain
    // thread clears the flag.
    if broadcaster.as_ref().is_some_and(|bc| bc.has_backpressure()) {
        state.frame_drops += 1;
        state.total_frame_drops += 1;
        return false;
    }

    // ── Handle IDR requests from broadcaster ─────────────────────────────────
    // Peek rather than take: if honoring is still rate-limited, the flag stays
    // set so a later frame forces the IDR once the interval elapses.
    let keyframe_pending =
        broadcaster.as_ref().is_some_and(|bc| bc.needs_keyframe.load(Ordering::Relaxed));
    if state.h264_encoder.is_some() && keyframe_pending && idr_request_due(state, Instant::now()) {
        if let Some(bc) = broadcaster {
            bc.take_keyframe_request();
        }
        state.last_forced_idr = Some(Instant::now());
        state.frame_drops += 1;
        state.total_frame_drops += 1;
        log::debug!("Frame drop #{} — forcing IDR on next frame", state.total_frame_drops);
        request_idr(state, w, h, fourcc, tx);
    }

    // ── FPS limiting ─────────────────────────────────────────────────────────
    if !should_encode_at(
        &mut state.next_encode_deadline,
        Instant::now(),
        crate::anchorapp::settings::settings().encoding.fps,
    ) {
        return false;
    }

    // ── Encode ───────────────────────────────────────────────────────────────
    let Some(encoder) = &mut state.h264_encoder else {
        return false;
    };
    if let Some(ref cpu_buffer) = captured.cpu_buffer {
        encode_cpu_streaming_frame(
            cpu_buffer.as_slice(),
            encoder,
            stride,
            captured.format.to_drm_fourcc(),
            state.iters,
            broadcaster,
            timing,
        );
        return true;
    }
    let Some(ref dma_buf) = captured.dma_buf else {
        return false;
    };
    encode_streaming_frame(
        dma_buf,
        encoder,
        w as usize,
        h as usize,
        stride,
        fourcc,
        state.iters,
        broadcaster,
        &mut timing.encode_us,
        &mut timing.concat_us,
        &mut timing.broadcast_us,
        &mut timing.dmabuf_recv_us,
        &mut timing.encoded_size,
        &mut timing.cpu_map_us,
        &mut timing.cpu_copy_us,
        &mut timing.cpu_convert_us,
        &mut timing.cpu_codec_us,
    );
    if encoder.selected_encoder != "h264_vaapi"
        && !encoder.vpp_available
        && let Err(e) = state.backend.prefer_cpu_capture()
    {
        log::warn!("Failed to switch to wl_shm CPU capture: {e}");
    }
    true
}

fn should_encode_at(deadline: &mut Option<Instant>, now: Instant, target_fps: u32) -> bool {
    if target_fps == 0 {
        return true;
    }
    let interval = Duration::from_secs_f64(1.0 / target_fps as f64);
    match *deadline {
        None => {
            *deadline = Some(now + interval);
            true
        }
        Some(next) if now < next => false,
        Some(next) => {
            let following = next + interval;
            *deadline = Some(if following <= now { now + interval } else { following });
            true
        }
    }
}

/// Returns true if `streaming_output_name` is no longer present after the refresh.
fn check_output_changes(
    backend: &mut Box<dyn CaptureBackend>,
    last: &mut Vec<crate::anchorwayland::capture_backend::OutputInfo>,
    tx: &Sender<AnchorEvent>,
    streaming_output_name: Option<&str>,
) -> bool {
    backend.refresh_outputs();
    let current = backend.get_outputs();
    if current != *last {
        log::info!("Output list changed: {} outputs", current.len());
        *last = current;
        send_output_list(tx, last);
    }
    if let Some(name) = streaming_output_name {
        !last.iter().any(|o| o.name == name)
    } else {
        false
    }
}

fn send_output_list(
    tx: &Sender<AnchorEvent>,
    outputs: &[crate::anchorwayland::capture_backend::OutputInfo],
) {
    // GUI gets the typed message for the combobox.
    tx.send(AnchorEvent {
        target: AnchorTarget::Gui,
        message: AnchorMessage::Wayland(WaylandPluginMsg::OutputList(outputs.to_vec())),
    })
    .ok();
    // Phones get JSON with stable IDs.
    let outputs_json: Vec<serde_json::Value> = outputs
        .iter()
        .map(|o| {
            json!({
                "id": o.id,
                "name": o.name,
                "width": o.width,
                "height": o.height,
                "x": o.x,
                "y": o.y,
            })
        })
        .collect();
    // Input owns a separate Wayland connection, so it cannot rely on the
    // capture worker's registry state. Push every refreshed topology to it so
    // absolute coordinates shrink as well as expand when outputs change.
    tx.send(AnchorEvent {
        target: AnchorTarget::Broadcast,
        message: AnchorMessage::Json(
            json!({"type": "output_layout", "outputs": outputs_json.clone()}).to_string(),
        ),
    })
    .ok();
    tx.send(AnchorEvent {
        target: AnchorTarget::Device,
        message: AnchorMessage::Json(
            json!({
                "plugin_id": "video",
                "type": "output_list",
                "outputs": outputs_json,
            })
            .to_string(),
        ),
    })
    .ok();
}

// ── Message polling ───────────────────────────────────────────────────────────

fn poll_messages(plugin_rx: &Receiver<AnchorEvent>, state: &CaptureState) -> CaptureAction {
    for event in plugin_rx.try_iter() {
        match event.message {
            AnchorMessage::Wayland(WaylandPluginMsg::Stop) => {
                log::debug!("Stop — ending capture+stream");
                return CaptureAction::Stop;
            }
            AnchorMessage::Wayland(WaylandPluginMsg::Start) => {
                // Phone reconnected while we're already streaming — resync state.
                if state.streaming {
                    log::info!("Start received while streaming — resyncing phone state");
                    return CaptureAction::ResyncToPhone;
                }
            }
            AnchorMessage::Wayland(WaylandPluginMsg::TabVisible(visible)) => {
                return CaptureAction::SetTabVisible(visible);
            }
            AnchorMessage::Wayland(WaylandPluginMsg::RequestOutputs) => {
                return CaptureAction::RequestOutputs;
            }
            AnchorMessage::Wayland(WaylandPluginMsg::PrepareOutputRemoval {
                name,
                response_tx,
            }) => {
                return CaptureAction::PrepareOutputRemoval { name, response_tx };
            }
            AnchorMessage::Wayland(WaylandPluginMsg::SelectOutput(idx)) => {
                return CaptureAction::SelectOutput(idx);
            }
            AnchorMessage::Json(ref json_str) => {
                if let Ok(json) = serde_json::from_str::<serde_json::Value>(json_str) {
                    // Device (re)connected — immediately push current state.
                    if json.get("type").and_then(|v| v.as_str()) == Some("device_connected") {
                        log::info!("device_connected while capturing — resyncing phone state");
                        return CaptureAction::ResyncToPhone;
                    }
                    match json.get("command").and_then(|v| v.as_str()) {
                        Some("stop") => {
                            log::info!("stop from phone");
                            return CaptureAction::Stop;
                        }
                        Some("start") => {
                            // Phone reconnected while we're already streaming — resync state.
                            if state.streaming {
                                log::info!("start from phone while streaming — resyncing state");
                                return CaptureAction::ResyncToPhone;
                            }
                        }
                        Some("select_output") => {
                            if let Some(idx) = json.get("index").and_then(|v| v.as_u64()) {
                                log::info!("select_output({}) from phone", idx);
                                return CaptureAction::SelectOutput(idx as usize);
                            }
                        }
                        Some("request_keyframe") if state.streaming => {
                            log::info!("Keyframe requested — recreating VAAPI encoder");
                            return CaptureAction::RequestKeyframe;
                        }
                        _ => {}
                    }
                }
            }
            _ => {}
        }
    }
    CaptureAction::Continue
}

fn fallback_output_index(outputs: &[OutputInfo], removed_name: &str) -> Option<usize> {
    anchor_topology_core::fallback_index(
        outputs.iter().map(|output| output.name.as_str()),
        removed_name,
    )
}

fn reconcile_output_selection(
    outputs: &[OutputInfo],
    selected_index: &mut usize,
    selected_name: &mut String,
) -> bool {
    let mut selection =
        anchor_topology_core::OutputSelection::new(*selected_index, selected_name.clone());
    let outcome = anchor_topology_core::reconcile_selection(
        outputs.iter().map(|output| output.name.as_str()),
        &mut selection,
    );
    *selected_index = selection.index;
    *selected_name = selection.name;
    matches!(outcome, anchor_topology_core::ReconcileOutcome::Preserved)
}

fn apply_output_selection(state: &mut CaptureState, outputs: &[OutputInfo], index: usize) {
    state.selected_output_index = if outputs.is_empty() { 0 } else { index.min(outputs.len() - 1) };
    let output = outputs.get(state.selected_output_index);
    state.selected_output_name = output.map(|output| output.name.clone()).unwrap_or_default();
    state.output_x = output.map(|output| output.x).unwrap_or(0);
    state.output_y = output.map(|output| output.y).unwrap_or(0);
}

fn try_apply_output_selection(
    state: &mut CaptureState,
    outputs: &[OutputInfo],
    index: usize,
) -> bool {
    if index >= outputs.len() {
        return false;
    }
    apply_output_selection(state, outputs, index);
    true
}

// ── Encoder management ────────────────────────────────────────────────────────

fn start_streaming(
    state: &mut CaptureState,
    width: u32,
    height: u32,
    compositor_fourcc: u32,
    tx: &Sender<AnchorEvent>,
) {
    match VaapiEncoder::new(
        state.vaapi_device_ctx.as_ref(),
        width,
        height,
        crate::anchorapp::settings::settings().encoding.bitrate_bps,
        crate::anchorapp::settings::settings().encoding.fps,
        compositor_fourcc,
    ) {
        Ok(enc) => {
            state.streaming = true;
            state.h264_encoder = Some(enc);
            send_stream_info(
                tx,
                width,
                height,
                &state.selected_output_name,
                state.output_x,
                state.output_y,
            );
            send_stream_status(tx, "streaming", state.selected_output_index, None);
            log::info!("VAAPI H.264 encoder started {}x{}", width, height);
        }
        Err(e) => {
            log::error!("VAAPI encoder init failed: {}", e);
            send_stream_status(
                tx,
                "error",
                state.selected_output_index,
                Some(&format!("Encoder init failed: {}", e)),
            );
        }
    }
}

/// Minimum spacing between honored IDR requests. Matches the receiver's
/// re-request cadence so a fresh request always lands before recovery stalls.
const MIN_IDR_INTERVAL: Duration = Duration::from_millis(250);

fn idr_request_due(state: &CaptureState, now: Instant) -> bool {
    state.last_forced_idr.is_none_or(|at| now.duration_since(at) >= MIN_IDR_INTERVAL)
}

/// Force the next encoded frame to be an IDR keyframe so a decoder that lost
/// reference frames can recover. Reuses the live encoder (no filter-graph rebuild,
/// no extra `/dev/dri/renderD128` fd, no capture stall) — a full recreate is only
/// needed when there is no encoder yet.
fn request_idr(
    state: &mut CaptureState,
    width: u32,
    height: u32,
    compositor_fourcc: u32,
    tx: &Sender<AnchorEvent>,
) {
    if let Some(encoder) = state.h264_encoder.as_mut() {
        encoder.request_keyframe();
    } else {
        recreate_encoder(state, width, height, compositor_fourcc, tx);
    }
}

fn recreate_encoder(
    state: &mut CaptureState,
    width: u32,
    height: u32,
    compositor_fourcc: u32,
    tx: &Sender<AnchorEvent>,
) {
    match VaapiEncoder::new(
        state.vaapi_device_ctx.as_ref(),
        width,
        height,
        crate::anchorapp::settings::settings().encoding.bitrate_bps,
        crate::anchorapp::settings::settings().encoding.fps,
        compositor_fourcc,
    ) {
        Ok(enc) => {
            state.h264_encoder = Some(enc);
            send_stream_info(
                tx,
                width,
                height,
                &state.selected_output_name,
                state.output_x,
                state.output_y,
            );
            log::info!("VAAPI encoder recreated — next frame will be IDR");
        }
        Err(e) => log::error!("VAAPI encoder recreate failed: {}", e),
    }
}

// ── Helpers: messaging ────────────────────────────────────────────────────────

/// Emit unified stream status to both the GUI and connected phones.
/// `state` ∈ { "idle", "starting", "streaming", "switching", "stopping", "error" }
fn send_stream_status(
    tx: &Sender<AnchorEvent>,
    state: &str,
    selected_index: usize,
    error: Option<&str>,
) {
    let mut gui_payload =
        json!({"type": "capturing_state", "state": state, "selected_index": selected_index});
    let mut phone_payload = json!({
        "plugin_id": "video",
        "type": "stream_status",
        "state": state,
        "selected_index": selected_index,
    });
    if let Some(err) = error {
        gui_payload["error"] = serde_json::Value::String(err.to_string());
        phone_payload["error"] = serde_json::Value::String(err.to_string());
    }

    tx.send(AnchorEvent {
        target: AnchorTarget::Gui,
        message: AnchorMessage::Json(gui_payload.to_string()),
    })
    .ok();
    tx.send(AnchorEvent {
        target: AnchorTarget::Device,
        message: AnchorMessage::Json(phone_payload.to_string()),
    })
    .ok();
}

fn send_stream_info(
    tx: &Sender<AnchorEvent>,
    width: u32,
    height: u32,
    output_name: &str,
    output_x: i32,
    output_y: i32,
) {
    let json = json!({
        "type": "stream_info",
        "width": width,
        "height": height,
        "output_name": output_name,
        "output_x": output_x,
        "output_y": output_y,
    })
    .to_string();
    tx.send(AnchorEvent {
        target: AnchorTarget::Device,
        message: AnchorMessage::Json(json.clone()),
    })
    .ok();
    tx.send(AnchorEvent { target: AnchorTarget::Broadcast, message: AnchorMessage::Json(json) })
        .ok();
}

// ── Helpers: logging ──────────────────────────────────────────────────────────

fn stats_interval() -> u64 {
    crate::anchorapp::settings::settings().capture.stats_log_interval_frames
}

fn log_frame_timing(timing: &FrameTiming, state: &CaptureState, ifi_us: u64) {
    if !state.encoded_frames.is_multiple_of(stats_interval()) {
        return;
    }
    let is_keyframe = timing.encoded_size > 10000 && state.encoded_frames > 1;
    log::debug!(
        "[perf] frame={} | encode={}us cpu_map={}us cpu_copy={}us cpu_convert={}us cpu_codec={}us dmabuf_recv={}us concat={}us broadcast={}us enc_size={}B ifi={}us drops={} keyframe={} | streaming={}",
        state.encoded_frames,
        timing.encode_us,
        timing.cpu_map_us,
        timing.cpu_copy_us,
        timing.cpu_convert_us,
        timing.cpu_codec_us,
        timing.dmabuf_recv_us,
        timing.concat_us,
        timing.broadcast_us,
        timing.encoded_size,
        ifi_us,
        state.frame_drops,
        is_keyframe,
        state.streaming,
    );
    crate::metrics::emit(serde_json::json!({
        "t": "frame",
        "frame": state.encoded_frames,
        "encode_us": timing.encode_us,
        "cpu_map_us": timing.cpu_map_us,
        "cpu_copy_us": timing.cpu_copy_us,
        "cpu_convert_us": timing.cpu_convert_us,
        "cpu_codec_us": timing.cpu_codec_us,
        "concat_us": timing.concat_us,
        "dmabuf_recv_us": timing.dmabuf_recv_us,
        "broadcast_us": timing.broadcast_us,
        "enc_size_bytes": timing.encoded_size,
        "ifi_us": ifi_us,
        "drops": state.frame_drops,
        "is_keyframe": is_keyframe,
        "streaming": state.streaming,
    }));
    // Emit the per-frame nanosecond trace for this interval sample.
    let trace_json = state.trace_ring.get(state.iters).to_json_log();
    crate::metrics::emit_raw(&trace_json);
}

fn report_stats(state: &mut CaptureState, width: usize, height: usize, tx: &Sender<AnchorEvent>) {
    let target_fps = crate::anchorapp::settings::settings().encoding.fps.max(1) as f64;
    let report_interval = Duration::from_secs_f64(stats_interval() as f64 / target_fps);
    let elapsed = state.stats_started_at.elapsed();
    if elapsed < report_interval {
        return;
    }
    let elapsed_us = elapsed.as_micros().max(1);
    let fps = state.stats_encoded_frames as u128 * 1_000_000 / elapsed_us;
    let avg_frame_ms = if state.stats_encoded_frames == 0 {
        0
    } else {
        elapsed.as_millis() / state.stats_encoded_frames as u128
    };
    state.stats_encoded_frames = 0;
    state.stats_started_at = Instant::now();

    let streaming = state.streaming;
    log::debug!(
        "[timing] avg frame={}ms fps={} drops={} total_drops={} (streaming={})",
        avg_frame_ms,
        fps,
        state.frame_drops,
        state.total_frame_drops,
        streaming,
    );
    crate::metrics::emit(serde_json::json!({
        "t": "timing",
        "avg_frame_ms": avg_frame_ms,
        "fps": fps,
        "drops": state.frame_drops,
        "total_drops": state.total_frame_drops,
        "streaming": streaming,
    }));
    state.frame_drops = 0;

    tx.send(AnchorEvent {
        target: AnchorTarget::Gui,
        message: AnchorMessage::Json(
            json!({
                "type": "state",
                "state": if streaming { "streaming" } else { "starting" },
                "fps": fps,
                "avg_frame_ms": avg_frame_ms,
                "frame_size_bytes": (width * height * 4) as u32,
                "bitrate_kbps": 0u32,
                "latency_ms": 0u32,
            })
            .to_string(),
        ),
    })
    .ok();
}

// ── Helpers: frame processing ─────────────────────────────────────────────────

#[allow(clippy::too_many_arguments)]
fn encode_streaming_frame(
    buf_params: &DrmBufParams,
    encoder: &mut VaapiEncoder,
    width: usize,
    height: usize,
    stride: usize,
    compositor_fourcc: u32,
    source_frame_id: u64,
    broadcaster: &Option<Arc<FrameBroadcaster>>,
    encode_us: &mut u128,
    concat_us: &mut u128,
    broadcast_us: &mut u128,
    dmabuf_recv_us: &mut u128,
    encoded_size: &mut usize,
    cpu_map_us: &mut u128,
    cpu_copy_us: &mut u128,
    cpu_convert_us: &mut u128,
    cpu_codec_us: &mut u128,
) {
    let t = Instant::now();

    let combined: Option<Vec<u8>> = if encoder.selected_encoder == "h264_vaapi"
        && encoder.dmabuf_import_available
    {
        match encoder.encode_dmabuf(
            buf_params.fd.as_raw_fd(),
            buf_params.stride,
            buf_params.offset,
            u64::from(buf_params.modifier),
            compositor_fourcc,
        ) {
            Ok((data, timing)) if !data.is_empty() => {
                *dmabuf_recv_us = timing.receive_packets_us;
                Some(data)
            }
            Ok(_) => None,
            Err(e) => {
                // A rejected import is a capability mismatch (for example
                // ENOSYS from an AMD driver), not a transient frame failure.
                // Stop retrying it on every frame and use the CPU fallback.
                encoder.dmabuf_import_available = false;
                log::warn!(
                    "encode_dmabuf failed ({}); disabling zero-copy DMA-BUF import and using CPU path",
                    e
                );
                let packets = encode_xbgr_fallback(
                    encoder,
                    buf_params,
                    width,
                    height,
                    stride,
                    compositor_fourcc,
                    cpu_map_us,
                    cpu_copy_us,
                    cpu_convert_us,
                    cpu_codec_us,
                );
                concat_packets(packets, concat_us)
            }
        }
    } else if encoder.selected_encoder == "h264_vaapi" {
        let packets = encode_xbgr_fallback(
            encoder,
            buf_params,
            width,
            height,
            stride,
            compositor_fourcc,
            cpu_map_us,
            cpu_copy_us,
            cpu_convert_us,
            cpu_codec_us,
        );
        concat_packets(packets, concat_us)
    } else if encoder.vpp_available {
        match encoder.encode_dmabuf_vpp(
            buf_params.fd.as_raw_fd(),
            buf_params.stride,
            buf_params.offset,
            u64::from(buf_params.modifier),
            compositor_fourcc,
        ) {
            Ok(pkts) => concat_packets(pkts, concat_us),
            Err(e) => {
                // A failed DMA-BUF import is normally a capability mismatch (for
                // example AVERROR(ENOSYS)), not a transient per-frame error.  Keep
                // using the working CPU path for this encoder instead of retrying
                // the same expensive hardware operation and logging at 60 fps.
                encoder.vpp_available = false;
                log::warn!("encode_dmabuf_vpp failed ({}); disabling VPP and using CPU path", e);
                let packets = encode_xbgr_fallback(
                    encoder,
                    buf_params,
                    width,
                    height,
                    stride,
                    compositor_fourcc,
                    cpu_map_us,
                    cpu_copy_us,
                    cpu_convert_us,
                    cpu_codec_us,
                );
                concat_packets(packets, concat_us)
            }
        }
    } else {
        let packets = encode_xbgr_fallback(
            encoder,
            buf_params,
            width,
            height,
            stride,
            compositor_fourcc,
            cpu_map_us,
            cpu_copy_us,
            cpu_convert_us,
            cpu_codec_us,
        );
        concat_packets(packets, concat_us)
    };

    *encode_us = t.elapsed().as_micros();

    if let Some(data) = combined {
        *encoded_size = data.len();
        if let Some(bc) = broadcaster {
            let t_broadcast = Instant::now();
            bc.publish_frame(source_frame_id, data);
            *broadcast_us = t_broadcast.elapsed().as_micros();
        }
    }
}

fn encode_cpu_streaming_frame(
    pixels: &[u8],
    encoder: &mut VaapiEncoder,
    stride: usize,
    drm_fourcc: u32,
    source_frame_id: u64,
    broadcaster: &Option<Arc<FrameBroadcaster>>,
    timing: &mut FrameTiming,
) {
    let start = Instant::now();
    match encoder.encode_xbgr(pixels, stride, drm_fourcc) {
        Ok((packets, cpu)) => {
            timing.cpu_copy_us = cpu.copy_us;
            timing.cpu_convert_us = cpu.convert_us;
            timing.cpu_codec_us = cpu.codec_us;
            if let Some(data) = concat_packets(packets, &mut timing.concat_us) {
                timing.encoded_size = data.len();
                if let Some(bc) = broadcaster {
                    let publish = Instant::now();
                    bc.publish_frame(source_frame_id, data);
                    timing.broadcast_us = publish.elapsed().as_micros();
                }
            }
        }
        Err(e) => log::warn!("H.264 encode error: {e}"),
    }
    timing.encode_us = start.elapsed().as_micros();
}

fn concat_packets(mut packets: Vec<Vec<u8>>, concat_us: &mut u128) -> Option<Vec<u8>> {
    if packets.is_empty() {
        return None;
    }
    // Encoders usually emit one AVPacket per access unit — move it instead
    // of copying the whole frame into a fresh buffer.
    if packets.len() == 1 {
        return Some(packets.pop().unwrap());
    }
    let t = Instant::now();
    let total: usize = packets.iter().map(|p| p.len()).sum();
    let mut buf = Vec::with_capacity(total);
    for pkt in &packets {
        buf.extend_from_slice(pkt);
    }
    *concat_us += t.elapsed().as_micros();
    Some(buf)
}

#[allow(clippy::too_many_arguments)]
fn encode_xbgr_fallback(
    encoder: &mut VaapiEncoder,
    buf_params: &DrmBufParams,
    width: usize,
    height: usize,
    stride: usize,
    drm_fourcc: u32,
    map_us: &mut u128,
    copy_us: &mut u128,
    convert_us: &mut u128,
    codec_us: &mut u128,
) -> Vec<Vec<u8>> {
    let mut pkts = Vec::new();
    let map_start = Instant::now();
    let mut callback_us = 0;
    let _ = buf_params.bo.map(0, 0, width as u32, height as u32, |mapped_bo| {
        let callback_start = Instant::now();
        match encoder.encode_xbgr(mapped_bo.buffer(), stride, drm_fourcc) {
            Ok((p, timing)) => {
                pkts = p;
                *copy_us = timing.copy_us;
                *convert_us = timing.convert_us;
                *codec_us = timing.codec_us;
            }
            Err(e) => log::warn!("H.264 encode error: {}", e),
        }
        callback_us = callback_start.elapsed().as_micros();
    });
    // GBM's map callback encloses the actual frame work. Subtract it so this
    // counter reports only map setup/synchronization and teardown overhead.
    *map_us = map_start.elapsed().as_micros().saturating_sub(callback_us);
    pkts
}

#[allow(clippy::too_many_arguments)]
fn capture_preview_frame(
    buf_params: &DrmBufParams,
    width: usize,
    height: usize,
    stride: usize,
    frame_num: u64,
    frame_buffer: &Arc<Mutex<Option<SharedFrameBuffer>>>,
) {
    let scale = (width as f32 / 720.0).max(height as f32 / 450.0).max(1.0);
    let preview_width = ((width as f32 / scale).round() as usize).max(1);
    let preview_height = ((height as f32 / scale).round() as usize).max(1);

    let rgba_data = buf_params.bo.map(0, 0, width as u32, height as u32, |mapped_bo| {
        let buffer = mapped_bo.buffer();

        let t_convert = Instant::now();
        let mut rgba_data = Vec::with_capacity(preview_width * preview_height * 4);
        xbgr_to_rgba_scaled(
            buffer,
            stride,
            width,
            height,
            preview_width,
            preview_height,
            &mut rgba_data,
        );
        let convert_us = t_convert.elapsed().as_micros();

        if frame_num.is_multiple_of(30) {
            log::debug!(
                "[perf] frame={} inside_map(rgba_only): convert={}us buf_size={}",
                frame_num,
                convert_us,
                buffer.len(),
            );
        }

        rgba_data
    });

    if let Ok(rgba_data) = rgba_data {
        *frame_buffer.lock().unwrap() = Some(SharedFrameBuffer {
            sequence: frame_num,
            width: preview_width as u32,
            height: preview_height as u32,
            stride: (preview_width * 4) as u32,
            pixel_data: rgba_data,
        });
    }
}

fn xbgr_to_rgba(
    xbgr_buffer: &[u8],
    stride: usize,
    width: usize,
    height: usize,
    rgba_out: &mut Vec<u8>,
) {
    rgba_out.reserve(width * height * 4);
    for y in 0..height {
        let row_start = y * stride;
        for x in 0..width {
            let i = row_start + (x * 4);
            if i + 3 < xbgr_buffer.len() {
                rgba_out.push(xbgr_buffer[i + 2]); // R
                rgba_out.push(xbgr_buffer[i + 1]); // G
                rgba_out.push(xbgr_buffer[i]); // B
                rgba_out.push(255); // A
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn xbgr_to_rgba_scaled(
    xbgr_buffer: &[u8],
    stride: usize,
    source_width: usize,
    source_height: usize,
    target_width: usize,
    target_height: usize,
    rgba_out: &mut Vec<u8>,
) {
    if source_width == target_width && source_height == target_height {
        xbgr_to_rgba(xbgr_buffer, stride, source_width, source_height, rgba_out);
        return;
    }
    for y in 0..target_height {
        let source_y = y * source_height / target_height;
        let row_start = source_y * stride;
        for x in 0..target_width {
            let source_x = x * source_width / target_width;
            let index = row_start + source_x * 4;
            if index + 3 < xbgr_buffer.len() {
                rgba_out.extend_from_slice(&[
                    xbgr_buffer[index + 2],
                    xbgr_buffer[index + 1],
                    xbgr_buffer[index],
                    255,
                ]);
            }
        }
    }
}

/// Send `Unavailable` to GUI (for the status banner) and an error
/// stream_status + empty output list to devices.
fn send_unavailable(tx: &Sender<AnchorEvent>, reason: &str) {
    let r = reason.to_string();
    tx.send(AnchorEvent {
        target: AnchorTarget::Gui,
        message: AnchorMessage::Wayland(WaylandPluginMsg::Unavailable(r.clone())),
    })
    .ok();
    tx.send(AnchorEvent {
        target: AnchorTarget::Device,
        message: AnchorMessage::Json(
            json!({
                "plugin_id": "video",
                "type": "stream_status",
                "state": "error",
                "error": r,
                "selected_index": 0,
            })
            .to_string(),
        ),
    })
    .ok();
    // Send empty output list so the GUI doesn't show stale data.
    send_output_list(tx, &[]);
}

#[cfg(test)]
mod tests {
    use super::WaylandPluginMsg;
    use super::*;
    use crate::anchorapp::event::{AnchorEvent, AnchorMessage, AnchorTarget};
    use crate::anchorwayland::capture_backend::{CaptureBackend, CapturedFrame, OutputInfo};
    use std::sync::mpsc;

    #[test]
    fn deadline_limiter_caps_120hz_capture_to_60fps() {
        let start = Instant::now();
        let mut deadline = None;
        let encoded = (0..120)
            .filter(|frame| {
                should_encode_at(
                    &mut deadline,
                    start + Duration::from_secs_f64(*frame as f64 / 120.0),
                    60,
                )
            })
            .count();
        assert!((59..=61).contains(&encoded), "encoded {encoded} frames");
    }

    #[test]
    fn deadline_limiter_is_disabled_at_zero_fps() {
        let mut deadline = None;
        let now = Instant::now();
        assert!(should_encode_at(&mut deadline, now, 0));
        assert!(should_encode_at(&mut deadline, now, 0));
    }

    // ── Shared helpers ────────────────────────────────────────────────────────

    struct MockBackend {
        outputs: Vec<OutputInfo>,
    }

    impl MockBackend {
        fn new(outputs: Vec<OutputInfo>) -> Self {
            Self { outputs }
        }
    }

    impl CaptureBackend for MockBackend {
        fn init(&mut self) -> Result<(), String> {
            Ok(())
        }
        fn capture_frame(&mut self, _output_index: usize) -> Result<CapturedFrame, String> {
            Err("mock".into())
        }
        fn get_outputs(&self) -> Vec<OutputInfo> {
            self.outputs.clone()
        }
        fn supports_zero_copy_encoding(&self) -> bool {
            false
        }
        fn shutdown(&mut self) {}
    }

    fn make_output(id: usize, name: &str, w: u32, h: u32) -> OutputInfo {
        OutputInfo {
            id,
            name: name.to_string(),
            description: String::new(),
            width: w,
            height: h,
            refresh_rate_mhz: 60000,
            x: 0,
            y: 0,
        }
    }

    fn ordered_topologies(outputs: &[OutputInfo]) -> Vec<Vec<OutputInfo>> {
        fn append_permutations(
            prefix: &mut Vec<OutputInfo>,
            remaining: &mut Vec<OutputInfo>,
            result: &mut Vec<Vec<OutputInfo>>,
        ) {
            result.push(prefix.clone());
            for index in 0..remaining.len() {
                let output = remaining.remove(index);
                prefix.push(output.clone());
                append_permutations(prefix, remaining, result);
                prefix.pop();
                remaining.insert(index, output);
            }
        }

        let mut result = Vec::new();
        append_permutations(&mut Vec::new(), &mut outputs.to_vec(), &mut result);
        result
    }

    /// Build a CaptureState with no real GPU encoder.
    /// `streaming` controls whether the state machine considers itself live.
    fn make_state(streaming: bool, selected_output_index: usize) -> CaptureState {
        CaptureState {
            streaming,
            h264_encoder: None,
            vaapi_device_ctx: None,
            selected_output_index,
            selected_output_name: String::new(),
            output_x: 0,
            output_y: 0,
            iters: 1,
            encoded_frames: 0,
            stats_encoded_frames: 0,
            stats_started_at: Instant::now(),
            next_encode_deadline: None,
            backend: Box::new(MockBackend::new(vec![])),
            last_encoded_frame_time: None,
            last_forced_idr: None,
            frame_drops: 0,
            total_frame_drops: 0,
            trace_ring: Arc::new(TraceRing::new()),
            wayland_tab_visible: true,
        }
    }

    /// Inject a single message into a one-shot channel and call poll_messages.
    fn poll_one(msg: AnchorMessage, state: &CaptureState) -> CaptureAction {
        let (tx, rx) = mpsc::channel::<AnchorEvent>();
        tx.send(AnchorEvent { target: AnchorTarget::Wayland, message: msg }).unwrap();
        poll_messages(&rx, state)
    }

    fn json_cmd(cmd: &str) -> AnchorMessage {
        AnchorMessage::Json(format!(r#"{{"plugin_id":"wayland","command":"{}"}}"#, cmd))
    }

    fn json_cmd_idx(cmd: &str, idx: usize) -> AnchorMessage {
        AnchorMessage::Json(format!(
            r#"{{"plugin_id":"wayland","command":"{}","index":{}}}"#,
            cmd, idx
        ))
    }

    fn json_type(t: &str) -> AnchorMessage {
        AnchorMessage::Json(format!(r#"{{"type":"{}"}}"#, t))
    }

    // ── poll_messages: empty channel ─────────────────────────────────────────

    #[test]
    fn poll_empty_channel_returns_continue() {
        let (_tx, rx) = mpsc::channel::<AnchorEvent>();
        let state = make_state(false, 0);
        assert!(matches!(poll_messages(&rx, &state), CaptureAction::Continue));
    }

    // ── poll_messages: Stop ───────────────────────────────────────────────────

    #[test]
    fn poll_wayland_stop_returns_stop() {
        let state = make_state(true, 0);
        assert!(matches!(
            poll_one(AnchorMessage::Wayland(WaylandPluginMsg::Stop), &state),
            CaptureAction::Stop
        ));
    }

    #[test]
    fn poll_wayland_stop_while_not_streaming_returns_stop() {
        let state = make_state(false, 0);
        assert!(matches!(
            poll_one(AnchorMessage::Wayland(WaylandPluginMsg::Stop), &state),
            CaptureAction::Stop
        ));
    }

    #[test]
    fn poll_json_stop_returns_stop() {
        let state = make_state(true, 0);
        assert!(matches!(poll_one(json_cmd("stop"), &state), CaptureAction::Stop));
    }

    #[test]
    fn poll_json_stop_while_not_streaming_returns_stop() {
        let state = make_state(false, 0);
        assert!(matches!(poll_one(json_cmd("stop"), &state), CaptureAction::Stop));
    }

    // ── poll_messages: SelectOutput ───────────────────────────────────────────

    #[test]
    fn poll_wayland_select_output_returns_select() {
        let state = make_state(true, 0);
        let action = poll_one(AnchorMessage::Wayland(WaylandPluginMsg::SelectOutput(2)), &state);
        assert!(matches!(action, CaptureAction::SelectOutput(2)));
    }

    #[test]
    fn poll_json_select_output_returns_select() {
        let state = make_state(true, 0);
        let action = poll_one(json_cmd_idx("select_output", 1), &state);
        assert!(matches!(action, CaptureAction::SelectOutput(1)));
    }

    #[test]
    fn poll_json_select_output_index_zero() {
        let state = make_state(false, 1);
        let action = poll_one(json_cmd_idx("select_output", 0), &state);
        assert!(matches!(action, CaptureAction::SelectOutput(0)));
    }

    // ── poll_messages: RequestKeyframe ────────────────────────────────────────

    #[test]
    fn poll_keyframe_while_streaming_returns_keyframe() {
        let state = make_state(true, 0);
        assert!(matches!(
            poll_one(json_cmd("request_keyframe"), &state),
            CaptureAction::RequestKeyframe
        ));
    }

    #[test]
    fn poll_keyframe_while_not_streaming_returns_continue() {
        let state = make_state(false, 0);
        assert!(matches!(poll_one(json_cmd("request_keyframe"), &state), CaptureAction::Continue));
    }

    // ── poll_messages: ResyncToPhone ─────────────────────────────────────────

    #[test]
    fn poll_wayland_start_while_streaming_returns_resync() {
        let state = make_state(true, 0);
        assert!(matches!(
            poll_one(AnchorMessage::Wayland(WaylandPluginMsg::Start), &state),
            CaptureAction::ResyncToPhone
        ));
    }

    #[test]
    fn poll_wayland_start_while_not_streaming_returns_continue() {
        // Encoder not yet ready (starting phase) — ignore duplicate Start.
        let state = make_state(false, 0);
        assert!(matches!(
            poll_one(AnchorMessage::Wayland(WaylandPluginMsg::Start), &state),
            CaptureAction::Continue
        ));
    }

    #[test]
    fn poll_json_start_while_streaming_returns_resync() {
        let state = make_state(true, 0);
        assert!(matches!(poll_one(json_cmd("start"), &state), CaptureAction::ResyncToPhone));
    }

    #[test]
    fn poll_json_start_while_not_streaming_returns_continue() {
        let state = make_state(false, 0);
        assert!(matches!(poll_one(json_cmd("start"), &state), CaptureAction::Continue));
    }

    #[test]
    fn poll_device_connected_while_streaming_returns_resync() {
        let state = make_state(true, 0);
        assert!(matches!(
            poll_one(json_type("device_connected"), &state),
            CaptureAction::ResyncToPhone
        ));
    }

    #[test]
    fn poll_device_connected_while_not_streaming_returns_resync() {
        // Even during "starting" phase the phone should know we're in progress.
        let state = make_state(false, 0);
        assert!(matches!(
            poll_one(json_type("device_connected"), &state),
            CaptureAction::ResyncToPhone
        ));
    }

    // ── poll_messages: unknown / ignored messages ─────────────────────────────

    #[test]
    fn poll_unknown_json_command_returns_continue() {
        let state = make_state(true, 0);
        assert!(matches!(poll_one(json_cmd("unknown_cmd"), &state), CaptureAction::Continue));
    }

    #[test]
    fn poll_wayland_ready_returns_continue() {
        let state = make_state(true, 0);
        assert!(matches!(
            poll_one(AnchorMessage::Wayland(WaylandPluginMsg::Ready), &state),
            CaptureAction::Continue
        ));
    }

    #[test]
    fn poll_wayland_output_list_returns_continue() {
        let state = make_state(true, 0);
        let msg = AnchorMessage::Wayland(WaylandPluginMsg::OutputList(vec![make_output(
            0, "eDP-1", 1920, 1200,
        )]));
        assert!(matches!(poll_one(msg, &state), CaptureAction::Continue));
    }

    #[test]
    fn poll_generic_message_returns_continue() {
        let state = make_state(true, 0);
        assert!(matches!(
            poll_one(AnchorMessage::Generic("hello".into()), &state),
            CaptureAction::Continue
        ));
    }

    // ── poll_messages: message priority (first relevant wins) ─────────────────

    #[test]
    fn poll_stop_before_select_output_returns_stop() {
        let (tx, rx) = mpsc::channel::<AnchorEvent>();
        let state = make_state(true, 0);
        tx.send(AnchorEvent {
            target: AnchorTarget::Wayland,
            message: AnchorMessage::Wayland(WaylandPluginMsg::Stop),
        })
        .unwrap();
        tx.send(AnchorEvent {
            target: AnchorTarget::Wayland,
            message: AnchorMessage::Wayland(WaylandPluginMsg::SelectOutput(1)),
        })
        .unwrap();
        assert!(matches!(poll_messages(&rx, &state), CaptureAction::Stop));
    }

    #[test]
    fn poll_resync_before_stop_returns_resync() {
        let (tx, rx) = mpsc::channel::<AnchorEvent>();
        let state = make_state(true, 0);
        tx.send(AnchorEvent {
            target: AnchorTarget::Wayland,
            message: AnchorMessage::Wayland(WaylandPluginMsg::Start),
        })
        .unwrap();
        tx.send(AnchorEvent {
            target: AnchorTarget::Wayland,
            message: AnchorMessage::Wayland(WaylandPluginMsg::Stop),
        })
        .unwrap();
        assert!(matches!(poll_messages(&rx, &state), CaptureAction::ResyncToPhone));
    }

    // ── send_stream_status: JSON structure ────────────────────────────────────

    fn drain(rx: &mpsc::Receiver<AnchorEvent>) -> Vec<AnchorEvent> {
        rx.try_iter().collect()
    }

    fn parse_json(msg: &AnchorMessage) -> serde_json::Value {
        match msg {
            AnchorMessage::Json(s) => serde_json::from_str(s).expect("valid JSON"),
            other => panic!("expected Json, got {:?}", other),
        }
    }

    #[test]
    fn stream_status_sends_to_both_gui_and_device() {
        let (tx, rx) = mpsc::channel::<AnchorEvent>();
        send_stream_status(&tx, "streaming", 0, None);
        let events = drain(&rx);
        assert_eq!(events.len(), 2);
        let targets: Vec<_> = events.iter().map(|e| &e.target).collect();
        assert!(targets.iter().any(|t| matches!(t, AnchorTarget::Gui)));
        assert!(targets.iter().any(|t| matches!(t, AnchorTarget::Device)));
    }

    fn assert_stream_status(state: &str, selected_index: usize, error: Option<&str>) {
        let (tx, rx) = mpsc::channel::<AnchorEvent>();
        send_stream_status(&tx, state, selected_index, error);
        for event in rx.try_iter() {
            let j = parse_json(&event.message);
            assert_eq!(j["selected_index"], selected_index);
            if let Some(err) = error {
                assert_eq!(j["error"], err);
            } else {
                assert!(j.get("error").is_none(), "unexpected error field");
            }
        }
    }

    #[test]
    fn stream_status_idle_no_error() {
        assert_stream_status("idle", 0, None);
    }

    #[test]
    fn stream_status_starting_no_error() {
        assert_stream_status("starting", 0, None);
    }

    #[test]
    fn stream_status_streaming_with_selected_index() {
        assert_stream_status("streaming", 1, None);
    }

    #[test]
    fn stream_status_switching_no_error() {
        assert_stream_status("switching", 2, None);
    }

    #[test]
    fn stream_status_stopping_no_error() {
        assert_stream_status("stopping", 0, None);
    }

    #[test]
    fn stream_status_error_includes_error_field() {
        assert_stream_status("error", 0, Some("Encoder init failed: VAAPI unavailable"));
    }

    #[test]
    fn stream_status_gui_message_has_capturing_state_type() {
        let (tx, rx) = mpsc::channel::<AnchorEvent>();
        send_stream_status(&tx, "streaming", 0, None);
        let gui_event =
            rx.try_iter().find(|e| matches!(e.target, AnchorTarget::Gui)).expect("no GUI event");
        let j = parse_json(&gui_event.message);
        assert_eq!(j["type"], "capturing_state");
        assert_eq!(j["state"], "streaming");
    }

    #[test]
    fn stream_status_device_message_has_stream_status_type_and_plugin_id() {
        let (tx, rx) = mpsc::channel::<AnchorEvent>();
        send_stream_status(&tx, "idle", 0, None);
        let dev_event = rx
            .try_iter()
            .find(|e| matches!(e.target, AnchorTarget::Device))
            .expect("no Device event");
        let j = parse_json(&dev_event.message);
        assert_eq!(j["type"], "stream_status");
        assert_eq!(j["plugin_id"], "video");
        assert_eq!(j["state"], "idle");
    }

    // ── send_output_list: JSON structure ──────────────────────────────────────

    #[test]
    fn output_list_device_json_includes_id_field() {
        let (tx, rx) = mpsc::channel::<AnchorEvent>();
        let outputs = vec![make_output(7, "eDP-1", 1920, 1200)];
        send_output_list(&tx, &outputs);
        let dev_event = rx
            .try_iter()
            .find(|e| matches!(e.target, AnchorTarget::Device))
            .expect("no Device event");
        let j = parse_json(&dev_event.message);
        assert_eq!(j["type"], "output_list");
        assert_eq!(j["outputs"][0]["id"], 7);
        assert_eq!(j["outputs"][0]["name"], "eDP-1");
        assert_eq!(j["outputs"][0]["width"], 1920);
        assert_eq!(j["outputs"][0]["height"], 1200);
    }

    #[test]
    fn output_list_gui_gets_wayland_plugin_msg() {
        let (tx, rx) = mpsc::channel::<AnchorEvent>();
        let outputs = vec![make_output(0, "eDP-1", 1920, 1200)];
        send_output_list(&tx, &outputs);
        let gui_event =
            rx.try_iter().find(|e| matches!(e.target, AnchorTarget::Gui)).expect("no GUI event");
        match gui_event.message {
            AnchorMessage::Wayland(WaylandPluginMsg::OutputList(list)) => {
                assert_eq!(list.len(), 1);
                assert_eq!(list[0].name, "eDP-1");
            }
            other => panic!("expected WaylandPluginMsg::OutputList, got {:?}", other),
        }
    }

    #[test]
    fn output_list_empty_sends_gui_input_and_device_updates() {
        let (tx, rx) = mpsc::channel::<AnchorEvent>();
        send_output_list(&tx, &[]);
        let events = drain(&rx);
        assert_eq!(events.len(), 3);
        let gui = events.iter().find(|e| matches!(e.target, AnchorTarget::Gui)).unwrap();
        match &gui.message {
            AnchorMessage::Wayland(WaylandPluginMsg::OutputList(list)) => {
                assert!(list.is_empty())
            }
            other => panic!("unexpected: {:?}", other),
        }

        let topology = events
            .iter()
            .find(|event| matches!(event.target, AnchorTarget::Broadcast))
            .expect("input topology update missing");
        let topology_json = parse_json(&topology.message);
        assert_eq!(topology_json["type"], "output_layout");
        assert_eq!(topology_json["outputs"], serde_json::json!([]));
    }

    #[test]
    fn output_list_multiple_outputs_all_have_ids() {
        let (tx, rx) = mpsc::channel::<AnchorEvent>();
        let outputs =
            vec![make_output(0, "eDP-1", 1920, 1200), make_output(1, "HDMI-A-1", 2560, 1440)];
        send_output_list(&tx, &outputs);
        let dev_event = rx.try_iter().find(|e| matches!(e.target, AnchorTarget::Device)).unwrap();
        let j = parse_json(&dev_event.message);
        assert_eq!(j["outputs"][0]["id"], 0);
        assert_eq!(j["outputs"][1]["id"], 1);
        assert_eq!(j["outputs"][1]["name"], "HDMI-A-1");
    }

    #[test]
    fn output_layout_broadcast_includes_geometry_used_by_absolute_input() {
        let (tx, rx) = mpsc::channel::<AnchorEvent>();
        let mut output = make_output(4, "HEADLESS-1", 1080, 2340);
        output.x = -1080;
        output.y = 72;

        send_output_list(&tx, &[output]);

        let event = rx
            .try_iter()
            .find(|event| matches!(event.target, AnchorTarget::Broadcast))
            .expect("input topology update missing");
        let json = parse_json(&event.message);
        assert_eq!(json["outputs"][0]["name"], "HEADLESS-1");
        assert_eq!(json["outputs"][0]["x"], -1080);
        assert_eq!(json["outputs"][0]["y"], 72);
        assert_eq!(json["outputs"][0]["width"], 1080);
        assert_eq!(json["outputs"][0]["height"], 2340);
    }

    // ── Scenario: phone reconnects while streaming ────────────────────────────

    #[test]
    fn scenario_reconnect_during_stream_via_device_connected() {
        // Desktop is streaming; device_connected arrives → ResyncToPhone
        let state = make_state(true, 0);
        assert!(matches!(
            poll_one(json_type("device_connected"), &state),
            CaptureAction::ResyncToPhone
        ));
    }

    #[test]
    fn scenario_reconnect_during_stream_via_start_command() {
        // Phone presses Start while desktop is already streaming → ResyncToPhone
        let state = make_state(true, 0);
        assert!(matches!(poll_one(json_cmd("start"), &state), CaptureAction::ResyncToPhone));
    }

    #[test]
    fn scenario_reconnect_during_starting_phase() {
        // device_connected arrives while encoder is being set up → still resync
        let state = make_state(false, 0);
        assert!(matches!(
            poll_one(json_type("device_connected"), &state),
            CaptureAction::ResyncToPhone
        ));
    }

    #[test]
    fn scenario_stop_then_start_is_not_resync() {
        // After Stop the streaming flag resets, so a new Start is not a resync
        let (tx, rx) = mpsc::channel::<AnchorEvent>();
        let state = make_state(false, 0); // streaming=false simulates post-stop idle
        tx.send(AnchorEvent {
            target: AnchorTarget::Wayland,
            message: AnchorMessage::Wayland(WaylandPluginMsg::Start),
        })
        .unwrap();
        // Should be Continue (engine is idle — idle loop handles Start, not poll_messages)
        assert!(matches!(poll_messages(&rx, &state), CaptureAction::Continue));
    }

    #[test]
    fn scenario_select_output_mid_stream_carries_correct_index() {
        let state = make_state(true, 0);
        let action = poll_one(json_cmd_idx("select_output", 3), &state);
        assert!(matches!(action, CaptureAction::SelectOutput(3)));
    }

    // ── Output list change detection ──────────────────────────────────────────

    #[test]
    fn output_list_comparison_detects_add() {
        let old = vec![make_output(0, "eDP-1", 1920, 1200)];
        let new = vec![make_output(0, "eDP-1", 1920, 1200), make_output(1, "HDMI-1", 2560, 1440)];
        assert_ne!(old, new);
    }

    #[test]
    fn output_list_comparison_detects_removal() {
        let old = vec![make_output(0, "eDP-1", 1920, 1200), make_output(1, "HDMI-1", 2560, 1440)];
        let new = vec![make_output(0, "eDP-1", 1920, 1200)];
        assert_ne!(old, new);
    }

    #[test]
    fn output_list_comparison_detects_resize() {
        let old = vec![make_output(0, "eDP-1", 1920, 1200)];
        let new = vec![make_output(0, "eDP-1", 1920, 1080)];
        assert_ne!(old, new);
    }

    #[test]
    fn output_list_comparison_same_is_equal() {
        let a = vec![make_output(0, "eDP-1", 1920, 1200), make_output(1, "HDMI-1", 2560, 1440)];
        let b = a.clone();
        assert_eq!(a, b);
    }

    #[test]
    fn removal_fallback_skips_the_output_being_removed() {
        let outputs =
            vec![make_output(0, "HEADLESS-1", 1080, 2340), make_output(1, "eDP-1", 1920, 1200)];

        assert_eq!(fallback_output_index(&outputs, "HEADLESS-1"), Some(1));
    }

    #[test]
    fn removal_fallback_rejects_removing_the_only_output() {
        let outputs = vec![make_output(0, "HEADLESS-1", 1080, 2340)];

        assert_eq!(fallback_output_index(&outputs, "HEADLESS-1"), None);
    }

    #[test]
    fn topology_reorder_preserves_selection_by_output_name() {
        let outputs =
            vec![make_output(0, "HEADLESS-1", 1080, 2340), make_output(1, "eDP-1", 1920, 1200)];
        let mut index = 0;
        let mut name = "eDP-1".to_string();

        assert!(reconcile_output_selection(&outputs, &mut index, &mut name));
        assert_eq!(index, 1);
        assert_eq!(name, "eDP-1");
    }

    #[test]
    fn topology_removal_falls_back_to_a_valid_output() {
        let outputs = vec![make_output(0, "eDP-1", 1920, 1200)];
        let mut index = 1;
        let mut name = "HEADLESS-1".to_string();

        assert!(!reconcile_output_selection(&outputs, &mut index, &mut name));
        assert_eq!(index, 0);
        assert_eq!(name, "eDP-1");
    }

    #[test]
    fn every_add_remove_reorder_sequence_preserves_selection_or_uses_first_fallback() {
        let universe = vec![
            make_output(0, "eDP-1", 1920, 1200),
            make_output(1, "HEADLESS-1", 1080, 2340),
            make_output(2, "HDMI-A-1", 2560, 1440),
            make_output(3, "DP-2", 3840, 2160),
        ];
        let topologies = ordered_topologies(&universe);

        for selected in universe.iter().map(|output| output.name.as_str()).chain(["REMOVED-1"]) {
            for outputs in &topologies {
                let mut index = usize::MAX;
                let mut name = selected.to_string();
                let preserved = reconcile_output_selection(outputs, &mut index, &mut name);
                let expected = outputs.iter().position(|output| output.name == selected);

                match expected {
                    Some(expected_index) => {
                        assert!(preserved);
                        assert_eq!(index, expected_index);
                        assert_eq!(name, selected);
                    }
                    None if outputs.is_empty() => {
                        assert!(!preserved);
                        assert_eq!(index, 0);
                        assert!(name.is_empty());
                    }
                    None => {
                        assert!(!preserved);
                        assert_eq!(index, 0);
                        assert_eq!(name, outputs[0].name);
                    }
                }

                let reconciled_name = name.clone();
                let reconciled_index = index;
                let second_result = reconcile_output_selection(outputs, &mut index, &mut name);
                assert_eq!((index, name.as_str()), (reconciled_index, reconciled_name.as_str()));
                assert_eq!(second_result, !outputs.is_empty());
            }
        }
    }

    #[test]
    fn fallback_never_selects_the_output_being_deleted() {
        let outputs = vec![
            make_output(0, "eDP-1", 1920, 1200),
            make_output(1, "HEADLESS-1", 1080, 2340),
            make_output(2, "HDMI-A-1", 2560, 1440),
        ];

        for removed in &outputs {
            let index = fallback_output_index(&outputs, &removed.name).expect("fallback required");
            assert!(index < outputs.len());
            assert_ne!(outputs[index].name, removed.name);
        }
    }

    #[test]
    fn stale_device_selection_after_deletion_leaves_valid_stream_unchanged() {
        let outputs = vec![make_output(0, "eDP-1", 1920, 1200)];
        let mut state = make_state(true, 0);
        state.selected_output_name = "eDP-1".into();
        state.output_x = -20;
        state.output_y = 30;

        assert!(!try_apply_output_selection(&mut state, &outputs, 1));
        assert!(state.streaming);
        assert_eq!(state.selected_output_index, 0);
        assert_eq!(state.selected_output_name, "eDP-1");
        assert_eq!((state.output_x, state.output_y), (-20, 30));
    }

    #[test]
    fn valid_selection_updates_stable_name_and_current_geometry_together() {
        let mut first = make_output(0, "eDP-1", 1920, 1200);
        first.x = -1920;
        let mut second = make_output(1, "HEADLESS-1", 1080, 2340);
        second.x = 0;
        second.y = 100;
        let outputs = vec![first, second];
        let mut state = make_state(true, 0);

        assert!(try_apply_output_selection(&mut state, &outputs, 1));
        assert_eq!(state.selected_output_index, 1);
        assert_eq!(state.selected_output_name, "HEADLESS-1");
        assert_eq!((state.output_x, state.output_y), (0, 100));
    }

    #[test]
    fn removed_stream_selection_resets_to_a_valid_idle_target() {
        let outputs = vec![make_output(0, "eDP-1", 1920, 1200)];
        let mut state = make_state(true, 1);
        state.selected_output_name = "HEADLESS-1".into();

        apply_output_selection(&mut state, &outputs, 0);

        assert_eq!(state.selected_output_index, 0);
        assert_eq!(state.selected_output_name, "eDP-1");
    }

    #[test]
    fn check_output_changes_sends_on_diff() {
        let (tx, rx) = mpsc::channel::<AnchorEvent>();
        let mut backend: Box<dyn CaptureBackend> = Box::new(MockBackend::new(vec![
            make_output(0, "eDP-1", 1920, 1200),
            make_output(1, "NEW", 1280, 720),
        ]));
        let mut last = vec![make_output(0, "eDP-1", 1920, 1200)];

        check_output_changes(&mut backend, &mut last, &tx, None);

        let event = rx.try_recv().expect("expected OutputList event");
        match event.message {
            AnchorMessage::Wayland(WaylandPluginMsg::OutputList(outputs)) => {
                assert_eq!(outputs.len(), 2);
                assert_eq!(outputs[1].name, "NEW");
            }
            _ => panic!("expected OutputList message"),
        }

        assert_eq!(last.len(), 2);
    }

    #[test]
    fn check_output_changes_no_send_when_same() {
        let (tx, rx) = mpsc::channel::<AnchorEvent>();
        let outputs = vec![make_output(0, "eDP-1", 1920, 1200)];
        let mut backend: Box<dyn CaptureBackend> = Box::new(MockBackend::new(outputs.clone()));
        let mut last = outputs;

        check_output_changes(&mut backend, &mut last, &tx, None);

        assert!(rx.try_recv().is_err(), "should not send when unchanged");
    }
}
