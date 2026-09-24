use std::{
    collections::HashMap,
    ops::Deref,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
        mpsc::{self, SyncSender},
    },
};

/// Encoded frame plus the capture-loop identifier that produced it. Keeping
/// this metadata beside the bytes allows the sender trace to distinguish a
/// stale/coalesced frame from a transport delay without copying the payload.
#[derive(Debug)]
pub struct BroadcastFrame {
    pub source_frame_id: u64,
    pub published_ns: u64,
    pub data: Arc<Vec<u8>>,
}

/// The receiver and loss marker for one Sideboat viewer. The broadcaster sets
/// `gap_detected` when its bounded queue drops an access unit. The viewer must
/// then discard predictive H.264 frames until an IDR arrives.
pub type TracedFrameSubscription = (mpsc::Receiver<Arc<BroadcastFrame>>, Arc<AtomicBool>);

struct TracedSender {
    tx: SyncSender<Arc<BroadcastFrame>>,
    gap_detected: Arc<AtomicBool>,
}

impl Deref for BroadcastFrame {
    type Target = [u8];

    fn deref(&self) -> &Self::Target {
        &self.data
    }
}

/// Broadcasts H264 frames from the Wayland encoder to all subscribed device drain threads.
/// Each device gets its own bounded channel. Slow devices drop frames independently.
pub struct FrameBroadcaster {
    senders: Mutex<HashMap<String, SyncSender<Arc<Vec<u8>>>>>,
    traced_senders: Mutex<HashMap<String, TracedSender>>,
    /// Set by a transport when a receiver needs an IDR.
    pub needs_keyframe: AtomicBool,
    /// Set when the last publish had to drop on a full subscriber queue,
    /// cleared by drain threads once the queue empties. The capture loop
    /// reads this to skip ENCODING while downstream is saturated — a frame
    /// the encoder never saw cannot break the decoder's reference chain,
    /// so skipping costs a frame of framerate instead of a recovery IDR.
    congested: AtomicBool,
}

impl Default for FrameBroadcaster {
    fn default() -> Self {
        Self::new()
    }
}

impl FrameBroadcaster {
    pub fn new() -> Self {
        FrameBroadcaster {
            senders: Mutex::new(HashMap::new()),
            traced_senders: Mutex::new(HashMap::new()),
            needs_keyframe: AtomicBool::new(false),
            congested: AtomicBool::new(false),
        }
    }

    /// Register a device to receive frames. Returns the Receiver for the drain thread.
    pub fn subscribe(&self, device_id: String, buffer_size: usize) -> mpsc::Receiver<Arc<Vec<u8>>> {
        let (tx, rx) = mpsc::sync_channel(buffer_size);
        self.senders.lock().unwrap().insert(device_id, tx);
        rx
    }

    /// Register a Sideboat subscriber while retaining the capture frame ID for
    /// end-to-end tracing.
    pub fn subscribe_traced(
        &self,
        device_id: String,
        buffer_size: usize,
    ) -> TracedFrameSubscription {
        let (tx, rx) = mpsc::sync_channel(buffer_size);
        let gap_detected = Arc::new(AtomicBool::new(false));
        self.traced_senders
            .lock()
            .unwrap()
            .insert(device_id, TracedSender { tx, gap_detected: Arc::clone(&gap_detected) });
        (rx, gap_detected)
    }

    /// Remove a device. The drain thread's Receiver will return Err on next recv.
    pub fn unsubscribe(&self, device_id: &str) {
        self.senders.lock().unwrap().remove(device_id);
        self.traced_senders.lock().unwrap().remove(device_id);
        // A departed drain thread can no longer clear congestion itself.
        self.congested.store(false, Ordering::Relaxed);
    }

    /// True while the last publish dropped on a full subscriber queue. The
    /// capture loop should skip encoding rather than produce an access unit
    /// that will be dropped post-encode (which would force a recovery IDR).
    pub fn has_backpressure(&self) -> bool {
        self.congested.load(Ordering::Relaxed)
    }

    /// Called by a drain thread after it empties its subscription queue —
    /// the next publish has room again, so the capture loop may resume
    /// encoding. Also safe to call on unsubscribe/teardown.
    pub fn clear_backpressure(&self) {
        self.congested.store(false, Ordering::Relaxed);
    }

    /// Called by Wayland encoder every frame. Sends Arc<frame> to all subscribers.
    /// Returns true if all subscribers received the frame, false if any were dropped.
    pub fn publish(&self, data: Vec<u8>) -> bool {
        self.publish_frame(0, data)
    }

    /// Publish an encoded access unit with its capture-loop identifier.
    pub fn publish_frame(&self, source_frame_id: u64, data: Vec<u8>) -> bool {
        let mut senders = self.senders.lock().unwrap();
        let mut traced_senders = self.traced_senders.lock().unwrap();
        // Skip the Arc allocs and timestamp read entirely when nobody is
        // listening — idle capture with no connected device is the common
        // case outside active sessions.
        if senders.is_empty() && traced_senders.is_empty() {
            return true;
        }
        let data = Arc::new(data);
        let frame = Arc::new(BroadcastFrame {
            source_frame_id,
            published_ns: crate::anchorwayland::frame_trace::mono_ns(),
            data: Arc::clone(&data),
        });
        let legacy_frame = data;
        let mut all_delivered = true;
        senders.retain(|id, tx| match tx.try_send(Arc::clone(&legacy_frame)) {
            Ok(()) => true,
            Err(mpsc::TrySendError::Full(_)) => {
                log::debug!("FrameBroadcaster: dropped frame for slow device {}", id);
                crate::anchorwayland::frame_trace::event!(
                    "broadcaster_drop",
                    {
                        "device_id": id,
                        "source_frame_id": frame.source_frame_id,
                        "encoded_bytes": frame.len(),
                        "reason": "subscriber_queue_full",
                    }
                );
                all_delivered = false;
                true
            }
            Err(mpsc::TrySendError::Disconnected(_)) => {
                log::info!("FrameBroadcaster: device {} disconnected, removing", id);
                crate::anchorwayland::frame_trace::event!(
                    "broadcaster_disconnect",
                    {
                        "device_id": id,
                        "source_frame_id": frame.source_frame_id,
                        "reason": "subscriber_disconnected",
                    }
                );
                false
            }
        });
        traced_senders.retain(|id, sender| match sender.tx.try_send(Arc::clone(&frame)) {
            Ok(()) => true,
            Err(mpsc::TrySendError::Full(_)) => {
                log::debug!("FrameBroadcaster: dropped traced frame for slow device {}", id);
                all_delivered = false;
                // The frame never reached this H.264 decoder. Do not allow
                // a later predictive access unit to use it as a reference.
                sender.gap_detected.store(true, Ordering::Release);
                crate::anchorwayland::frame_trace::event!(
                    "broadcaster_drop",
                    {
                        "device_id": id,
                        "source_frame_id": frame.source_frame_id,
                        "encoded_bytes": frame.len(),
                        "reason": "subscriber_queue_full",
                    }
                );
                true
            }
            Err(mpsc::TrySendError::Disconnected(_)) => {
                log::info!("FrameBroadcaster: traced device {} disconnected, removing", id);
                false
            }
        });
        if all_delivered {
            crate::anchorwayland::frame_trace::event!(
                "broadcaster_accept",
                {
                    "source_frame_id": frame.source_frame_id,
                    "encoded_bytes": frame.len(),
                    "subscriber_count": senders.len() + traced_senders.len(),
                }
            );
        }
        // Any dropped access unit breaks the H.264 reference chain — the
        // decoder's next P-frame would reference a frame it never received.
        // This applies to SDK subscribers too: a publish-side drop never
        // reaches their transport recovery, so the encoder must emit an IDR.
        if !all_delivered {
            self.needs_keyframe.store(true, Ordering::Relaxed);
            self.congested.store(true, Ordering::Relaxed);
        }
        all_delivered
    }

    /// Check and clear the keyframe request flag.
    pub fn take_keyframe_request(&self) -> bool {
        self.needs_keyframe.swap(false, Ordering::Relaxed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::thread;
    use std::time::Duration;

    #[test]
    fn broadcaster_single_subscriber_receives_frame() {
        let bc = FrameBroadcaster::new();
        let rx = bc.subscribe("device-1".to_string(), 4);

        bc.publish(vec![1, 2, 3]);

        let frame = rx.recv_timeout(Duration::from_secs(1)).unwrap();
        assert_eq!(*frame, vec![1, 2, 3]);
    }

    #[test]
    fn broadcaster_multiple_subscribers_all_receive() {
        let bc = FrameBroadcaster::new();
        let rx1 = bc.subscribe("device-1".to_string(), 4);
        let rx2 = bc.subscribe("device-2".to_string(), 4);

        bc.publish(vec![10, 20]);

        let f1 = rx1.recv_timeout(Duration::from_secs(1)).unwrap();
        let f2 = rx2.recv_timeout(Duration::from_secs(1)).unwrap();
        assert_eq!(*f1, vec![10, 20]);
        assert_eq!(*f2, vec![10, 20]);
        assert!(Arc::ptr_eq(&f1, &f2));
    }

    #[test]
    fn broadcaster_unsubscribe_stops_delivery() {
        let bc = FrameBroadcaster::new();
        let rx = bc.subscribe("device-1".to_string(), 4);

        bc.unsubscribe("device-1");
        bc.publish(vec![1, 2, 3]);

        assert!(rx.recv_timeout(Duration::from_millis(100)).is_err());
    }

    #[test]
    fn broadcaster_full_channel_drops_frame() {
        let bc = FrameBroadcaster::new();
        let rx = bc.subscribe("device-1".to_string(), 2);

        bc.publish(vec![1]);
        bc.publish(vec![2]);
        bc.publish(vec![3]);

        let f1 = rx.recv_timeout(Duration::from_secs(1)).unwrap();
        let f2 = rx.recv_timeout(Duration::from_secs(1)).unwrap();
        assert_eq!(*f1, vec![1]);
        assert_eq!(*f2, vec![2]);
        assert!(rx.recv_timeout(Duration::from_millis(100)).is_err());
        assert!(bc.take_keyframe_request());

        bc.publish(vec![4]);
        let f4 = rx.recv_timeout(Duration::from_secs(1)).unwrap();
        assert_eq!(*f4, vec![4]);
    }

    #[test]
    fn broadcaster_disconnected_subscriber_auto_removed() {
        let bc = FrameBroadcaster::new();
        let rx = bc.subscribe("device-1".to_string(), 4);
        let _rx2 = bc.subscribe("device-2".to_string(), 4);

        drop(rx);
        bc.publish(vec![1]);

        let senders = bc.senders.lock().unwrap();
        assert_eq!(senders.len(), 1);
        assert!(senders.contains_key("device-2"));
    }

    #[test]
    fn sdk_screen_queue_drop_forces_keyframe() {
        // A publish-side drop breaks the decoder's reference chain and never
        // reaches transport recovery — the encoder must emit an IDR.
        let bc = FrameBroadcaster::new();
        let _rx = bc.subscribe("sdk-screen:device-1".to_string(), 1);
        bc.publish(vec![1]);
        assert!(!bc.publish(vec![2]));
        assert!(bc.take_keyframe_request());
    }

    #[test]
    fn traced_subscriber_receives_capture_frame_id_without_changing_payload() {
        let bc = FrameBroadcaster::new();
        let (rx, gap_detected) = bc.subscribe_traced("sdk-screen:device-1".to_string(), 1);

        assert!(bc.publish_frame(42, vec![1, 2, 3]));
        let frame = rx.recv_timeout(Duration::from_secs(1)).unwrap();
        assert_eq!(frame.source_frame_id, 42);
        assert!(frame.published_ns > 0);
        assert_eq!(frame.data.as_slice(), &[1, 2, 3]);
        assert!(!gap_detected.load(Ordering::Acquire));
    }

    #[test]
    fn traced_queue_drop_marks_only_that_viewer_for_idr_recovery() {
        let bc = FrameBroadcaster::new();
        let (_slow_rx, slow_gap) = bc.subscribe_traced("sdk-screen:slow".to_string(), 1);
        let (fast_rx, fast_gap) = bc.subscribe_traced("sdk-screen:fast".to_string(), 2);

        assert!(bc.publish_frame(1, vec![1]));
        assert!(!bc.publish_frame(2, vec![2]));

        assert!(slow_gap.load(Ordering::Acquire));
        assert!(!fast_gap.load(Ordering::Acquire));
        assert_eq!(fast_rx.recv_timeout(Duration::from_secs(1)).unwrap().source_frame_id, 1);
        assert_eq!(fast_rx.recv_timeout(Duration::from_secs(1)).unwrap().source_frame_id, 2);
    }

    #[test]
    fn broadcaster_no_subscribers_is_noop() {
        let bc = FrameBroadcaster::new();
        bc.publish(vec![1, 2, 3]);
    }

    #[test]
    fn broadcaster_concurrent_publish_and_receive() {
        let bc = Arc::new(FrameBroadcaster::new());
        let rx = bc.subscribe("device-1".to_string(), 64);

        let bc_clone = bc.clone();
        let producer = thread::spawn(move || {
            for i in 0..100u8 {
                bc_clone.publish(vec![i]);
            }
        });

        let consumer = thread::spawn(move || {
            let mut count = 0;
            while let Ok(_frame) = rx.recv_timeout(Duration::from_secs(2)) {
                count += 1;
            }
            count
        });

        producer.join().unwrap();
        drop(bc);
        let received = consumer.join().unwrap();
        assert!(received > 0);
        assert!(received <= 100);
    }
}
