//! Wayland input backend.
//!
//! Injects pointer events via wlr-virtual-pointer-unstable-v1 and keyboard
//! events via virtual-keyboard-unstable-v1.

use std::collections::HashMap;
use std::os::unix::io::AsRawFd;

use wayland_client::backend::WaylandError;
use wayland_client::protocol::{wl_output, wl_pointer, wl_registry, wl_seat};
use wayland_client::{Connection, Dispatch, EventQueue, Proxy, QueueHandle, WEnum, delegate_noop};
use wayland_protocols::xdg::xdg_output::zv1::client::{zxdg_output_manager_v1, zxdg_output_v1};
use wayland_protocols_misc::zwp_virtual_keyboard_v1::client::{
    zwp_virtual_keyboard_manager_v1, zwp_virtual_keyboard_v1,
};
use wayland_protocols_wlr::virtual_pointer::v1::client::{
    zwlr_virtual_pointer_manager_v1, zwlr_virtual_pointer_v1,
};

use super::{InputBackend, OutputGeometry};

const MOD_SHIFT: u32 = 1;

/// Per-output info collected from Wayland events.
#[derive(Debug, Clone, Default)]
struct OutputInfo {
    name: String,
    x: i32,
    y: i32,
    width: u32,
    height: u32,
}

struct InputWaylandState {
    seat: Option<wl_seat::WlSeat>,
    pointer_manager: Option<zwlr_virtual_pointer_manager_v1::ZwlrVirtualPointerManagerV1>,
    keyboard_manager: Option<zwp_virtual_keyboard_manager_v1::ZwpVirtualKeyboardManagerV1>,
    xdg_output_manager: Option<zxdg_output_manager_v1::ZxdgOutputManagerV1>,
    /// All outputs, keyed by wl_output object id.
    outputs: HashMap<u32, OutputInfo>,
    /// Bound output objects, retained so virtual pointers can target them.
    output_objects: HashMap<u32, wl_output::WlOutput>,
    /// Registry global name → wl_output object id, used to remove stale
    /// geometry when an output is unplugged and recreated.
    output_globals: HashMap<u32, u32>,
    /// Map from xdg_output object id → wl_output object id, for routing xdg events.
    xdg_to_wl: HashMap<u32, u32>,
    /// Outputs currently receiving geometry/mode/name updates, keyed by
    /// wl_output object id. Updates for different displays can interleave.
    pending_outputs: HashMap<u32, OutputInfo>,
    /// The streamed output's geometry in global compositor coords.
    stream_output_x: i32,
    stream_output_y: i32,
    stream_output_w: u32,
    stream_output_h: u32,
    /// Bounding box of the compositor layout. Wayland output coordinates may
    /// be negative when an output is left of or above the primary output.
    layout_min_x: i32,
    layout_min_y: i32,
    total_w: u32,
    total_h: u32,
}

/// Wayland-based pointer + keyboard injection.
pub struct WaylandInput {
    conn: Connection,
    /// Keeps the input connection subscribed to runtime xdg-output changes.
    /// Without this queue, positions are only the values observed at startup.
    event_queue: EventQueue<InputWaylandState>,
    wayland_state: InputWaylandState,
    qh: QueueHandle<InputWaylandState>,
    /// Unbound fallback for compositors that do not expose virtual-pointer v2.
    generic_virtual_pointer: zwlr_virtual_pointer_v1::ZwlrVirtualPointerV1,
    virtual_pointer: zwlr_virtual_pointer_v1::ZwlrVirtualPointerV1,
    /// Output-bound virtual pointers keyed by stable Wayland output name.
    output_virtual_pointers: HashMap<String, zwlr_virtual_pointer_v1::ZwlrVirtualPointerV1>,
    /// True only when `virtual_pointer` is constrained to the streamed output.
    pointer_is_output_bound: bool,
    virtual_keyboard: Option<zwp_virtual_keyboard_v1::ZwpVirtualKeyboardV1>,
    char_to_keycode: HashMap<char, (u32, bool)>,
    outputs: HashMap<u32, OutputInfo>,
    /// Last topology published by the capture worker. It supplies output
    /// membership, while the local Wayland connection supplies logical geometry.
    reported_outputs: Option<Vec<OutputGeometry>>,
    stream_output_name: String,
    stream_output_x: i32,
    stream_output_y: i32,
    stream_output_w: u32,
    stream_output_h: u32,
    layout_min_x: i32,
    layout_min_y: i32,
    total_w: u32,
    total_h: u32,
}

fn output_layout_bounds<'a>(outputs: impl Iterator<Item = &'a OutputInfo>) -> (i32, i32, u32, u32) {
    let mut min_x = i64::MAX;
    let mut min_y = i64::MAX;
    let mut max_x = i64::MIN;
    let mut max_y = i64::MIN;
    let mut found = false;

    for output in outputs {
        found = true;
        min_x = min_x.min(i64::from(output.x));
        min_y = min_y.min(i64::from(output.y));
        max_x = max_x.max(i64::from(output.x) + i64::from(output.width));
        max_y = max_y.max(i64::from(output.y) + i64::from(output.height));
    }

    if !found {
        return (0, 0, 1920, 1080);
    }

    (
        min_x.clamp(i64::from(i32::MIN), i64::from(i32::MAX)) as i32,
        min_y.clamp(i64::from(i32::MIN), i64::from(i32::MAX)) as i32,
        (max_x - min_x).clamp(1, i64::from(u32::MAX)) as u32,
        (max_y - min_y).clamp(1, i64::from(u32::MAX)) as u32,
    )
}

#[allow(clippy::too_many_arguments)]
fn absolute_position_in_layout(
    layout_min_x: i32,
    layout_min_y: i32,
    total_w: u32,
    total_h: u32,
    output_x: i32,
    output_y: i32,
    output_w: u32,
    output_h: u32,
    x_norm: f64,
    y_norm: f64,
) -> (u32, u32) {
    let x = x_norm.clamp(0.0, 1.0);
    let y = y_norm.clamp(0.0, 1.0);
    let translated_x = (f64::from(output_x) - f64::from(layout_min_x)) + x * f64::from(output_w);
    let translated_y = (f64::from(output_y) - f64::from(layout_min_y)) + y * f64::from(output_h);
    (
        translated_x.round().clamp(0.0, f64::from(total_w)) as u32,
        translated_y.round().clamp(0.0, f64::from(total_h)) as u32,
    )
}

/// Convert normalized stream coordinates into the selected output's local
/// coordinate frame. This is used only by an output-bound virtual pointer.
fn absolute_position_in_output(
    output_w: u32,
    output_h: u32,
    x_norm: f64,
    y_norm: f64,
) -> (u32, u32) {
    (
        (x_norm.clamp(0.0, 1.0) * f64::from(output_w)).round() as u32,
        (y_norm.clamp(0.0, 1.0) * f64::from(output_h)).round() as u32,
    )
}

fn refreshed_stream_output<'a>(
    outputs: &'a HashMap<u32, OutputInfo>,
    current_name: &str,
) -> Option<&'a OutputInfo> {
    outputs
        .values()
        .find(|output| output.name == current_name)
        .or_else(|| outputs.iter().min_by_key(|(index, _)| *index).map(|(_, output)| output))
}

/// Merge a capture-worker topology into the input connection's output view.
///
/// xdg-output is authoritative for logical positions. The capture worker can
/// report `(0, 0)` for virtual Sway outputs even when they are placed elsewhere
/// in the desktop layout. Keep the local Wayland logical geometry for an output
/// with the same stable name. The worker still supplies membership and fallback
/// geometry for outputs which the input connection has not seen yet.
fn merge_output_layout(
    known_outputs: &HashMap<u32, OutputInfo>,
    reported_outputs: &[OutputGeometry],
) -> HashMap<u32, OutputInfo> {
    reported_outputs
        .iter()
        .enumerate()
        .map(|(index, output)| {
            let known = known_outputs.values().find(|known| known.name == output.name);
            (
                index as u32,
                OutputInfo {
                    name: output.name.clone(),
                    x: known.map(|output| output.x).unwrap_or(output.x),
                    y: known.map(|output| output.y).unwrap_or(output.y),
                    width: known.map(|output| output.width).unwrap_or(output.width),
                    height: known.map(|output| output.height).unwrap_or(output.height),
                },
            )
        })
        .collect()
}

/// Resolve the global origin for the output that is currently being streamed.
///
/// A display name is the only safe local identity. Width and height are not an
/// identity: a virtual display and the built-in display often have the same
/// mode. If the input connection has not discovered the named display yet,
/// use the capture worker's explicit origin instead of guessing from its size.
fn stream_output_origin(
    outputs: &HashMap<u32, OutputInfo>,
    output_name: &str,
    reported_x: Option<i32>,
    reported_y: Option<i32>,
) -> (i32, i32) {
    if let Some(output) = outputs.values().find(|output| output.name == output_name) {
        return (output.x, output.y);
    }

    match (reported_x, reported_y) {
        (Some(x), Some(y)) => (x, y),
        _ => (0, 0),
    }
}

impl WaylandInput {
    /// Read already-available Wayland events without waiting for the compositor.
    /// Input must remain responsive, so a quiet Wayland socket is not an error.
    fn poll_wayland_events(&mut self) {
        if let Err(error) = self.event_queue.dispatch_pending(&mut self.wayland_state) {
            log::warn!("Input: could not dispatch pending Wayland events: {error}");
            return;
        }

        let Some(read_guard) = self.event_queue.prepare_read() else {
            return;
        };
        match read_guard.read() {
            Ok(_) => {
                if let Err(error) = self.event_queue.dispatch_pending(&mut self.wayland_state) {
                    log::warn!("Input: could not dispatch Wayland events: {error}");
                }
            }
            Err(WaylandError::Io(error)) if error.kind() == std::io::ErrorKind::WouldBlock => {}
            Err(error) => log::warn!("Input: could not read Wayland events: {error}"),
        }
    }

    /// Create a pointer for each output first discovered after startup.
    fn create_missing_output_pointers(&mut self) {
        let Some(manager) = self
            .wayland_state
            .pointer_manager
            .as_ref()
            .filter(|manager| manager.version() >= 2)
            .cloned()
        else {
            return;
        };

        let missing_outputs: Vec<_> = self
            .wayland_state
            .outputs
            .iter()
            .filter(|(_, info)| !self.output_virtual_pointers.contains_key(&info.name))
            .filter_map(|(id, info)| {
                self.wayland_state
                    .output_objects
                    .get(id)
                    .cloned()
                    .map(|object| (info.name.clone(), object))
            })
            .collect();

        for (name, output) in missing_outputs {
            let pointer = manager.create_virtual_pointer_with_output(
                self.wayland_state.seat.as_ref(),
                Some(&output),
                &self.qh,
                (),
            );
            self.output_virtual_pointers.insert(name, pointer);
        }

        let live_names: Vec<_> =
            self.wayland_state.outputs.values().map(|output| output.name.as_str()).collect();
        self.output_virtual_pointers.retain(|name, _| live_names.contains(&name.as_str()));
    }

    /// Apply current local Wayland geometry to the topology received from the
    /// capture worker. Output names join the two independent connections.
    fn refresh_live_output_layout(&mut self) {
        self.poll_wayland_events();
        self.create_missing_output_pointers();

        self.outputs = match &self.reported_outputs {
            // Before the capture worker sends its first topology, use the
            // local view. Once it has sent one, even an empty list means all
            // published outputs were removed.
            None => self.wayland_state.outputs.clone(),
            Some(reported_outputs) => {
                merge_output_layout(&self.wayland_state.outputs, reported_outputs)
            }
        };
        let (min_x, min_y, total_w, total_h) = output_layout_bounds(self.outputs.values());
        self.layout_min_x = min_x;
        self.layout_min_y = min_y;
        self.total_w = total_w;
        self.total_h = total_h;

        let selected = refreshed_stream_output(&self.outputs, &self.stream_output_name).cloned();
        if let Some(output) = selected {
            let selected_changed = output.name != self.stream_output_name;
            if selected_changed && !self.stream_output_name.is_empty() {
                log::warn!(
                    "Input: mapped output '{}' disappeared; falling back to '{}'",
                    self.stream_output_name,
                    output.name
                );
            }
            self.stream_output_name = output.name;
            self.stream_output_x = output.x;
            self.stream_output_y = output.y;
            self.stream_output_w = output.width;
            self.stream_output_h = output.height;
            if let Some(pointer) = self.output_virtual_pointers.get(&self.stream_output_name) {
                self.virtual_pointer = pointer.clone();
                self.pointer_is_output_bound = true;
            }
        } else {
            self.stream_output_name.clear();
            self.stream_output_x = 0;
            self.stream_output_y = 0;
            self.stream_output_w = self.total_w;
            self.stream_output_h = self.total_h;
            self.virtual_pointer = self.generic_virtual_pointer.clone();
            self.pointer_is_output_bound = false;
        }
    }

    /// Connect to Wayland, discover outputs, and create virtual devices.
    pub fn new() -> Result<WaylandInput, String> {
        let conn = Connection::connect_to_env()
            .map_err(|e| format!("failed to connect to Wayland: {}", e))?;

        let display = conn.display();
        let mut event_queue: EventQueue<InputWaylandState> = conn.new_event_queue();
        let qh = event_queue.handle();

        let mut wl_state = InputWaylandState {
            seat: None,
            pointer_manager: None,
            keyboard_manager: None,
            xdg_output_manager: None,
            outputs: HashMap::new(),
            output_objects: HashMap::new(),
            output_globals: HashMap::new(),
            xdg_to_wl: HashMap::new(),
            pending_outputs: HashMap::new(),
            stream_output_x: 0,
            stream_output_y: 0,
            stream_output_w: 1920,
            stream_output_h: 1080,
            layout_min_x: 0,
            layout_min_y: 0,
            total_w: 1920,
            total_h: 1080,
        };

        display.get_registry(&qh, ());
        // First roundtrip: discovers globals (seat, managers, outputs).
        event_queue
            .roundtrip(&mut wl_state)
            .map_err(|e| format!("Wayland roundtrip failed: {}", e))?;
        // Second roundtrip: receives wl_output events (mode, name, geometry, done).
        let _ = event_queue.roundtrip(&mut wl_state);
        // Third + fourth roundtrip: receives xdg_output events (logical position/size)
        // which update the wl_output entries with correct compositor-layout coords.
        let _ = event_queue.roundtrip(&mut wl_state);
        let _ = event_queue.roundtrip(&mut wl_state);

        // Compute total compositor extent from discovered outputs.
        for (id, info) in &wl_state.outputs {
            log::info!(
                "Input: output '{}' (id={}) {}x{} at ({},{})",
                info.name,
                id,
                info.width,
                info.height,
                info.x,
                info.y
            );
        }
        let (min_x, min_y, total_w, total_h) = output_layout_bounds(wl_state.outputs.values());
        wl_state.layout_min_x = min_x;
        wl_state.layout_min_y = min_y;
        wl_state.total_w = total_w;
        wl_state.total_h = total_h;
        log::info!(
            "Input: compositor bounds origin=({},{}) extent={}x{}",
            min_x,
            min_y,
            total_w,
            total_h
        );

        // Virtual pointer
        let generic_virtual_pointer = match wl_state.pointer_manager.as_ref() {
            Some(m) => m.create_virtual_pointer(wl_state.seat.as_ref(), &qh, ()),
            None => {
                return Err(
                    "compositor does not support zwlr_virtual_pointer_manager_v1".to_string()
                );
            }
        };
        let mut output_virtual_pointers = HashMap::new();
        if let Some(manager) =
            wl_state.pointer_manager.as_ref().filter(|manager| manager.version() >= 2)
        {
            for (id, info) in &wl_state.outputs {
                if let Some(output) = wl_state.output_objects.get(id) {
                    let pointer = manager.create_virtual_pointer_with_output(
                        wl_state.seat.as_ref(),
                        Some(output),
                        &qh,
                        (),
                    );
                    output_virtual_pointers.insert(info.name.clone(), pointer);
                }
            }
        } else {
            log::warn!("Input: virtual-pointer v2 unavailable; absolute input uses global layout");
        }

        // Virtual keyboard
        let virtual_keyboard = match (wl_state.keyboard_manager.as_ref(), wl_state.seat.as_ref()) {
            (Some(m), Some(seat)) => Some(m.create_virtual_keyboard(seat, &qh, ())),
            _ => {
                log::warn!(
                    "Input: compositor does not support zwp_virtual_keyboard_manager_v1, keyboard input disabled"
                );
                None
            }
        };

        event_queue
            .roundtrip(&mut wl_state)
            .map_err(|e| format!("roundtrip after creating devices failed: {}", e))?;

        // Set up XKB keymap for the virtual keyboard.
        let xkb_context = xkbcommon::xkb::Context::new(0);
        let keymap = xkbcommon::xkb::Keymap::new_from_names(
            &xkb_context,
            "",   // rules
            "",   // model
            "",   // layout (default, usually "us")
            "",   // variant
            None, // options
            0,
        );

        let char_to_keycode = match &keymap {
            Some(km) => build_char_to_keycode(km),
            None => {
                log::warn!("Input: failed to create XKB keymap, keyboard input disabled");
                HashMap::new()
            }
        };

        // Send keymap to compositor via memfd.
        if let (Some(vk), Some(km)) = (&virtual_keyboard, &keymap) {
            let keymap_str = km.get_as_string(xkbcommon::xkb::KEYMAP_FORMAT_TEXT_V1);
            let keymap_bytes = keymap_str.as_bytes();
            let size = keymap_bytes.len();

            // Create a memfd and write the keymap into it.
            let name = std::ffi::CString::new("anchor-keymap").unwrap();
            let fd = unsafe { libc::memfd_create(name.as_ptr(), libc::MFD_CLOEXEC) };
            if fd >= 0 {
                unsafe {
                    libc::ftruncate(fd, size as libc::off_t);
                    let ptr = libc::mmap(
                        std::ptr::null_mut(),
                        size,
                        libc::PROT_WRITE,
                        libc::MAP_SHARED,
                        fd,
                        0,
                    );
                    if ptr != libc::MAP_FAILED {
                        std::ptr::copy_nonoverlapping(keymap_bytes.as_ptr(), ptr as *mut u8, size);
                        libc::munmap(ptr, size);
                    }
                }

                // Send keymap fd to the virtual keyboard.
                // format 1 = XKB_KEYMAP_FORMAT_TEXT_V1
                use std::os::unix::io::{BorrowedFd, FromRawFd};
                let owned_fd = unsafe { std::os::unix::io::OwnedFd::from_raw_fd(fd) };
                let borrowed = unsafe { BorrowedFd::borrow_raw(owned_fd.as_raw_fd()) };
                vk.keymap(1, borrowed, size as u32);
                let _ = conn.flush();
                drop(owned_fd);
                log::info!(
                    "Input: virtual keyboard keymap sent ({} bytes, {} char mappings)",
                    size,
                    char_to_keycode.len()
                );
            } else {
                log::error!("Input: memfd_create failed");
            }
        }

        log::info!(
            "Input: ready — pointer: yes, keyboard: {}, total extent: {}x{}",
            virtual_keyboard.is_some(),
            wl_state.total_w,
            wl_state.total_h
        );
        let initial_outputs = wl_state.outputs.clone();
        let initial_stream_output_x = wl_state.stream_output_x;
        let initial_stream_output_y = wl_state.stream_output_y;
        let initial_stream_output_w = wl_state.stream_output_w;
        let initial_stream_output_h = wl_state.stream_output_h;
        let initial_layout_min_x = wl_state.layout_min_x;
        let initial_layout_min_y = wl_state.layout_min_y;
        let initial_total_w = wl_state.total_w;
        let initial_total_h = wl_state.total_h;

        Ok(WaylandInput {
            conn,
            event_queue,
            wayland_state: wl_state,
            qh,
            generic_virtual_pointer: generic_virtual_pointer.clone(),
            virtual_pointer: generic_virtual_pointer,
            output_virtual_pointers,
            pointer_is_output_bound: false,
            virtual_keyboard,
            char_to_keycode,
            outputs: initial_outputs,
            reported_outputs: None,
            stream_output_name: String::new(),
            stream_output_x: initial_stream_output_x,
            stream_output_y: initial_stream_output_y,
            stream_output_w: initial_stream_output_w,
            stream_output_h: initial_stream_output_h,
            layout_min_x: initial_layout_min_x,
            layout_min_y: initial_layout_min_y,
            total_w: initial_total_w,
            total_h: initial_total_h,
        })
    }
}

impl InputBackend for WaylandInput {
    fn total_extent(&self) -> (u32, u32) {
        (self.total_w, self.total_h)
    }

    fn stream_mapping(&self) -> (u32, u32, i32, i32) {
        (self.stream_output_w, self.stream_output_h, self.stream_output_x, self.stream_output_y)
    }

    fn has_keyboard(&self) -> bool {
        self.virtual_keyboard.is_some()
    }

    fn set_stream_info(
        &mut self,
        output_name: &str,
        width: u32,
        height: u32,
        output_x: Option<i32>,
        output_y: Option<i32>,
    ) {
        self.refresh_live_output_layout();
        self.stream_output_name = output_name.to_string();
        // Prefer local xdg_output positions (always correct) over what
        // the screencopy backend reports — wl_output::Geometry gives wrong
        // positions for virtual outputs like HEADLESS on Sway.
        let (ox, oy) = stream_output_origin(&self.outputs, output_name, output_x, output_y);

        self.stream_output_x = ox;
        self.stream_output_y = oy;
        self.stream_output_w = width;
        self.stream_output_h = height;
        if let Some(pointer) = self.output_virtual_pointers.get(output_name) {
            self.virtual_pointer = pointer.clone();
            self.pointer_is_output_bound = true;
        } else {
            self.virtual_pointer = self.generic_virtual_pointer.clone();
            self.pointer_is_output_bound = false;
            log::warn!(
                "Input: no output-bound pointer for '{}'; using global-layout fallback",
                output_name
            );
        }
        // Expand the signed compositor bounding box to include a stream output
        // that was not present during initial Wayland output discovery.
        let current_max_x = i64::from(self.layout_min_x) + i64::from(self.total_w);
        let current_max_y = i64::from(self.layout_min_y) + i64::from(self.total_h);
        let min_x = i64::from(self.layout_min_x).min(i64::from(ox));
        let min_y = i64::from(self.layout_min_y).min(i64::from(oy));
        let max_x = current_max_x.max(i64::from(ox) + i64::from(width));
        let max_y = current_max_y.max(i64::from(oy) + i64::from(height));
        self.layout_min_x = min_x.clamp(i64::from(i32::MIN), i64::from(i32::MAX)) as i32;
        self.layout_min_y = min_y.clamp(i64::from(i32::MIN), i64::from(i32::MAX)) as i32;
        self.total_w = (max_x - min_x).clamp(1, i64::from(u32::MAX)) as u32;
        self.total_h = (max_y - min_y).clamp(1, i64::from(u32::MAX)) as u32;
        log::info!(
            "Input: stream mapped to '{}' {}x{} at ({},{}) — layout origin ({},{}) extent {}x{}",
            output_name,
            width,
            height,
            ox,
            oy,
            self.layout_min_x,
            self.layout_min_y,
            self.total_w,
            self.total_h,
        );
    }

    fn set_output_layout(&mut self, outputs: &[OutputGeometry]) {
        self.reported_outputs = Some(outputs.to_vec());
        self.refresh_live_output_layout();
        log::info!(
            "Input: refreshed output topology ({} outputs), layout origin ({},{}) extent {}x{}",
            outputs.len(),
            self.layout_min_x,
            self.layout_min_y,
            self.total_w,
            self.total_h
        );
    }

    fn pointer_motion(&mut self, time: u32, dx: f64, dy: f64) {
        self.virtual_pointer.motion(time, dx, dy);
        self.virtual_pointer.frame();
    }

    fn pointer_motion_absolute(&mut self, time: u32, x_norm: f64, y_norm: f64) {
        self.refresh_live_output_layout();
        if self.pointer_is_output_bound {
            let (x, y) = absolute_position_in_output(
                self.stream_output_w,
                self.stream_output_h,
                x_norm,
                y_norm,
            );
            log::debug!(
                "Input: abs norm=({:.3},{:.3}) -> output-local=({},{}) extent={}x{} output='{}'",
                x_norm,
                y_norm,
                x,
                y,
                self.stream_output_w,
                self.stream_output_h,
                self.stream_output_name
            );
            self.virtual_pointer.motion_absolute(
                time,
                x,
                y,
                self.stream_output_w,
                self.stream_output_h,
            );
            self.virtual_pointer.frame();
            return;
        }
        // Translate signed global compositor coordinates into the unsigned
        // layout coordinate space required by wlr-virtual-pointer.
        let (abs_x, abs_y) = absolute_position_in_layout(
            self.layout_min_x,
            self.layout_min_y,
            self.total_w,
            self.total_h,
            self.stream_output_x,
            self.stream_output_y,
            self.stream_output_w,
            self.stream_output_h,
            x_norm,
            y_norm,
        );
        log::debug!(
            "Input: abs norm=({:.3},{:.3}) -> layout=({},{}) extent={}x{} output_offset=({},{}) layout_origin=({},{})",
            x_norm,
            y_norm,
            abs_x,
            abs_y,
            self.total_w,
            self.total_h,
            self.stream_output_x,
            self.stream_output_y,
            self.layout_min_x,
            self.layout_min_y
        );
        self.virtual_pointer.motion_absolute(time, abs_x, abs_y, self.total_w, self.total_h);
        self.virtual_pointer.frame();
    }

    fn pointer_motion_absolute_targeted(
        &mut self,
        time: u32,
        x_norm: f64,
        y_norm: f64,
        output_name: Option<&str>,
    ) {
        self.refresh_live_output_layout();
        let Some(output_name) = output_name else {
            self.pointer_motion_absolute(time, x_norm, y_norm);
            return;
        };
        let Some(output) = self.outputs.values().find(|output| output.name == output_name) else {
            log::warn!("Input: rejecting absolute motion for missing output '{output_name}'");
            return;
        };
        let Some(pointer) = self.output_virtual_pointers.get(output_name).cloned() else {
            log::warn!(
                "Input: rejecting absolute motion; no output-bound pointer for '{output_name}'"
            );
            return;
        };
        let (x, y) = absolute_position_in_output(output.width, output.height, x_norm, y_norm);
        log::debug!(
            "Input: targeted abs norm=({:.3},{:.3}) -> output-local=({},{}) extent={}x{} output='{}'",
            x_norm,
            y_norm,
            x,
            y,
            output.width,
            output.height,
            output_name
        );
        // A stroke is sent as a targeted absolute motion followed by button
        // transitions. Keep the selected output's virtual pointer active so
        // those transitions use the same Wayland device as the motion. Using
        // `pointer` only here would move one output-bound pointer and press
        // whichever pointer was selected by an earlier stream update.
        self.virtual_pointer = pointer;
        self.pointer_is_output_bound = true;
        self.virtual_pointer.motion_absolute(time, x, y, output.width, output.height);
        self.virtual_pointer.frame();
    }

    fn pointer_button(&mut self, time: u32, button: u32, pressed: bool) {
        let btn_state = if pressed {
            wl_pointer::ButtonState::Pressed
        } else {
            wl_pointer::ButtonState::Released
        };
        self.virtual_pointer.button(time, button, btn_state);
        self.virtual_pointer.frame();
    }

    fn pointer_axis(&mut self, time: u32, horizontal: bool, value: f64) {
        let axis = if horizontal {
            wl_pointer::Axis::HorizontalScroll
        } else {
            wl_pointer::Axis::VerticalScroll
        };
        self.virtual_pointer.axis(time, axis, value);
        self.virtual_pointer.frame();
    }

    fn pointer_frame(&mut self) {
        self.virtual_pointer.frame();
    }

    fn type_text(&mut self, time: u32, text: &str) {
        let Some(ref vk) = self.virtual_keyboard else {
            return;
        };
        for ch in text.chars() {
            if let Some(&(keycode, needs_shift)) = self.char_to_keycode.get(&ch) {
                if needs_shift {
                    vk.modifiers(MOD_SHIFT, 0, 0, 0);
                }
                vk.key(time, keycode, 1); // press
                vk.key(time, keycode, 0); // release
                if needs_shift {
                    vk.modifiers(0, 0, 0, 0);
                }
            }
        }
    }

    fn key_special(&mut self, time: u32, key: &str) -> bool {
        let Some(ref vk) = self.virtual_keyboard else {
            return false;
        };
        match special_key_to_evdev(key) {
            Some(keycode) => {
                vk.key(time, keycode, 1);
                vk.key(time, keycode, 0);
                true
            }
            None => false,
        }
    }

    fn key_combo(&mut self, time: u32, modifiers: &[String], key: &str) {
        let Some(ref vk) = self.virtual_keyboard else {
            return;
        };

        let mask: u32 = modifiers.iter().map(|m| modifier_to_xkb_mask(m)).fold(0, |a, b| a | b);
        let mod_keycodes: Vec<u32> =
            modifiers.iter().filter_map(|m| modifier_to_evdev(m)).collect();

        // Press modifiers
        vk.modifiers(mask, 0, 0, 0);
        for &kc in &mod_keycodes {
            vk.key(time, kc, 1);
        }

        // Press + release the key
        let key_keycode = if key.len() == 1 {
            key.chars()
                .next()
                .and_then(|ch| {
                    self.char_to_keycode
                        .get(&ch)
                        .or_else(|| self.char_to_keycode.get(&ch.to_ascii_lowercase()))
                })
                .map(|&(kc, _)| kc)
        } else {
            special_key_to_evdev(key)
        };
        if let Some(kc) = key_keycode {
            vk.key(time, kc, 1);
            vk.key(time, kc, 0);
        }

        // Release modifiers
        for &kc in mod_keycodes.iter().rev() {
            vk.key(time, kc, 0);
        }

        // Clear modifier state.
        vk.modifiers(0, 0, 0, 0);
    }

    fn key_hid(&mut self, time: u32, hid_usage: u32, pressed: bool) -> bool {
        let Some(ref vk) = self.virtual_keyboard else {
            return false;
        };
        let Some(keycode) = hid_usage_to_evdev(hid_usage) else {
            return false;
        };
        vk.key(time, keycode, u32::from(pressed));
        true
    }

    fn flush(&mut self) {
        if let Err(e) = self.conn.flush() {
            log::warn!("Input: Wayland flush failed: {}", e);
        }
    }
}

impl Drop for WaylandInput {
    fn drop(&mut self) {
        self.virtual_pointer.destroy();
        if let Some(vk) = &self.virtual_keyboard {
            vk.destroy();
        }
        let _ = self.conn.flush();
    }
}

/// Build a char→keycode lookup from the xkb keymap.
fn build_char_to_keycode(keymap: &xkbcommon::xkb::Keymap) -> HashMap<char, (u32, bool)> {
    let mut map = HashMap::new();
    let state = xkbcommon::xkb::State::new(keymap);

    // Scan keycodes 8..256 (evdev keycodes + 8 offset).
    for kc in 8u32..256 {
        let keycode = xkbcommon::xkb::Keycode::new(kc);
        // Unshifted
        let sym = state.key_get_one_sym(keycode);
        if let Some(ch) = keysym_to_char(sym) {
            map.entry(ch).or_insert((kc - 8, false));
        }
        // Shifted (level 1)
        let syms = keymap.key_get_syms_by_level(keycode, 0, 1);
        if let Some(&sym) = syms.first()
            && let Some(ch) = keysym_to_char(sym)
        {
            map.entry(ch).or_insert((kc - 8, true));
        }
    }

    map
}

fn keysym_to_char(sym: xkbcommon::xkb::Keysym) -> Option<char> {
    let raw = sym.raw();
    if raw == 0 {
        return None;
    }
    let cp = if raw >= 0x01000000 {
        raw - 0x01000000
    } else if (0x20..=0x7e).contains(&raw) || (0xa0..=0xff).contains(&raw) {
        raw
    } else {
        return None;
    };
    char::from_u32(cp)
}

/// Map special key names (from Android) to evdev keycodes.
fn special_key_to_evdev(key: &str) -> Option<u32> {
    Some(match key {
        "Return" | "Enter" => 28,
        "BackSpace" | "Backspace" => 14,
        "Tab" => 15,
        "Escape" | "Esc" => 1,
        "Delete" => 111,
        "Home" => 102,
        "End" => 107,
        "Left" => 105,
        "Right" => 106,
        "Up" => 103,
        "Down" => 108,
        "PageUp" => 104,
        "PageDown" => 109,
        "space" | "Space" => 57,
        _ => return None,
    })
}

fn hid_usage_to_evdev(usage: u32) -> Option<u32> {
    const LETTERS: [u32; 26] = [
        30, 48, 46, 32, 18, 33, 34, 35, 23, 36, 37, 38, 50, 49, 24, 25, 16, 19, 31, 20, 22, 47, 17,
        45, 21, 44,
    ];
    Some(match usage {
        0x04..=0x1d => LETTERS[(usage - 0x04) as usize],
        0x1e => 2,
        0x1f => 3,
        0x20 => 4,
        0x21 => 5,
        0x22 => 6,
        0x23 => 7,
        0x24 => 8,
        0x25 => 9,
        0x26 => 10,
        0x27 => 11,
        0x28 => 28,
        0x29 => 1,
        0x2a => 14,
        0x2b => 15,
        0x2c => 57,
        0x39 => 58,
        0x3a..=0x45 => {
            if usage <= 0x43 {
                usage + 25
            } else if usage == 0x44 {
                87
            } else {
                88
            }
        }
        0x4a => 102,
        0x4b => 104,
        0x4c => 111,
        0x4d => 107,
        0x4e => 109,
        0x4f => 106,
        0x50 => 105,
        0x51 => 108,
        0x52 => 103,
        0xe0 => 29,
        0xe1 => 42,
        0xe2 => 56,
        0xe3 => 125,
        _ => return None,
    })
}

/// Map modifier name to evdev keycode.
fn modifier_to_evdev(name: &str) -> Option<u32> {
    Some(match name {
        "ctrl" => 29,   // KEY_LEFTCTRL
        "alt" => 56,    // KEY_LEFTALT
        "shift" => 42,  // KEY_LEFTSHIFT
        "super" => 125, // KEY_LEFTMETA
        _ => return None,
    })
}

/// Map modifier name to XKB modifier mask bit.
fn modifier_to_xkb_mask(name: &str) -> u32 {
    match name {
        "ctrl" => 4,   // XKB_MOD_NAME_CTRL
        "alt" => 8,    // XKB_MOD_NAME_ALT (Mod1)
        "shift" => 1,  // XKB_MOD_NAME_SHIFT
        "super" => 64, // XKB_MOD_NAME_LOGO (Mod4)
        _ => 0,
    }
}

// -- Wayland dispatch --

impl Dispatch<wl_registry::WlRegistry, ()> for InputWaylandState {
    fn event(
        state: &mut Self,
        registry: &wl_registry::WlRegistry,
        event: wl_registry::Event,
        _data: &(),
        _conn: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        match event {
            wl_registry::Event::Global { name, interface, version } => match interface.as_str() {
                "wl_seat" => {
                    state.seat =
                        Some(registry.bind::<wl_seat::WlSeat, _, _>(name, version, qh, ()));
                }
                "zwlr_virtual_pointer_manager_v1" => {
                    state.pointer_manager = Some(
                        registry.bind::<zwlr_virtual_pointer_manager_v1::ZwlrVirtualPointerManagerV1, _, _>(
                            name, version.min(2), qh, (),
                        ),
                    );
                }
                "zwp_virtual_keyboard_manager_v1" => {
                    state.keyboard_manager = Some(
                        registry.bind::<zwp_virtual_keyboard_manager_v1::ZwpVirtualKeyboardManagerV1, _, _>(
                            name, version.min(1), qh, (),
                        ),
                    );
                }
                "wl_output" => {
                    let output =
                        registry.bind::<wl_output::WlOutput, _, _>(name, version.min(4), qh, ());
                    let output_id = output.id().protocol_id();
                    state.output_objects.insert(output_id, output.clone());
                    state.output_globals.insert(name, output_id);
                    // Request xdg_output for this wl_output to get logical position.
                    if let Some(mgr) = state.xdg_output_manager.clone() {
                        let wl_id = output.id().protocol_id();
                        let xdg_out = mgr.get_xdg_output(&output, qh, ());
                        let xdg_id = xdg_out.id().protocol_id();
                        state.xdg_to_wl.insert(xdg_id, wl_id);
                    }
                }
                "zxdg_output_manager_v1" => {
                    let manager = registry
                        .bind::<zxdg_output_manager_v1::ZxdgOutputManagerV1, _, _>(
                            name,
                            version.min(3),
                            qh,
                            (),
                        );
                    // Registry global order is not guaranteed. If outputs
                    // appeared first, subscribe them now instead of leaving
                    // their wl_output::Geometry coordinates authoritative.
                    for (wl_id, output) in &state.output_objects {
                        let xdg_output = manager.get_xdg_output(output, qh, ());
                        state.xdg_to_wl.insert(xdg_output.id().protocol_id(), *wl_id);
                    }
                    state.xdg_output_manager = Some(manager);
                }
                _ => {}
            },
            wl_registry::Event::GlobalRemove { name } => {
                let Some(output_id) = state.output_globals.remove(&name) else {
                    return;
                };
                state.outputs.remove(&output_id);
                state.pending_outputs.remove(&output_id);
                state.output_objects.remove(&output_id);
                state.xdg_to_wl.retain(|_, wl_id| *wl_id != output_id);
                let (min_x, min_y, total_w, total_h) = output_layout_bounds(state.outputs.values());
                state.layout_min_x = min_x;
                state.layout_min_y = min_y;
                state.total_w = total_w;
                state.total_h = total_h;
                log::info!("Input: removed Wayland output id={output_id}");
            }
            _ => {}
        }
    }
}

impl Dispatch<wl_output::WlOutput, ()> for InputWaylandState {
    fn event(
        state: &mut Self,
        proxy: &wl_output::WlOutput,
        event: wl_output::Event,
        _data: &(),
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
        let id = proxy.id().protocol_id();
        // wl_output sends a new Done after a mode or geometry change. Seed the
        // update with the prior name and geometry, because these later batches
        // often contain only the changed fields.
        let prior = state.outputs.get(&id).cloned().unwrap_or_default();
        let pending = state.pending_outputs.entry(id).or_insert(prior);

        match event {
            wl_output::Event::Geometry { x, y, .. } => {
                pending.x = x;
                pending.y = y;
            }
            wl_output::Event::Mode { flags, width, height, .. } => {
                let is_current = match flags {
                    WEnum::Value(f) => f.contains(wl_output::Mode::Current),
                    _ => false,
                };
                if is_current {
                    pending.width = width as u32;
                    pending.height = height as u32;
                }
            }
            wl_output::Event::Name { name } => {
                pending.name = name;
            }
            wl_output::Event::Done => {
                if let Some(info) = state.pending_outputs.remove(&id) {
                    // Only insert if this has real data (name + size).
                    // Subsequent Done events after xdg roundtrips have empty pending.
                    if !info.name.is_empty() && info.width > 0 {
                        log::info!(
                            "Input: output '{}' (id={}) {}x{} at ({},{})",
                            info.name,
                            id,
                            info.width,
                            info.height,
                            info.x,
                            info.y
                        );
                        state.outputs.insert(id, info);
                        // Recompute signed bounds so late-arriving outputs
                        // (HEADLESS/hotplug) can extend left or above origin.
                        let (min_x, min_y, new_w, new_h) =
                            output_layout_bounds(state.outputs.values());
                        if min_x != state.layout_min_x
                            || min_y != state.layout_min_y
                            || new_w != state.total_w
                            || new_h != state.total_h
                        {
                            log::info!(
                                "Input: compositor bounds updated ({},{}) {}x{} → ({},{}) {}x{}",
                                state.layout_min_x,
                                state.layout_min_y,
                                state.total_w,
                                state.total_h,
                                min_x,
                                min_y,
                                new_w,
                                new_h
                            );
                            state.layout_min_x = min_x;
                            state.layout_min_y = min_y;
                            state.total_w = new_w;
                            state.total_h = new_h;
                        }
                    }
                }
            }
            _ => {}
        }
    }
}

impl Dispatch<zxdg_output_v1::ZxdgOutputV1, ()> for InputWaylandState {
    fn event(
        state: &mut Self,
        proxy: &zxdg_output_v1::ZxdgOutputV1,
        event: zxdg_output_v1::Event,
        _data: &(),
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
        let xdg_id = proxy.id().protocol_id();
        let wl_id = match state.xdg_to_wl.get(&xdg_id) {
            Some(&id) => id,
            None => return,
        };
        // Only update if the output already exists (from wl_output::Done).
        let output = match state.outputs.get_mut(&wl_id) {
            Some(o) => o,
            None => return,
        };
        match event {
            zxdg_output_v1::Event::LogicalPosition { x, y } => {
                output.x = x;
                output.y = y;
                log::info!("Input: xdg logical position for id={}: ({},{})", wl_id, x, y);
            }
            zxdg_output_v1::Event::LogicalSize { width, height } => {
                output.width = width as u32;
                output.height = height as u32;
                log::info!("Input: xdg logical size for id={}: {}x{}", wl_id, width, height);
            }
            _ => {}
        }
        // xdg-output supplies the authoritative logical position/size after
        // wl_output::Done, so every correction must refresh the signed bounds.
        let (min_x, min_y, total_w, total_h) = output_layout_bounds(state.outputs.values());
        state.layout_min_x = min_x;
        state.layout_min_y = min_y;
        state.total_w = total_w;
        state.total_h = total_h;
    }
}

delegate_noop!(InputWaylandState: ignore wl_seat::WlSeat);
delegate_noop!(InputWaylandState: ignore zwlr_virtual_pointer_manager_v1::ZwlrVirtualPointerManagerV1);
delegate_noop!(InputWaylandState: ignore zwlr_virtual_pointer_v1::ZwlrVirtualPointerV1);
delegate_noop!(InputWaylandState: ignore zwp_virtual_keyboard_manager_v1::ZwpVirtualKeyboardManagerV1);
delegate_noop!(InputWaylandState: ignore zwp_virtual_keyboard_v1::ZwpVirtualKeyboardV1);
delegate_noop!(InputWaylandState: ignore zxdg_output_manager_v1::ZxdgOutputManagerV1);

#[cfg(test)]
mod coordinate_tests {
    use super::*;

    fn output(x: i32, y: i32, width: u32, height: u32) -> OutputInfo {
        OutputInfo { x, y, width, height, ..OutputInfo::default() }
    }

    #[test]
    fn topology_refresh_preserves_name_or_uses_published_first_output() {
        let outputs = HashMap::from([
            (1, OutputInfo { name: "alpha".into(), ..output(100, 0, 800, 600) }),
            (0, OutputInfo { name: "zeta".into(), ..output(0, 0, 100, 100) }),
        ]);

        assert_eq!(refreshed_stream_output(&outputs, "alpha").unwrap().name, "alpha");
        assert_eq!(refreshed_stream_output(&outputs, "deleted").unwrap().name, "zeta");
        assert!(refreshed_stream_output(&HashMap::new(), "deleted").is_none());
    }

    #[test]
    fn unknown_virtual_output_uses_reported_origin_not_same_sized_primary() {
        // Both displays are 1920x1080. Selecting by dimensions would map this
        // event to eDP-1 instead of the selected HEADLESS-2 output.
        let outputs =
            HashMap::from([(1, OutputInfo { name: "eDP-1".into(), ..output(0, 0, 1920, 1080) })]);

        assert_eq!(
            stream_output_origin(&outputs, "HEADLESS-2", Some(1920), Some(-240)),
            (1920, -240)
        );
    }

    #[test]
    fn known_output_position_overrides_capture_fallback() {
        let outputs = HashMap::from([(
            9,
            OutputInfo { name: "HEADLESS-2".into(), ..output(-1600, 80, 1920, 1080) },
        )]);

        assert_eq!(stream_output_origin(&outputs, "HEADLESS-2", Some(0), Some(0)), (-1600, 80));
    }

    #[test]
    fn layout_refresh_keeps_local_position_for_known_virtual_output() {
        let known = HashMap::from([
            (1, OutputInfo { name: "DP-1".into(), ..output(0, 0, 1920, 1080) }),
            (2, OutputInfo { name: "HEADLESS-3".into(), ..output(1920, 0, 1180, 820) }),
        ]);
        // The capture worker reports the virtual display at the compositor
        // origin. This is the failure observed with Sway virtual outputs.
        let reported = vec![
            OutputGeometry { name: "DP-1".into(), x: 0, y: 0, width: 1920, height: 1080 },
            // The capture surface has a different buffer size. Pointer
            // injection needs the compositor's logical output extent.
            OutputGeometry { name: "HEADLESS-3".into(), x: 0, y: 0, width: 2360, height: 1640 },
        ];

        let merged = merge_output_layout(&known, &reported);
        let headless = merged.values().find(|output| output.name == "HEADLESS-3").unwrap();

        assert_eq!((headless.x, headless.y), (1920, 0));
        assert_eq!((headless.width, headless.height), (1180, 820));
        assert_eq!(output_layout_bounds(merged.values()), (0, 0, 3100, 1080));
    }

    #[test]
    fn empty_published_topology_removes_all_outputs() {
        let known = HashMap::from([(
            1,
            OutputInfo { name: "HEADLESS-3".into(), ..output(1920, 0, 1180, 820) },
        )]);

        assert!(merge_output_layout(&known, &[]).is_empty());
    }

    #[test]
    fn layout_refresh_uses_reported_position_for_new_output() {
        let reported = vec![OutputGeometry {
            name: "HEADLESS-4".into(),
            x: -1080,
            y: 50,
            width: 1080,
            height: 1920,
        }];

        let merged = merge_output_layout(&HashMap::new(), &reported);
        let added = merged.values().next().unwrap();

        assert_eq!((added.x, added.y, added.width, added.height), (-1080, 50, 1080, 1920));
    }

    #[test]
    fn refreshed_layout_uses_live_position_after_a_known_output_moves() {
        let live = HashMap::from([
            (1, OutputInfo { name: "eDP-1".into(), ..output(0, 0, 1920, 1080) }),
            (2, OutputInfo { name: "HEADLESS-3".into(), ..output(-1370, 50, 1180, 820) }),
        ]);
        // The capture connection has not learned about the move and still
        // reports its unusable virtual-output origin.
        let reported = vec![
            OutputGeometry { name: "eDP-1".into(), x: 0, y: 0, width: 1920, height: 1080 },
            OutputGeometry { name: "HEADLESS-3".into(), x: 0, y: 0, width: 1180, height: 820 },
        ];

        let merged = merge_output_layout(&live, &reported);
        let selected = refreshed_stream_output(&merged, "HEADLESS-3").unwrap();
        let (min_x, min_y, total_w, total_h) = output_layout_bounds(merged.values());

        assert_eq!((selected.x, selected.y), (-1370, 50));
        // A centred point must follow the new display location. It must not
        // map to eDP-1 or the stale capture origin.
        assert_eq!(
            absolute_position_in_layout(
                min_x,
                min_y,
                total_w,
                total_h,
                selected.x,
                selected.y,
                selected.width,
                selected.height,
                0.5,
                0.5,
            ),
            (590, 460)
        );
    }

    #[test]
    fn reordered_capture_topology_keeps_selected_output_by_name() {
        let live = HashMap::from([
            (7, OutputInfo { name: "eDP-1".into(), ..output(0, 0, 1920, 1080) }),
            (11, OutputInfo { name: "HEADLESS-3".into(), ..output(1920, -300, 1180, 820) }),
        ]);
        // Capture order changes as displays are recreated. Positions in this
        // list are not identities.
        let reordered = vec![
            OutputGeometry { name: "HEADLESS-3".into(), x: 0, y: 0, width: 1180, height: 820 },
            OutputGeometry { name: "eDP-1".into(), x: 0, y: 0, width: 1920, height: 1080 },
        ];

        let merged = merge_output_layout(&live, &reordered);
        let selected = refreshed_stream_output(&merged, "HEADLESS-3").unwrap();

        assert_eq!((selected.name.as_str(), selected.x, selected.y), ("HEADLESS-3", 1920, -300));
    }

    #[test]
    fn output_bound_absolute_pointer_uses_only_the_selected_screen_extent() {
        // The virtual output starts at x=1920 in a 3840px-wide layout. An
        // output-bound pointer must receive local coordinates, not 2880/3840.
        assert_eq!(absolute_position_in_output(1920, 1080, 0.5, 0.5), (960, 540));
        assert_eq!(absolute_position_in_output(1920, 1080, -1.0, 2.0), (0, 1080));
    }

    #[test]
    fn positive_origin_layout_maps_second_output_corners_and_center() {
        let outputs = [output(0, 0, 1920, 1080), output(1920, 0, 2560, 1440)];
        let (min_x, min_y, width, height) = output_layout_bounds(outputs.iter());

        assert_eq!((min_x, min_y, width, height), (0, 0, 4480, 1440));
        assert_eq!(
            absolute_position_in_layout(min_x, min_y, width, height, 1920, 0, 2560, 1440, 0.0, 0.0),
            (1920, 0)
        );
        assert_eq!(
            absolute_position_in_layout(min_x, min_y, width, height, 1920, 0, 2560, 1440, 0.5, 0.5),
            (3200, 720)
        );
        assert_eq!(
            absolute_position_in_layout(min_x, min_y, width, height, 1920, 0, 2560, 1440, 1.0, 1.0),
            (4480, 1440)
        );
    }

    #[test]
    fn negative_left_output_is_translated_into_unsigned_layout_space() {
        let outputs = [output(-1280, 56, 1280, 1024), output(0, 0, 1920, 1080)];
        let (min_x, min_y, width, height) = output_layout_bounds(outputs.iter());

        assert_eq!((min_x, min_y, width, height), (-1280, 0, 3200, 1080));
        assert_eq!(
            absolute_position_in_layout(
                min_x, min_y, width, height, -1280, 56, 1280, 1024, 0.0, 0.0
            ),
            (0, 56)
        );
        assert_eq!(
            absolute_position_in_layout(
                min_x, min_y, width, height, -1280, 56, 1280, 1024, 0.5, 0.5
            ),
            (640, 568)
        );
        assert_eq!(
            absolute_position_in_layout(
                min_x, min_y, width, height, -1280, 56, 1280, 1024, 1.0, 1.0
            ),
            (1280, 1080)
        );
    }

    #[test]
    fn output_above_primary_translates_negative_y() {
        let outputs = [output(320, -900, 1600, 900), output(0, 0, 1920, 1080)];
        let (min_x, min_y, width, height) = output_layout_bounds(outputs.iter());

        assert_eq!((min_x, min_y, width, height), (0, -900, 1920, 1980));
        assert_eq!(
            absolute_position_in_layout(
                min_x, min_y, width, height, 320, -900, 1600, 900, 0.0, 0.0
            ),
            (320, 0)
        );
        assert_eq!(
            absolute_position_in_layout(
                min_x, min_y, width, height, 320, -900, 1600, 900, 0.5, 0.5
            ),
            (1120, 450)
        );
        assert_eq!(
            absolute_position_in_layout(
                min_x, min_y, width, height, 320, -900, 1600, 900, 1.0, 1.0
            ),
            (1920, 900)
        );
    }

    #[test]
    fn normalized_coordinates_are_clamped_to_streamed_output() {
        assert_eq!(
            absolute_position_in_layout(0, 0, 1920, 1080, 0, 0, 1920, 1080, -1.0, 2.0),
            (0, 1080)
        );
        assert_eq!(
            absolute_position_in_layout(0, 0, 1920, 1080, 0, 0, 1920, 1080, 0.5, 0.5),
            (960, 540)
        );
    }

    #[test]
    fn removing_virtual_output_shrinks_refreshed_layout() {
        let with_virtual = [output(0, 0, 1920, 1080), output(1920, 0, 1080, 2340)];
        let after_removal = [output(0, 0, 1920, 1080)];

        assert_eq!(output_layout_bounds(with_virtual.iter()), (0, 0, 3000, 2340));
        assert_eq!(output_layout_bounds(after_removal.iter()), (0, 0, 1920, 1080));
    }

    #[test]
    fn absolute_mapping_is_bounded_for_representative_hotplug_layouts() {
        let layouts = [
            vec![output(0, 0, 1920, 1080)],
            vec![output(-2560, -200, 2560, 1440), output(0, 0, 1920, 1080)],
            vec![output(0, 0, 1920, 1080), output(1920, 60, 1080, 2340)],
        ];
        let normalized = [-10.0, -0.01, 0.0, 0.25, 0.5, 1.0, 1.01, 10.0];

        for outputs in layouts {
            let (min_x, min_y, width, height) = output_layout_bounds(outputs.iter());
            for streamed in &outputs {
                for x in normalized {
                    for y in normalized {
                        let (mapped_x, mapped_y) = absolute_position_in_layout(
                            min_x,
                            min_y,
                            width,
                            height,
                            streamed.x,
                            streamed.y,
                            streamed.width,
                            streamed.height,
                            x,
                            y,
                        );
                        assert!(mapped_x <= width);
                        assert!(mapped_y <= height);
                    }
                }
            }
        }
    }

    #[test]
    fn extreme_valid_geometry_cannot_overflow_layout_bounds() {
        let outputs = [
            output(i32::MIN, i32::MIN, u32::MAX, u32::MAX),
            output(i32::MAX, i32::MAX, u32::MAX, u32::MAX),
        ];

        assert_eq!(output_layout_bounds(outputs.iter()), (i32::MIN, i32::MIN, u32::MAX, u32::MAX));
    }
}
