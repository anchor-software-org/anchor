use crate::anchorapp::event::{AnchorEvent, AnchorTarget};
use std::{
    collections::HashMap,
    sync::mpsc::{Receiver, Sender},
    thread,
};

pub struct MessageHandler {
    inbound_rx: Receiver<AnchorEvent>,
    to_gui: Sender<AnchorEvent>,
    to_wayland: Sender<AnchorEvent>,
    to_service: HashMap<String, Sender<AnchorEvent>>,
    to_network: Sender<AnchorEvent>,
}

impl MessageHandler {
    pub fn new(
        inbound_rx: Receiver<AnchorEvent>,
        to_gui: Sender<AnchorEvent>,
        to_wayland: Sender<AnchorEvent>,
        to_network: Sender<AnchorEvent>,
    ) -> Self {
        MessageHandler { inbound_rx, to_gui, to_wayland, to_network, to_service: HashMap::new() }
    }

    pub fn add_service(&mut self, id: String, tx: Sender<AnchorEvent>) {
        self.to_service.insert(id, tx);
    }

    pub fn start(self) {
        thread::spawn(move || {
            while let Ok(event) = self.inbound_rx.recv() {
                match &event.target {
                    AnchorTarget::Gui => {
                        let _ = self.to_gui.send(event);
                    }
                    AnchorTarget::Wayland => {
                        let _ = self.to_wayland.send(event);
                    }
                    AnchorTarget::Service(id) => {
                        if let Some(tx) = self.to_service.get(id) {
                            let _ = tx.send(event);
                        } else {
                            log::warn!("Service {} not found", id);
                        }
                    }
                    AnchorTarget::Network | AnchorTarget::Device | AnchorTarget::DeviceId(_) => {
                        let _ = self.to_network.send(event);
                    }
                    AnchorTarget::Broadcast => {
                        let _ = self.to_gui.send(event.clone());
                        let _ = self.to_wayland.send(event.clone());
                        for tx in self.to_service.values() {
                            let _ = tx.send(event.clone());
                        }
                    }
                }
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::anchorapp::event::{AnchorEvent, AnchorMessage, AnchorTarget};
    use std::sync::mpsc;
    use std::time::Duration;

    fn make_event(target: AnchorTarget) -> AnchorEvent {
        AnchorEvent { target, message: AnchorMessage::Generic("test".to_string()) }
    }

    #[test]
    fn routes_gui_to_gui_channel() {
        let (broker_tx, broker_rx) = mpsc::channel();
        let (gui_tx, gui_rx) = mpsc::channel();
        let (_way_tx, _way_rx) = mpsc::channel();
        let (_net_tx, _net_rx) = mpsc::channel();

        let handler = MessageHandler::new(broker_rx, gui_tx, _way_tx, _net_tx);
        handler.start();

        broker_tx.send(make_event(AnchorTarget::Gui)).unwrap();
        let event = gui_rx.recv_timeout(Duration::from_secs(1)).unwrap();
        assert_eq!(event.target, AnchorTarget::Gui);
    }

    #[test]
    fn routes_wayland_to_wayland_channel() {
        let (broker_tx, broker_rx) = mpsc::channel();
        let (_gui_tx, _gui_rx) = mpsc::channel();
        let (way_tx, way_rx) = mpsc::channel();
        let (_net_tx, _net_rx) = mpsc::channel();

        let handler = MessageHandler::new(broker_rx, _gui_tx, way_tx, _net_tx);
        handler.start();

        broker_tx.send(make_event(AnchorTarget::Wayland)).unwrap();
        let event = way_rx.recv_timeout(Duration::from_secs(1)).unwrap();
        assert_eq!(event.target, AnchorTarget::Wayland);
    }

    #[test]
    fn routes_device_to_network_channel() {
        let (broker_tx, broker_rx) = mpsc::channel();
        let (_gui_tx, _gui_rx) = mpsc::channel();
        let (_way_tx, _way_rx) = mpsc::channel();
        let (net_tx, net_rx) = mpsc::channel();

        let handler = MessageHandler::new(broker_rx, _gui_tx, _way_tx, net_tx);
        handler.start();

        broker_tx.send(make_event(AnchorTarget::Device)).unwrap();
        let event = net_rx.recv_timeout(Duration::from_secs(1)).unwrap();
        assert_eq!(event.target, AnchorTarget::Device);
    }

    #[test]
    fn routes_service_to_registered_service() {
        let (broker_tx, broker_rx) = mpsc::channel();
        let (_gui_tx, _gui_rx) = mpsc::channel();
        let (_way_tx, _way_rx) = mpsc::channel();
        let (_net_tx, _net_rx) = mpsc::channel();
        let (svc_tx, svc_rx) = mpsc::channel();

        let mut handler = MessageHandler::new(broker_rx, _gui_tx, _way_tx, _net_tx);
        handler.add_service("sms".to_string(), svc_tx);
        handler.start();

        broker_tx.send(make_event(AnchorTarget::Service("sms".to_string()))).unwrap();
        let event = svc_rx.recv_timeout(Duration::from_secs(1)).unwrap();
        assert_eq!(event.target, AnchorTarget::Service("sms".to_string()));
    }

    #[test]
    fn broadcast_reaches_gui_and_wayland() {
        let (broker_tx, broker_rx) = mpsc::channel();
        let (gui_tx, gui_rx) = mpsc::channel();
        let (way_tx, way_rx) = mpsc::channel();
        let (_net_tx, _net_rx) = mpsc::channel();

        let handler = MessageHandler::new(broker_rx, gui_tx, way_tx, _net_tx);
        handler.start();

        broker_tx.send(make_event(AnchorTarget::Broadcast)).unwrap();

        gui_rx.recv_timeout(Duration::from_secs(1)).unwrap();
        way_rx.recv_timeout(Duration::from_secs(1)).unwrap();
    }
}
