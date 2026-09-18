// Use Anchor's shipping capture and encoder modules verbatim. The small
// `anchorapp::settings` module in main.rs supplies an explicit benchmark
// configuration rather than reading the application's user settings file.
#[path = "../../../src/anchorwayland/capture_backend.rs"]
pub mod capture_backend;
#[path = "../../../src/anchorwayland/screencopy.rs"]
pub mod screencopy;
#[path = "../../../src/anchorwayland/vaapi_encoder.rs"]
pub mod vaapi_encoder;
#[path = "../../../src/anchorwayland/wayland_dispatch.rs"]
pub mod wayland_dispatch;
#[path = "../../../src/anchorwayland/wayland_mem.rs"]
pub mod wayland_mem;
#[path = "../../../src/anchorwayland/wayland_objects.rs"]
pub mod wayland_objects;
