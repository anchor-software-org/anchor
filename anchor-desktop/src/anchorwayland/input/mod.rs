//! Pluggable input-injection backends.
//!
//! `input_plugin` parses pointer/keyboard events from the phone and drives an
//! `InputBackend`. The Wayland implementation lives in [`wayland`]; swapping in
//! a different input system only means providing another `InputBackend`.

pub mod wayland;

pub use wayland::WaylandInput;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutputGeometry {
    pub name: String,
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
}

/// Injects pointer and keyboard events into the host system.
///
/// Coordinates for `pointer_motion_absolute` are normalized stream coordinates
/// in `[0, 1]`; the backend maps them onto the real output layout.
pub trait InputBackend {
    /// Total injectable extent (width, height) in compositor pixels.
    fn total_extent(&self) -> (u32, u32);

    /// Current stream→output mapping as (width, height, x, y).
    fn stream_mapping(&self) -> (u32, u32, i32, i32);

    /// Whether keyboard injection is available.
    fn has_keyboard(&self) -> bool;

    /// Update which output the streamed surface maps onto. `output_x`/`output_y`
    /// are fallbacks used only when the output can't be matched locally.
    fn set_stream_info(
        &mut self,
        output_name: &str,
        width: u32,
        height: u32,
        output_x: Option<i32>,
        output_y: Option<i32>,
    );

    /// Replace the compositor topology used for absolute-pointer mapping.
    fn set_output_layout(&mut self, outputs: &[OutputGeometry]);

    fn pointer_motion(&mut self, time: u32, dx: f64, dy: f64);
    fn pointer_motion_absolute(&mut self, time: u32, x_norm: f64, y_norm: f64);
    fn pointer_button(&mut self, time: u32, button: u32, pressed: bool);
    fn pointer_axis(&mut self, time: u32, horizontal: bool, value: f64);
    fn pointer_frame(&mut self);

    fn type_text(&mut self, time: u32, text: &str);
    /// Press+release a named special key. Returns `true` if recognized.
    fn key_special(&mut self, time: u32, key: &str) -> bool;
    fn key_combo(&mut self, time: u32, modifiers: &[String], key: &str);
    /// Inject a USB HID key transition after translating to the host keycode.
    fn key_hid(&mut self, time: u32, hid_usage: u32, pressed: bool) -> bool;

    /// Flush pending events to the host.
    fn flush(&mut self);
}
