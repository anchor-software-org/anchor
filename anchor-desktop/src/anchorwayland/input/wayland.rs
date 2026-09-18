//! Wayland input backend.
//!
//! Injects pointer events via wlr-virtual-pointer-unstable-v1 and keyboard
//! events via virtual-keyboard-unstable-v1.

use std::collections::HashMap;
use std::os::unix::io::AsRawFd;

use wayland_client::protocol::{wl_output, wl_pointer, wl_registry, wl_seat};
use wayland_client::{Connection, Dispatch, EventQueue, Proxy, QueueHandle, WEnum, delegate_noop};
use wayland_protocols::xdg::xdg_output::zv1::client::{zxdg_output_manager_v1, zxdg_output_v1};
use wayland_protocols_misc::zwp_virtual_keyboard_v1::client::{
    zwp_virtual_keyboard_manager_v1, zwp_virtual_keyboard_v1,
};
use wayland_protocols_wlr::virtual_pointer::v1::client::{
    zwlr_virtual_pointer_manager_v1, zwlr_virtual_pointer_v1,
};

use super::InputBackend;

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
    /// Map from xdg_output object id → wl_output object id, for routing xdg events.
    xdg_to_wl: HashMap<u32, u32>,
    /// Currently building output (accumulates geometry/mode/name events before Done).
    pending_output: Option<(u32, OutputInfo)>,
    /// The streamed output's geometry in global compositor coords.
    stream_output_x: i32,
    stream_output_y: i32,
    stream_output_w: u32,
    stream_output_h: u32,
    /// Total compositor extent.
    total_w: u32,
    total_h: u32,
}

/// Wayland-based pointer + keyboard injection.
pub struct WaylandInput {
    conn: Connection,
    virtual_pointer: zwlr_virtual_pointer_v1::ZwlrVirtualPointerV1,
    virtual_keyboard: Option<zwp_virtual_keyboard_v1::ZwpVirtualKeyboardV1>,
    char_to_keycode: HashMap<char, (u32, bool)>,
    outputs: HashMap<u32, OutputInfo>,
    stream_output_x: i32,
    stream_output_y: i32,
    stream_output_w: u32,
    stream_output_h: u32,
    total_w: u32,
    total_h: u32,
}

impl WaylandInput {
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
            xdg_to_wl: HashMap::new(),
            pending_output: None,
            stream_output_x: 0,
            stream_output_y: 0,
            stream_output_w: 1920,
            stream_output_h: 1080,
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
        wl_state.total_w = wl_state
            .outputs
            .values()
            .map(|o| (o.x as u32).saturating_add(o.width))
            .max()
            .unwrap_or(1920);
        wl_state.total_h = wl_state
            .outputs
            .values()
            .map(|o| (o.y as u32).saturating_add(o.height))
            .max()
            .unwrap_or(1080);
        log::info!("Input: total compositor extent {}x{}", wl_state.total_w, wl_state.total_h);

        // Virtual pointer
        let virtual_pointer = match wl_state.pointer_manager.as_ref() {
            Some(m) => m.create_virtual_pointer(wl_state.seat.as_ref(), &qh, ()),
            None => {
                return Err(
                    "compositor does not support zwlr_virtual_pointer_manager_v1".to_string()
                );
            }
        };

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

        Ok(WaylandInput {
            conn,
            virtual_pointer,
            virtual_keyboard,
            char_to_keycode,
            outputs: wl_state.outputs,
            stream_output_x: wl_state.stream_output_x,
            stream_output_y: wl_state.stream_output_y,
            stream_output_w: wl_state.stream_output_w,
            stream_output_h: wl_state.stream_output_h,
            total_w: wl_state.total_w,
            total_h: wl_state.total_h,
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
        // Prefer local xdg_output positions (always correct) over what
        // the screencopy backend reports — wl_output::Geometry gives wrong
        // positions for virtual outputs like HEADLESS on Sway.
        let (ox, oy) = if let Some(output) = self
            .outputs
            .values()
            .find(|o| o.name == output_name)
            .or_else(|| self.outputs.values().find(|o| o.width == width && o.height == height))
        {
            (output.x, output.y)
        } else if let (Some(x), Some(y)) = (output_x, output_y) {
            (x, y)
        } else {
            (0, 0)
        };

        self.stream_output_x = ox;
        self.stream_output_y = oy;
        self.stream_output_w = width;
        self.stream_output_h = height;
        // Expand total extent to include this output if needed.
        let needed_w = (ox as u32).saturating_add(width);
        let needed_h = (oy as u32).saturating_add(height);
        if needed_w > self.total_w {
            self.total_w = needed_w;
        }
        if needed_h > self.total_h {
            self.total_h = needed_h;
        }
        log::info!(
            "Input: stream mapped to '{}' {}x{} at ({},{}) — total extent {}x{}",
            output_name,
            width,
            height,
            ox,
            oy,
            self.total_w,
            self.total_h,
        );
    }

    fn pointer_motion(&mut self, time: u32, dx: f64, dy: f64) {
        self.virtual_pointer.motion(time, dx, dy);
        self.virtual_pointer.frame();
    }

    fn pointer_motion_absolute(&mut self, time: u32, x_norm: f64, y_norm: f64) {
        // Map normalized stream coords to global compositor pixel coords.
        let global_x = self.stream_output_x as f64 + x_norm * self.stream_output_w as f64;
        let global_y = self.stream_output_y as f64 + y_norm * self.stream_output_h as f64;
        let abs_x = global_x as u32;
        let abs_y = global_y as u32;
        log::debug!(
            "Input: abs norm=({:.3},{:.3}) -> global=({},{}) extent={}x{} output_offset=({},{})",
            x_norm,
            y_norm,
            abs_x,
            abs_y,
            self.total_w,
            self.total_h,
            self.stream_output_x,
            self.stream_output_y
        );
        self.virtual_pointer.motion_absolute(time, abs_x, abs_y, self.total_w, self.total_h);
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
        if let wl_registry::Event::Global { name, interface, version } = event {
            match interface.as_str() {
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
                    // Request xdg_output for this wl_output to get logical position.
                    if let Some(mgr) = &state.xdg_output_manager {
                        let wl_id = output.id().protocol_id();
                        let xdg_out = mgr.get_xdg_output(&output, qh, ());
                        let xdg_id = xdg_out.id().protocol_id();
                        state.xdg_to_wl.insert(xdg_id, wl_id);
                    }
                }
                "zxdg_output_manager_v1" => {
                    state.xdg_output_manager =
                        Some(registry.bind::<zxdg_output_manager_v1::ZxdgOutputManagerV1, _, _>(
                            name,
                            version.min(3),
                            qh,
                            (),
                        ));
                }
                _ => {}
            }
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
        let pending = state.pending_output.get_or_insert_with(|| (id, OutputInfo::default()));

        match event {
            wl_output::Event::Geometry { x, y, .. } => {
                pending.1.x = x;
                pending.1.y = y;
            }
            wl_output::Event::Mode { flags, width, height, .. } => {
                let is_current = match flags {
                    WEnum::Value(f) => f.contains(wl_output::Mode::Current),
                    _ => false,
                };
                if is_current {
                    pending.1.width = width as u32;
                    pending.1.height = height as u32;
                }
            }
            wl_output::Event::Name { name } => {
                pending.1.name = name;
            }
            wl_output::Event::Done => {
                if let Some((oid, info)) = state.pending_output.take() {
                    // Only insert if this has real data (name + size).
                    // Subsequent Done events after xdg roundtrips have empty pending.
                    if !info.name.is_empty() && info.width > 0 {
                        log::info!(
                            "Input: output '{}' (id={}) {}x{} at ({},{})",
                            info.name,
                            oid,
                            info.width,
                            info.height,
                            info.x,
                            info.y
                        );
                        state.outputs.insert(oid, info);
                        // Recompute total extent so late-arriving outputs (HEADLESS, hotplug) are included.
                        let new_w = state
                            .outputs
                            .values()
                            .map(|o| (o.x as u32).saturating_add(o.width))
                            .max()
                            .unwrap_or(state.total_w);
                        let new_h = state
                            .outputs
                            .values()
                            .map(|o| (o.y as u32).saturating_add(o.height))
                            .max()
                            .unwrap_or(state.total_h);
                        if new_w != state.total_w || new_h != state.total_h {
                            log::info!(
                                "Input: compositor extent updated {}x{} → {}x{}",
                                state.total_w,
                                state.total_h,
                                new_w,
                                new_h
                            );
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
    }
}

delegate_noop!(InputWaylandState: ignore wl_seat::WlSeat);
delegate_noop!(InputWaylandState: ignore zwlr_virtual_pointer_manager_v1::ZwlrVirtualPointerManagerV1);
delegate_noop!(InputWaylandState: ignore zwlr_virtual_pointer_v1::ZwlrVirtualPointerV1);
delegate_noop!(InputWaylandState: ignore zwp_virtual_keyboard_manager_v1::ZwpVirtualKeyboardManagerV1);
delegate_noop!(InputWaylandState: ignore zwp_virtual_keyboard_v1::ZwpVirtualKeyboardV1);
delegate_noop!(InputWaylandState: ignore zxdg_output_manager_v1::ZxdgOutputManagerV1);
