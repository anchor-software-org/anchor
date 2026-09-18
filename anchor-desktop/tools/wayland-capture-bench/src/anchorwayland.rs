// Reuse the exact capture implementation that Anchor ships. Keeping these
// paths explicit prevents this small benchmark from accidentally inheriting
// encoder, broadcaster, GUI, or network dependencies.
#[path = "../../../src/anchorwayland/capture_backend.rs"]
pub mod capture_backend;
#[path = "../../../src/anchorwayland/screencopy.rs"]
pub mod screencopy;
#[path = "../../../src/anchorwayland/wayland_dispatch.rs"]
pub mod wayland_dispatch;
#[path = "../../../src/anchorwayland/wayland_mem.rs"]
pub mod wayland_mem;
#[path = "../../../src/anchorwayland/wayland_objects.rs"]
pub mod wayland_objects;
