use wayland_client::protocol::wl_buffer::{self, WlBuffer};
use wayland_client::protocol::wl_output::{self, WlOutput};
use wayland_client::protocol::wl_shm::{self, WlShm};
use wayland_client::protocol::wl_shm_pool::{self, WlShmPool};
use wayland_client::{Connection, Dispatch, QueueHandle, protocol::wl_registry};
use wayland_protocols::wp::linux_dmabuf::zv1::client::zwp_linux_buffer_params_v1::{
    self, ZwpLinuxBufferParamsV1,
};
use wayland_protocols::wp::linux_dmabuf::zv1::client::zwp_linux_dmabuf_v1::{
    self, ZwpLinuxDmabufV1,
};
use wayland_protocols_wlr::screencopy::v1::client::zwlr_screencopy_frame_v1::{
    self, ZwlrScreencopyFrameV1,
};
use wayland_protocols_wlr::screencopy::v1::client::zwlr_screencopy_manager_v1;
use wayland_protocols_wlr::screencopy::v1::client::zwlr_screencopy_manager_v1::ZwlrScreencopyManagerV1;

use crate::anchorwayland::wayland_objects::{AnchorState, BufferState, WloutputInfo};

impl Dispatch<wl_registry::WlRegistry, ()> for AnchorState {
    fn event(
        state: &mut Self,
        registry: &wl_registry::WlRegistry,
        event: wl_registry::Event,
        _: &(),
        _: &Connection,
        qh: &QueueHandle<AnchorState>,
    ) {
        match event {
            wl_registry::Event::Global { name, interface, version } => match &interface[..] {
                "zwlr_screencopy_manager_v1" => {
                    log::debug!("Found screencopy manager");
                    state.zwlr_screenshot_manager_v1 =
                        Some(registry.bind::<ZwlrScreencopyManagerV1, _, _>(name, version, qh, ()))
                }
                "wl_output" => {
                    log::debug!("Binding output {name} with version {version}");
                    registry.bind::<WlOutput, _, _>(name, version, qh, name);
                }
                "zwp_linux_dmabuf_v1" => {
                    log::debug!("Received dmabufv1");
                    state.zwlr_linux_dmabuf =
                        Some(registry.bind::<ZwpLinuxDmabufV1, _, _>(name, version, qh, ()));
                }
                "wl_shm" => {
                    state.wl_shm = Some(registry.bind::<WlShm, _, _>(name, version.min(1), qh, ()))
                }
                _ => {}
            },
            wl_registry::Event::GlobalRemove { name } => {
                let before = state.wl_outputs.len();
                state.wl_outputs.retain(|o| o.global_name != name);
                if state.wl_outputs.len() != before {
                    log::info!(
                        "Output removed (global {}), {} remaining",
                        name,
                        state.wl_outputs.len()
                    );
                }
            }
            _ => {}
        }
    }
}

impl Dispatch<WlShm, ()> for AnchorState {
    fn event(
        _state: &mut Self,
        _proxy: &WlShm,
        _event: wl_shm::Event,
        _data: &(),
        _conn: &Connection,
        _qhandle: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<WlShmPool, ()> for AnchorState {
    fn event(
        _state: &mut Self,
        _proxy: &WlShmPool,
        _event: wl_shm_pool::Event,
        _data: &(),
        _conn: &Connection,
        _qhandle: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<ZwpLinuxDmabufV1, ()> for AnchorState {
    fn event(
        _state: &mut Self,
        _proxy: &ZwpLinuxDmabufV1,
        _event: zwp_linux_dmabuf_v1::Event,
        _data: &(),
        _conn: &Connection,
        _qhandle: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<ZwlrScreencopyFrameV1, ()> for AnchorState {
    fn event(
        state: &mut Self,
        _proxy: &ZwlrScreencopyFrameV1,
        event: zwlr_screencopy_frame_v1::Event,
        _data: &(),
        _conn: &Connection,
        _qhandle: &QueueHandle<Self>,
    ) {
        match event {
            zwlr_screencopy_frame_v1::Event::Flags { .. } => {}
            zwlr_screencopy_frame_v1::Event::Buffer {
                format: wayland_client::WEnum::Value(format),
                width,
                height,
                stride,
            } => {
                state.shm_params.width = width;
                state.shm_params.height = height;
                state.shm_params.stride = stride;
                state.shm_params.format = Some(format);
            }
            zwlr_screencopy_frame_v1::Event::LinuxDmabuf { format, width, height } => {
                // Compositor tells us what DRM fourcc format it will write to our buffer.
                // Log only when format/size changes to avoid spam.
                if state.dmabuf_params.buffer_format != format
                    || state.dmabuf_params.buffer_width != width
                    || state.dmabuf_params.buffer_height != height
                {
                    log::info!(
                        "[wayland] Compositor buffer format: {}x{}, DRM fourcc=0x{:08x}",
                        width,
                        height,
                        format
                    );
                }
                state.dmabuf_params.buffer_height = height;
                state.dmabuf_params.buffer_width = width;
                state.dmabuf_params.buffer_format = format;
            }
            zwlr_screencopy_frame_v1::Event::Ready { tv_sec_hi, tv_sec_lo, tv_nsec } => {
                let seconds = (u64::from(tv_sec_hi) << 32) | u64::from(tv_sec_lo);
                state.last_presentation_time_ns = seconds
                    .checked_mul(1_000_000_000)
                    .and_then(|value| value.checked_add(u64::from(tv_nsec)));
                state.buf_state = BufferState::READY;
            }
            zwlr_screencopy_frame_v1::Event::BufferDone => {
                state.buf_state = BufferState::PARAMS;
            }
            zwlr_screencopy_frame_v1::Event::Failed => {
                log::warn!("Screencopy frame failed");
                state.buf_state = BufferState::FAILED;
            }

            _ => (),
        }
    }
}

impl Dispatch<ZwlrScreencopyManagerV1, ()> for AnchorState {
    fn event(
        _state: &mut Self,
        _proxy: &ZwlrScreencopyManagerV1,
        _event: zwlr_screencopy_manager_v1::Event,
        _data: &(),
        _conn: &Connection,
        _qhandle: &QueueHandle<Self>,
    ) {
        // Do nothing ...
    }
}

impl Dispatch<ZwpLinuxBufferParamsV1, ()> for AnchorState {
    fn event(
        _state: &mut Self,
        _proxy: &ZwpLinuxBufferParamsV1,
        _event: zwp_linux_buffer_params_v1::Event,
        _data: &(),
        _conn: &Connection,
        _qhandle: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<WlBuffer, ()> for AnchorState {
    fn event(
        _state: &mut Self,
        _proxy: &WlBuffer,
        _event: wl_buffer::Event,
        _data: &(),
        _conn: &Connection,
        _qhandle: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<WlOutput, u32> for AnchorState {
    fn event(
        state: &mut Self,
        proxy: &WlOutput,
        event: wl_output::Event,
        global_name: &u32,
        _conn: &Connection,
        _qhandle: &QueueHandle<Self>,
    ) {
        // Find existing entry by global_name (stable across re-fires)
        let output_res =
            state.wl_outputs.iter_mut().find(|output| output.global_name == *global_name);

        let output = match output_res {
            Some(output) => output,
            None => {
                state.wl_outputs.push(WloutputInfo {
                    output: proxy.clone(),
                    global_name: *global_name,
                    width: 0,
                    height: 0,
                    name: String::new(),
                    description: String::new(),
                    x: 0,
                    y: 0,
                });
                state.wl_outputs.last_mut().unwrap()
            }
        };

        match event {
            wl_output::Event::Name { name } => output.name = name,
            wl_output::Event::Description { description } => output.description = description,
            wl_output::Event::Scale { .. } => (),
            wl_output::Event::Geometry { x, y, .. } => {
                output.x = x;
                output.y = y;
            }
            wl_output::Event::Mode { height, width, .. } => {
                output.width = width;
                output.height = height
            }
            wl_output::Event::Done => {
                log::info!("Found display {:?}", output);
                state.done_roundtrip = true
            }
            _ => (),
        }
    }
}
