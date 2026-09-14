//! The `Plugin` trait and shared frame buffer used by all plugins.

use crate::anchorapp::event::AnchorEvent;
use std::sync::mpsc::{Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;

#[derive(Debug, Clone)]
pub struct SharedFrameBuffer {
    pub sequence: u64,
    pub width: u32,
    pub height: u32,
    pub stride: u32,
    pub pixel_data: Vec<u8>,
}

pub trait Plugin {
    fn run(&mut self) -> Option<JoinHandle<()>>;

    fn init(
        plugin_rx: Option<Receiver<AnchorEvent>>,
        broker_tx: Option<Sender<AnchorEvent>>,
        frame_buffer: Arc<Mutex<Option<SharedFrameBuffer>>>,
    ) -> Self;
}
