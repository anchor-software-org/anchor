//! Anchor's sole device listener: the SDK QUIC endpoint on UDP port 5027.
//! Sessions are accepted only for a client certificate already present in the
//! desktop pairing store.

use std::{
    collections::{HashMap, HashSet},
    sync::Arc,
    thread,
    time::{Duration, Instant},
};

use anchor_sdk::{
    AcceptedSession, AnchorListener, IncomingConnection, ListenerConfig, ListenerIdentity,
    PairingSession, SessionEvent, SessionIdentity, camera, clipboard, commands, device, files,
    input, media, notifications, screen, sms, v1,
};
use base64::Engine;
use sha2::{Digest, Sha256};

use super::DeviceRegistry;
use crate::anchorapp::{
    clipboard_plugin::SdkClipboardBinding,
    event::{AnchorEvent, AnchorMessage, AnchorTarget, PairingRequest, SharedPairingRequest},
    sdk_camera::{SdkCameraBinding, SdkCameraSender},
    sdk_commands::{SdkCommandsBinding, SdkCommandsSender},
    sdk_device::{SdkDeviceBinding, SdkDeviceSender},
    sdk_files::{SdkFilesBinding, SdkFilesSender},
    sdk_media::{SdkMediaBinding, SdkMediaSender},
    sdk_notifications::{SdkNotificationsBinding, SdkNotificationsSender},
    sdk_screen::{SdkScreenBinding, SdkScreenSender},
    sdk_sms::{SdkSmsBinding, SdkSmsSender},
    tls::AnchorIdentity,
};

pub(crate) struct SdkServer {
    identity: AnchorIdentity,
    registry: DeviceRegistry,
    clipboard: SdkClipboardBinding,
    media: SdkMediaBinding,
    notifications: SdkNotificationsBinding,
    files: SdkFilesBinding,
    screen: SdkScreenBinding,
    sms: SdkSmsBinding,
    commands: SdkCommandsBinding,
    camera: SdkCameraBinding,
    device: SdkDeviceBinding,
    broker_tx: std::sync::mpsc::Sender<AnchorEvent>,
    pairing_request: SharedPairingRequest,
}

impl SdkServer {
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn new(
        identity: AnchorIdentity,
        registry: DeviceRegistry,
        clipboard: SdkClipboardBinding,
        media: SdkMediaBinding,
        notifications: SdkNotificationsBinding,
        files: SdkFilesBinding,
        screen: SdkScreenBinding,
        sms: SdkSmsBinding,
        commands: SdkCommandsBinding,
        camera: SdkCameraBinding,
        device: SdkDeviceBinding,
        broker_tx: std::sync::mpsc::Sender<AnchorEvent>,
        pairing_request: SharedPairingRequest,
    ) -> Self {
        Self {
            identity,
            registry,
            clipboard,
            media,
            notifications,
            files,
            screen,
            sms,
            commands,
            camera,
            device,
            broker_tx,
            pairing_request,
        }
    }

    pub(crate) fn spawn(self) {
        thread::Builder::new()
            .name("anchor-sdk-quic-listener".into())
            .spawn(move || {
                let runtime = match tokio::runtime::Builder::new_multi_thread()
                    .worker_threads(2)
                    .enable_all()
                    .build()
                {
                    Ok(runtime) => runtime,
                    Err(error) => {
                        log::error!("SDK QUIC runtime failed: {error}");
                        return;
                    }
                };
                runtime.block_on(self.run());
            })
            .expect("SDK QUIC listener thread must start");
    }

    async fn run(self) {
        let identity = session_identity(&self.identity);
        let listener_identity = ListenerIdentity::from_pem(
            self.identity.certificate_pem.clone(),
            self.identity.private_key_pem.clone(),
        );
        let desktop_certificate_der = match listener_identity.certificate_der() {
            Ok(certificate) => certificate,
            Err(error) => {
                log::error!("SDK listener identity is invalid: {error}");
                return;
            }
        };
        let listener = match AnchorListener::bind(ListenerConfig {
            bind_address: "0.0.0.0:5027".parse().expect("valid SDK listener address"),
            identity: listener_identity,
            session_identity: identity.clone(),
        }) {
            Ok(listener) => listener,
            Err(error) => {
                log::error!("Failed to bind SDK QUIC port 5027: {error}");
                return;
            }
        };
        log::info!("Anchor SDK QUIC listener on port 5027");
        let desktop_safety_number = pairing_safety_number(&desktop_certificate_der);
        let desktop_device_id = self.identity.device_id.clone();
        log::info!(
            "Anchor SDK endpoint catalog: {}",
            identity
                .endpoints
                .iter()
                .flat_map(|endpoint| endpoint.capabilities.iter())
                .map(|capability| capability.name.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        );
        loop {
            let incoming = match listener.accept().await {
                Ok(Some(incoming)) => incoming,
                Ok(None) => break,
                Err(error) => {
                    log::warn!("SDK listener stopped accepting connections: {error}");
                    break;
                }
            };
            log::info!("SDK QUIC incoming connection");
            let registry = self.registry.clone();
            let clipboard = self.clipboard.clone();
            let media = self.media.clone();
            let notifications = self.notifications.clone();
            let files = self.files.clone();
            let screen = self.screen.clone();
            let sms = self.sms.clone();
            let commands = self.commands.clone();
            let camera = self.camera.clone();
            let device = self.device.clone();
            let broker_tx = self.broker_tx.clone();
            let pairing_request = self.pairing_request.clone();
            let desktop_safety_number = desktop_safety_number.clone();
            let desktop_certificate_der = desktop_certificate_der.clone();
            let desktop_device_id = desktop_device_id.clone();
            tokio::spawn(async move {
                if let Err(error) = handle_session(
                    incoming,
                    registry,
                    clipboard,
                    media,
                    notifications,
                    files,
                    screen,
                    sms,
                    commands,
                    camera,
                    device,
                    broker_tx,
                    pairing_request,
                    desktop_safety_number,
                    desktop_certificate_der,
                    desktop_device_id,
                )
                .await
                {
                    log::warn!("SDK session failed: {error}");
                }
            });
        }
    }
}

#[allow(clippy::too_many_arguments)]
async fn handle_session(
    incoming: IncomingConnection,
    registry: DeviceRegistry,
    clipboard_binding: SdkClipboardBinding,
    media_binding: SdkMediaBinding,
    notifications_binding: SdkNotificationsBinding,
    files_binding: SdkFilesBinding,
    screen_binding: SdkScreenBinding,
    sms_binding: SdkSmsBinding,
    commands_binding: SdkCommandsBinding,
    camera_binding: SdkCameraBinding,
    device_binding: SdkDeviceBinding,
    broker_tx: std::sync::mpsc::Sender<AnchorEvent>,
    pairing_request: SharedPairingRequest,
    desktop_safety_number: String,
    desktop_certificate_der: Vec<u8>,
    desktop_device_id: String,
) -> Result<(), anchor_sdk::SessionError> {
    let session = match incoming.accept().await.map_err(|error| {
        log::debug!("SDK connection rejected before session dispatch: {error}");
        anchor_sdk::SessionError::UnexpectedRecord("SDK connection could not be established")
    })? {
        AcceptedSession::Session(session) => session,
        AcceptedSession::Pairing(pairing) => {
            return handle_pairing(
                *pairing,
                registry,
                broker_tx,
                pairing_request,
                desktop_safety_number,
                desktop_certificate_der,
                desktop_device_id,
            )
            .await;
        }
    };
    let certificate = session.peer_certificate_der().ok_or(
        anchor_sdk::SessionError::UnexpectedRecord("SDK peer did not present a certificate"),
    )?;
    let device_id = match registry.check_device(&certificate) {
        super::DeviceCheckResult::Trusted(device_id) => device_id,
        super::DeviceCheckResult::Unknown(fingerprint) => {
            log::warn!("Rejecting unpaired SDK peer (fingerprint {fingerprint})");
            session.close(0, b"pairing required");
            return Ok(());
        }
    };
    registry.mark_last_seen(&device_id);
    // The registry is deliberately only a pairing/UI state store. The guard
    // mirrors this authenticated QUIC session and clears state on every exit.
    let _sdk_connection = registry.connect_sdk(&device_id);
    let mut file_sessions = HashSet::new();
    let mut file_capabilities = HashMap::new();
    let mut command_sessions = HashSet::new();
    let mut screen_sessions = HashSet::new();
    let mut camera_sessions = HashSet::new();
    let mut input_sessions = HashSet::new();
    let mut device_sessions = HashSet::new();
    // Track host pointer buttons per authenticated SDK session.  A mobile
    // process can disappear between a press and release; without this guard
    // the compositor keeps dragging until another release happens to arrive.
    let mut pressed_buttons = HashSet::<u32>::new();
    let mut file_hashes = HashMap::<String, Vec<u8>>::new();
    // StreamOpen arrives before the FileContentStart record because the
    // sender cannot know its QUIC stream ID until MsQuic has opened it. Keep
    // the association in shared state so the receiver task can wait briefly
    // for the typed binding without buffering the file in memory.
    let stream_transfers = Arc::new(tokio::sync::Mutex::new(HashMap::<u64, String>::new()));
    // File COMPLETE (control record) can arrive before the data stream has
    // been fully forwarded as file_chunk JSON. Hold the complete packet until
    // the stream task signals it has finished, so filetransfer never sees a
    // premature file_complete for an empty file.
    let pending_file_completes: Arc<tokio::sync::Mutex<HashMap<String, String>>> =
        Arc::new(tokio::sync::Mutex::new(HashMap::new()));
    let completed_file_streams: Arc<tokio::sync::Mutex<HashMap<String, Instant>>> =
        Arc::new(tokio::sync::Mutex::new(HashMap::new()));

    loop {
        match session.next_event().await? {
            SessionEvent::CapabilityOpenRequested {
                request_id,
                capability_session_id,
                endpoint_id,
                capability_name,
                capability_major,
            } if endpoint_id == clipboard::ENDPOINT_ID
                && capability_name == clipboard::CAPABILITY_NAME
                && capability_major == clipboard::CAPABILITY_MAJOR =>
            {
                let capability = session
                    .accept_capability(
                        request_id,
                        capability_session_id,
                        &endpoint_id,
                        &capability_name,
                        capability_major,
                    )
                    .await?;
                clipboard_binding.attach(
                    device_id.clone(),
                    crate::anchorapp::sdk_clipboard::SdkClipboardSender::new(capability),
                    node_id(&device_id),
                );
                registry.register_sdk_capability(&device_id, "clipboard");
                log::info!("SDK clipboard capability opened by {device_id}");
            }
            SessionEvent::CapabilityOpenRequested {
                request_id,
                capability_session_id,
                endpoint_id,
                capability_name,
                capability_major,
            } if endpoint_id == device::ENDPOINT_ID
                && capability_name == device::CAPABILITY_NAME
                && capability_major == device::CAPABILITY_MAJOR =>
            {
                let capability = session
                    .accept_capability(
                        request_id,
                        capability_session_id,
                        &endpoint_id,
                        &capability_name,
                        capability_major,
                    )
                    .await?;
                registry.register_sdk_capability(&device_id, "device");
                device_binding.attach(device_id.clone(), SdkDeviceSender::new(capability));
                device_sessions.insert(capability_session_id);
                log::info!("SDK device capability opened by {device_id}");
            }
            SessionEvent::CapabilityOpenRequested {
                request_id,
                capability_session_id,
                endpoint_id,
                capability_name,
                capability_major,
            } if endpoint_id == notifications::ENDPOINT_ID
                && capability_name == notifications::CAPABILITY_NAME
                && capability_major == notifications::CAPABILITY_MAJOR =>
            {
                let capability = session
                    .accept_capability(
                        request_id,
                        capability_session_id,
                        &endpoint_id,
                        &capability_name,
                        capability_major,
                    )
                    .await?;
                notifications_binding
                    .attach(device_id.clone(), SdkNotificationsSender::new(capability));
                registry.register_sdk_capability(&device_id, "notifications");
                log::info!("SDK notifications capability opened by {device_id}");
            }
            SessionEvent::CapabilityOpenRequested {
                request_id,
                capability_session_id,
                endpoint_id,
                capability_name,
                capability_major,
            } if endpoint_id == input::ENDPOINT_ID
                && capability_name == input::CAPABILITY_NAME
                && capability_major == input::CAPABILITY_MAJOR =>
            {
                session
                    .accept_capability(
                        request_id,
                        capability_session_id,
                        &endpoint_id,
                        &capability_name,
                        capability_major,
                    )
                    .await?;
                input_sessions.insert(capability_session_id);
                registry.register_sdk_capability(&device_id, "input");
                log::info!("SDK input capability opened by {device_id}");
            }
            SessionEvent::CapabilityOpenRequested {
                request_id,
                capability_session_id,
                endpoint_id,
                capability_name,
                capability_major,
            } if endpoint_id == media::ENDPOINT_ID
                && capability_name == media::CAPABILITY_NAME
                && capability_major == media::CAPABILITY_MAJOR =>
            {
                let capability = session
                    .accept_capability(
                        request_id,
                        capability_session_id,
                        &endpoint_id,
                        &capability_name,
                        capability_major,
                    )
                    .await?;
                media_binding.attach(device_id.clone(), SdkMediaSender::new(capability));
                registry.register_sdk_capability(&device_id, "media");
                log::info!("SDK media capability opened by {device_id}");
            }
            SessionEvent::CapabilityOpenRequested {
                request_id,
                capability_session_id,
                endpoint_id,
                capability_name,
                capability_major,
            } if endpoint_id == screen::ENDPOINT_ID
                && capability_major == 1
                && matches!(
                    capability_name.as_str(),
                    screen::CAPABILITY_NAME
                        | camera::CAPABILITY_NAME
                        | files::CAPABILITY_NAME
                        | sms::CAPABILITY_NAME
                        | commands::CAPABILITY_NAME
                ) =>
            {
                let capability = session
                    .accept_capability(
                        request_id,
                        capability_session_id,
                        &endpoint_id,
                        &capability_name,
                        capability_major,
                    )
                    .await?;
                registry.register_sdk_capability(&device_id, &capability_name);
                log::info!("SDK {capability_name} capability opened by {device_id}");
                if capability_name == screen::CAPABILITY_NAME {
                    let sender = SdkScreenSender::new(capability);
                    screen_sessions.insert(capability_session_id);
                    screen_binding.attach_device(device_id.clone(), sender);
                } else if capability_name == files::CAPABILITY_NAME {
                    file_sessions.insert(capability_session_id);
                    file_capabilities.insert(capability_session_id, capability.clone());
                    files_binding.attach(device_id.clone(), SdkFilesSender::new(capability));
                } else if capability_name == sms::CAPABILITY_NAME {
                    sms_binding.attach(device_id.clone(), SdkSmsSender::new(capability));
                } else if capability_name == commands::CAPABILITY_NAME {
                    command_sessions.insert(capability_session_id);
                    commands_binding.attach(device_id.clone(), SdkCommandsSender::new(capability));
                    let _ = broker_tx.send(AnchorEvent {
                        target: AnchorTarget::Service("commands".into()),
                        message: AnchorMessage::Json(
                            serde_json::json!({"plugin_id":"commands", "type":"list"}).to_string(),
                        ),
                    });
                } else if capability_name == camera::CAPABILITY_NAME {
                    camera_sessions.insert(capability_session_id);
                    camera_binding.attach(device_id.clone(), SdkCameraSender::new(capability));
                }
            }
            SessionEvent::CapabilityRecord { capability_session_id, type_url, payload }
                if file_sessions.contains(&capability_session_id)
                    && type_url == files::OFFER_TYPE_URL =>
            {
                let Ok(offer) = files::decode_offer(&payload) else {
                    log::warn!("Ignoring malformed SDK file offer from {device_id}");
                    continue;
                };
                let accepted = valid_file_offer(&offer);
                if let Some(capability) = file_capabilities.get(&capability_session_id).cloned() {
                    let decision =
                        files::FileDecision { transfer_id: offer.transfer_id.clone(), accepted };
                    capability
                        .send_record(files::DECISION_TYPE_URL, files::encode_decision(decision))
                        .await?;
                }
                if accepted {
                    let transfer_id = hex_bytes(&offer.transfer_id);
                    file_hashes.insert(transfer_id.clone(), offer.sha256.clone());
                    let sms_unique_identifier =
                        crate::anchorapp::attachment_pending::claim_for_offer(
                            transfer_id.clone(),
                            &offer.filename,
                        );
                    if let Some(ref uid) = sms_unique_identifier {
                        log::info!(
                            "File transfer correlated to SMS attachment {} via {}: {}",
                            uid,
                            transfer_id,
                            offer.filename
                        );
                    }
                    let mut packet = serde_json::json!({
                        "plugin_id": "filetransfer",
                        "type": "file_offer",
                        "transfer_id": transfer_id,
                        "name": offer.filename,
                        "size": offer.byte_length,
                        // QUIC chooses receive boundaries independently of
                        // the sender's writes; let the existing receiver use
                        // the signed completion hash instead of guessing a
                        // chunk count from the advertised byte length.
                        "chunks": 0,
                    });
                    if let Some(uid) = sms_unique_identifier {
                        packet["sms_unique_identifier"] = serde_json::Value::String(uid);
                    }
                    let _ = broker_tx.send(AnchorEvent {
                        target: AnchorTarget::Service("filetransfer".into()),
                        message: AnchorMessage::Json(packet.to_string()),
                    });
                } else {
                    log::warn!("Rejected SDK file offer from {device_id}");
                }
            }
            SessionEvent::CapabilityRecord { capability_session_id, type_url, payload }
                if file_sessions.contains(&capability_session_id)
                    && type_url == files::CONTENT_START_TYPE_URL =>
            {
                if let Ok(start) = files::decode_content_start(&payload)
                    && start.transfer_id.len() == 16
                {
                    stream_transfers
                        .lock()
                        .await
                        .insert(start.quic_stream_id, hex_bytes(&start.transfer_id));
                } else {
                    log::warn!("Ignoring malformed SDK file content binding from {device_id}");
                }
            }
            SessionEvent::CapabilityRecord { capability_session_id, type_url, payload }
                if file_sessions.contains(&capability_session_id)
                    && type_url == files::COMPLETE_TYPE_URL =>
            {
                if let Ok(complete) = files::decode_complete(&payload)
                    && complete.transfer_id.len() == 16
                    && complete.sha256.len() == 32
                {
                    let transfer_id = hex_bytes(&complete.transfer_id);
                    let Some(expected_hash) = file_hashes.remove(&transfer_id) else {
                        log::warn!("Ignoring SDK completion for unknown transfer from {device_id}");
                        continue;
                    };
                    if expected_hash != complete.sha256 {
                        log::warn!(
                            "Ignoring SDK completion with hash different from its offer from {device_id}"
                        );
                        continue;
                    }
                    let packet = serde_json::json!({
                        "plugin_id": "filetransfer",
                        "type": "file_complete",
                        "transfer_id": transfer_id.clone(),
                        "sha256": hex_bytes(&complete.sha256),
                    });
                    let packet_str = packet.to_string();
                    // Check if the data stream has already finished (COMPLETE
                    // arrived after stream). In that case forward immediately.
                    let stream_already_done = {
                        let mut guard = completed_file_streams.lock().await;
                        if guard.remove(&transfer_id).is_some() {
                            true
                        } else {
                            // Also reap stale completed entries (5 min TTL)
                            let now = Instant::now();
                            guard.retain(|_, t| now.duration_since(*t) < Duration::from_secs(300));
                            false
                        }
                    };
                    if stream_already_done {
                        log::debug!(
                            "Stream already finished for {}, forwarding file_complete immediately",
                            transfer_id
                        );
                        let _ = broker_tx.send(AnchorEvent {
                            target: AnchorTarget::Service("filetransfer".into()),
                            message: AnchorMessage::Json(packet_str),
                        });
                    } else {
                        // Check if stream is still active or hasn't even been
                        // opened yet (CONTENT_START not yet received). In
                        // either case hold until the stream task signals done.
                        let still_streaming = {
                            let guard = stream_transfers.lock().await;
                            guard.values().any(|v| v == &transfer_id)
                        };
                        // For SDK files, a stream is always expected, so we
                        // hold even if not yet seen, and let the stream task
                        // forward when it finishes. Fallback: if no stream
                        // appears within 15s, the pending will be reaped and
                        // we forward anyway via a timeout task.
                        if still_streaming || !stream_already_done {
                            log::debug!(
                                "Holding file_complete for {} until data stream finishes",
                                transfer_id
                            );
                            pending_file_completes
                                .lock()
                                .await
                                .insert(transfer_id.clone(), packet_str);
                            // Spawn a timeout that will forward the complete
                            // even if the stream never arrives or stream open failed.
                            // This prevents a held complete from being lost forever.
                            let pending_clone = pending_file_completes.clone();
                            let broker_clone = broker_tx.clone();
                            let tid = transfer_id.clone();
                            tokio::spawn(async move {
                                tokio::time::sleep(Duration::from_secs(15)).await;
                                if let Some(pending) = pending_clone.lock().await.remove(&tid) {
                                    log::warn!(
                                        "Forwarding held file_complete for {} after timeout",
                                        tid
                                    );
                                    let _ = broker_clone.send(AnchorEvent {
                                        target: AnchorTarget::Service("filetransfer".into()),
                                        message: AnchorMessage::Json(pending),
                                    });
                                }
                            });
                        } else {
                            let _ = broker_tx.send(AnchorEvent {
                                target: AnchorTarget::Service("filetransfer".into()),
                                message: AnchorMessage::Json(packet_str),
                            });
                        }
                    }
                } else {
                    log::warn!("Ignoring malformed SDK file completion from {device_id}");
                }
            }
            SessionEvent::StreamOpenRequested {
                request_id,
                quic_stream_id,
                capability_session_id,
                payload_type_url,
            } if screen_sessions.contains(&capability_session_id)
                && payload_type_url == screen::FRAME_TYPE_URL =>
            {
                log::info!(
                    "SDK screen reliable stream open requested by {device_id}: capability_session={capability_session_id} quic_stream={quic_stream_id}"
                );
                let handle = tokio::runtime::Handle::current();
                let stream = session.accept_stream(request_id, quic_stream_id).await?;
                let stream_id = stream.stream_id();
                let (send, mut recv) = stream.into_parts();
                screen_binding.attach_frame_stream(device_id.clone(), stream_id, send, handle);
                // Screen is desktop-to-phone. Drain the reverse half so a
                // peer that sends an accidental payload cannot consume
                // connection flow-control credit.
                tokio::spawn(async move {
                    while let Ok(Some(_)) = recv.read_chunk(16 * 1024, true).await {}
                });
            }
            SessionEvent::StreamOpenRequested {
                request_id,
                quic_stream_id,
                capability_session_id,
                payload_type_url,
                ..
            } if file_sessions.contains(&capability_session_id)
                && payload_type_url == files::CONTENT_STREAM_TYPE_URL =>
            {
                log::info!(
                    "SDK file stream open requested by {device_id}: capability_session={capability_session_id} quic_stream={quic_stream_id}"
                );
                let session = session.clone();
                let expected_id = quic_stream_id;
                let peer = device_id.clone();
                let stream_transfers = stream_transfers.clone();
                let broker_tx = broker_tx.clone();
                let pending_file_completes = pending_file_completes.clone();
                let completed_file_streams = completed_file_streams.clone();
                tokio::spawn(async move {
                    match session.accept_stream(request_id, expected_id).await {
                        Ok(stream) => {
                            let (_send, mut recv) = stream.into_parts();
                            let deadline = tokio::time::Instant::now() + Duration::from_secs(15);
                            let transfer_id = loop {
                                if let Some(id) =
                                    stream_transfers.lock().await.get(&expected_id).cloned()
                                {
                                    break id;
                                }
                                if tokio::time::Instant::now() >= deadline {
                                    log::warn!(
                                        "SDK file stream {expected_id} was never bound to a transfer"
                                    );
                                    return;
                                }
                                tokio::time::sleep(Duration::from_millis(5)).await;
                            };
                            let mut seq = 0u64;
                            loop {
                                match recv.read_chunk(16 * 1024, true).await {
                                    Ok(Some(chunk)) => {
                                        let packet = serde_json::json!({
                                            "plugin_id": "filetransfer",
                                            "type": "file_chunk",
                                            "transfer_id": transfer_id.clone(),
                                            "seq": seq,
                                            "data": base64::engine::general_purpose::STANDARD.encode(chunk.as_ref()),
                                        });
                                        let _ = broker_tx.send(AnchorEvent {
                                            target: AnchorTarget::Service("filetransfer".into()),
                                            message: AnchorMessage::Json(packet.to_string()),
                                        });
                                        seq += 1;
                                    }
                                    Ok(None) => break,
                                    Err(error) => {
                                        log::warn!(
                                            "SDK file stream read failed from {peer}: {error}"
                                        );
                                        return;
                                    }
                                }
                            }
                            stream_transfers.lock().await.remove(&expected_id);
                            log::info!("SDK file stream received from {peer}: {seq} chunks");
                            // If a COMPLETE was held waiting for this stream, forward it now.
                            if let Some(pending) =
                                pending_file_completes.lock().await.remove(&transfer_id)
                            {
                                log::debug!("Forwarding held file_complete for {}", transfer_id);
                                let _ = broker_tx.send(AnchorEvent {
                                    target: AnchorTarget::Service("filetransfer".into()),
                                    message: AnchorMessage::Json(pending),
                                });
                            } else {
                                // No pending COMPLETE yet — mark stream as
                                // completed so a later COMPLETE can be
                                // forwarded immediately without waiting.
                                completed_file_streams
                                    .lock()
                                    .await
                                    .insert(transfer_id.clone(), Instant::now());
                            }
                        }
                        Err(error) => {
                            log::warn!("SDK file stream accept failed from {peer}: {error}")
                        }
                    }
                });
            }
            SessionEvent::StreamOpenRequested {
                request_id,
                quic_stream_id,
                capability_session_id,
                ..
            } => {
                log::warn!(
                    "SDK stream open requested for unbound capability from {device_id}: capability_session={capability_session_id} quic_stream={quic_stream_id}"
                );
                session.send_stream_opened(request_id, quic_stream_id).await?;
            }
            SessionEvent::DatagramFlowOpenRequested {
                request_id,
                capability_session_id,
                flow_id,
                payload_type_url,
            } if screen_sessions.contains(&capability_session_id)
                && payload_type_url == screen::FRAME_TYPE_URL =>
            {
                let flow = session.accept_datagram_flow(request_id, flow_id).await?;
                screen_binding.attach_frame_flow(device_id.clone(), flow);
                log::info!(
                    "SDK screen datagram flow opened by {device_id}: capability_session={capability_session_id} flow={flow_id}"
                );
            }
            SessionEvent::DatagramFlowOpenRequested {
                request_id,
                capability_session_id,
                flow_id,
                payload_type_url,
            } if camera_sessions.contains(&capability_session_id)
                && payload_type_url == camera::FRAME_TYPE_URL =>
            {
                let flow = session.accept_datagram_flow(request_id, flow_id).await?;
                camera_binding.attach_frame_flow(device_id.clone(), capability_session_id, flow);
                log::info!(
                    "SDK camera datagram flow opened by {device_id}: capability_session={capability_session_id} flow={flow_id}"
                );
            }
            SessionEvent::CapabilityRecord { capability_session_id, type_url, payload }
                if type_url == clipboard::PUBLISH_TYPE_URL =>
            {
                log::debug!(
                    "SDK capability record received: session={capability_session_id} type={type_url} bytes={}",
                    payload.len()
                );
                if let Ok(message) = clipboard::decode_publish(&payload)
                    && let Some(content) = message.content
                {
                    let (content_type, content_value) = match content {
                        v1::capabilities::clipboard::clipboard_publish::Content::TextUtf8(text) => {
                            ("text/plain", text)
                        }
                        v1::capabilities::clipboard::clipboard_publish::Content::Png(png) => (
                            "image/png",
                            base64::Engine::encode(&base64::engine::general_purpose::STANDARD, png),
                        ),
                    };
                    let hash = Sha256::digest(content_value.as_bytes())
                        .iter()
                        .map(|byte| format!("{byte:02x}"))
                        .collect::<String>();
                    let packet = serde_json::json!({
                        "plugin_id": "clipboard",
                        "type": "clipboard_content",
                        "content_type": content_type,
                        "content": content_value,
                        "timestamp": now_ms(),
                        "hash": hash,
                    });
                    let _ = broker_tx.send(AnchorEvent {
                        target: AnchorTarget::Service("clipboard".into()),
                        message: AnchorMessage::Json(packet.to_string()),
                    });
                } else {
                    log::warn!("Ignoring malformed SDK clipboard record");
                }
            }
            SessionEvent::CapabilityRecord { capability_session_id, type_url, payload }
                if accepts_device_state(&device_sessions, capability_session_id, &type_url) =>
            {
                log::debug!(
                    "SDK capability record received: session={capability_session_id} type={type_url} bytes={}",
                    payload.len()
                );
                if let Ok(state) = device::decode_state(&payload)
                    && state.battery_percent <= 100
                {
                    registry.set_display_size(
                        &device_id,
                        state.display_width,
                        state.display_height,
                    );
                    let packet = serde_json::json!({
                        "plugin_id": "gui",
                        "type": "battery_update",
                        "device_id": device_id,
                        "level": state.battery_percent,
                        "charging": state.charging,
                        "status_text": if state.charging { "Charging" } else { "Discharging" },
                    });
                    let _ = broker_tx.send(AnchorEvent {
                        target: AnchorTarget::Gui,
                        message: AnchorMessage::Json(packet.to_string()),
                    });
                    log::debug!("SDK device state received from {device_id}");
                } else {
                    log::warn!("Ignoring malformed SDK device state");
                }
            }
            SessionEvent::CapabilityClosed { capability_session_id, .. }
                if device_sessions.remove(&capability_session_id) =>
            {
                device_binding.detach(&device_id);
                log::info!("SDK device capability closed by {device_id}");
            }
            SessionEvent::CapabilityRecord { capability_session_id, type_url, payload }
                if type_url == notifications::POSTED_TYPE_URL =>
            {
                log::debug!(
                    "SDK capability record received: session={capability_session_id} type={type_url} bytes={}",
                    payload.len()
                );
                if let Ok(notification) = notifications::decode_posted(&payload) {
                    let title = notification.title.clone();
                    let packet = serde_json::json!({
                        "plugin_id": "foghorn",
                        "type": "notification",
                        "source": "android",
                        "app_package": notification.application_id,
                        "app_name": notification.application_name,
                        "title": notification.title,
                        "body": notification.body,
                        "timestamp": notification.posted_at_unix_ms / 1000,
                    });
                    let _ = broker_tx.send(AnchorEvent {
                        target: AnchorTarget::Service("foghorn".into()),
                        message: AnchorMessage::Json(packet.to_string()),
                    });
                    log::info!("SDK notification received from {device_id}: {title}");
                } else {
                    log::warn!("Ignoring malformed SDK notification record");
                }
            }
            SessionEvent::CapabilityRecord { capability_session_id, type_url, payload }
                if input_sessions.contains(&capability_session_id)
                    && type_url == input::POINTER_ABSOLUTE_TYPE_URL =>
            {
                if let Ok(message) = input::decode_pointer_absolute(&payload) {
                    let packet = serde_json::json!({"plugin_id":"input", "type":"anchor.input.motion_absolute", "x": message.x as f64 / 65535.0, "y": message.y as f64 / 65535.0, "output_name": message.target_output_name, "time": now_ms()});
                    let _ = broker_tx.send(AnchorEvent {
                        target: AnchorTarget::Service("input".into()),
                        message: AnchorMessage::Json(packet.to_string()),
                    });
                }
            }
            SessionEvent::CapabilityRecord { capability_session_id, type_url, payload }
                if input_sessions.contains(&capability_session_id)
                    && type_url == input::POINTER_RELATIVE_TYPE_URL =>
            {
                if let Ok(message) = input::decode_pointer_relative(&payload) {
                    let packet = serde_json::json!({"plugin_id":"input", "type":"anchor.input.motion", "dx": message.dx_1000ths as f64 / 1000.0, "dy": message.dy_1000ths as f64 / 1000.0, "time": now_ms()});
                    let _ = broker_tx.send(AnchorEvent {
                        target: AnchorTarget::Service("input".into()),
                        message: AnchorMessage::Json(packet.to_string()),
                    });
                }
            }
            SessionEvent::CapabilityRecord { capability_session_id, type_url, payload }
                if input_sessions.contains(&capability_session_id)
                    && type_url == input::KEY_TYPE_URL =>
            {
                if let Ok(message) = input::decode_key(&payload) {
                    let packet = serde_json::json!({"plugin_id":"input", "type":"anchor.input.key_hid", "hid_usage": message.hid_usage, "pressed": message.pressed, "time": now_ms()});
                    let _ = broker_tx.send(AnchorEvent {
                        target: AnchorTarget::Service("input".into()),
                        message: AnchorMessage::Json(packet.to_string()),
                    });
                }
            }
            SessionEvent::CapabilityRecord { capability_session_id, type_url, payload }
                if input_sessions.contains(&capability_session_id)
                    && type_url == input::POINTER_BUTTON_TYPE_URL =>
            {
                if let Ok(message) = input::decode_pointer_button(&payload) {
                    // The wire enum is platform-neutral; translate to Linux
                    // evdev codes only at the Wayland boundary.
                    let button = match message.button {
                        1 => Some(272), // left
                        2 => Some(274), // middle
                        3 => Some(273), // right
                        4 => Some(275), // back
                        5 => Some(276), // forward
                        _ => None,
                    };
                    if let Some(button) = button {
                        if message.pressed {
                            pressed_buttons.insert(button);
                        } else {
                            pressed_buttons.remove(&button);
                        }
                        log::debug!(
                            "SDK input button from {device_id}: button={button} pressed={}",
                            message.pressed
                        );
                        let packet = serde_json::json!({"plugin_id":"input", "type":"anchor.input.button", "button": button, "state": if message.pressed { 1 } else { 0 }, "time": now_ms()});
                        let _ = broker_tx.send(AnchorEvent {
                            target: AnchorTarget::Service("input".into()),
                            message: AnchorMessage::Json(packet.to_string()),
                        });
                    } else {
                        log::warn!(
                            "Ignoring SDK input button with unknown enum value {}",
                            message.button
                        );
                    }
                }
            }
            SessionEvent::CapabilityRecord { capability_session_id, type_url, payload }
                if input_sessions.contains(&capability_session_id)
                    && type_url == input::SCROLL_TYPE_URL =>
            {
                if let Ok(message) = input::decode_scroll(&payload) {
                    let packet = serde_json::json!({"plugin_id":"input", "type":"anchor.input.axis", "axis": 0, "value": message.vertical_120ths, "time": now_ms()});
                    let _ = broker_tx.send(AnchorEvent {
                        target: AnchorTarget::Service("input".into()),
                        message: AnchorMessage::Json(packet.to_string()),
                    });
                }
            }
            SessionEvent::CapabilityRecord { capability_session_id, type_url, payload }
                if input_sessions.contains(&capability_session_id)
                    && type_url == input::TEXT_TYPE_URL =>
            {
                if let Ok(message) = input::decode_text(&payload) {
                    let packet = serde_json::json!({"plugin_id":"input", "type":"anchor.input.text", "text": message.text_utf8, "time": now_ms()});
                    let _ = broker_tx.send(AnchorEvent {
                        target: AnchorTarget::Service("input".into()),
                        message: AnchorMessage::Json(packet.to_string()),
                    });
                }
            }
            SessionEvent::CapabilityRecord { capability_session_id: _, type_url, payload }
                if type_url == media::STATE_TYPE_URL =>
            {
                if let Ok(state) = media::decode_state(&payload) {
                    let art_url = if state.artwork_jpeg.len() <= media::MAX_ARTWORK_JPEG_BYTES
                        && !state.artwork_jpeg.is_empty()
                    {
                        format!(
                            "data:image/jpeg;base64,{}",
                            base64::engine::general_purpose::STANDARD.encode(state.artwork_jpeg),
                        )
                    } else {
                        String::new()
                    };
                    let packet = serde_json::json!({
                        "plugin_id": "media", "type": "media_state", "source": "android",
                        "state": if state.playing { "playing" } else { "paused" },
                        "title": state.title, "artist": state.artist, "album": state.album,
                        "position_ms": state.position_ms, "duration_ms": state.duration_ms,
                        "session_id": "sdk", "app": "android",
                        "art_url": art_url,
                        "can_play": true, "can_pause": true, "can_next": true, "can_prev": true, "can_seek": true,
                    });
                    let _ = broker_tx.send(AnchorEvent {
                        target: AnchorTarget::Service("media".into()),
                        message: AnchorMessage::Json(packet.to_string()),
                    });
                    log::info!("SDK media state received from {device_id}: {}", state.title);
                }
            }
            SessionEvent::CapabilityRecord { capability_session_id: _, type_url, payload }
                if type_url == media::COMMAND_TYPE_URL =>
            {
                if let Ok(command) = media::decode_command(&payload) {
                    let command_name = match command.kind {
                        1 => "play",
                        2 => "pause",
                        3 => "next",
                        4 => "previous",
                        5 => "seek",
                        _ => "",
                    };
                    if !command_name.is_empty() {
                        let packet = serde_json::json!({"plugin_id":"media", "type":"media_command", "command":command_name, "position_ms":command.position_ms});
                        let _ = broker_tx.send(AnchorEvent {
                            target: AnchorTarget::Service("media".into()),
                            message: AnchorMessage::Json(packet.to_string()),
                        });
                    }
                }
            }
            SessionEvent::CapabilityRecord { capability_session_id: _, type_url, payload }
                if type_url == screen::START_TYPE_URL =>
            {
                if let Ok(start) = screen::decode_start(&payload) {
                    let packet = serde_json::json!({
                        "plugin_id": "wayland", "command": "start",
                        "max_fps": start.max_fps, "bitrate_kbps": start.target_bitrate_kbps,
                        "output_id": start.output_id,
                    });
                    let _ = broker_tx.send(AnchorEvent {
                        target: AnchorTarget::Wayland,
                        message: AnchorMessage::Json(packet.to_string()),
                    });
                    log::info!("SDK screen start received from {device_id}");
                }
            }
            SessionEvent::CapabilityRecord { capability_session_id: _, type_url, payload }
                if type_url == screen::STOP_TYPE_URL =>
            {
                if screen::decode_stop(&payload).is_ok() {
                    let packet = serde_json::json!({"plugin_id":"wayland", "command":"stop"});
                    let _ = broker_tx.send(AnchorEvent {
                        target: AnchorTarget::Wayland,
                        message: AnchorMessage::Json(packet.to_string()),
                    });
                    log::info!("SDK screen stop received from {device_id}");
                }
            }
            SessionEvent::CapabilityRecord { capability_session_id: _, type_url, payload }
                if type_url == screen::SELECT_OUTPUT_TYPE_URL =>
            {
                if let Ok(select) = screen::decode_select_output(&payload) {
                    let index = select.output_id.parse::<u32>().unwrap_or(0);
                    let packet = serde_json::json!({"plugin_id":"wayland", "command":"select_output", "index":index});
                    let _ = broker_tx.send(AnchorEvent {
                        target: AnchorTarget::Wayland,
                        message: AnchorMessage::Json(packet.to_string()),
                    });
                }
            }
            SessionEvent::CapabilityRecord { capability_session_id: _, type_url, payload }
                if type_url == screen::REQUEST_KEYFRAME_TYPE_URL =>
            {
                if screen::decode_request_keyframe(&payload).is_ok() {
                    let packet =
                        serde_json::json!({"plugin_id":"wayland", "command":"request_keyframe"});
                    let _ = broker_tx.send(AnchorEvent {
                        target: AnchorTarget::Wayland,
                        message: AnchorMessage::Json(packet.to_string()),
                    });
                }
            }
            SessionEvent::CapabilityRecord { capability_session_id: _, type_url, payload }
                if type_url == camera::STATUS_TYPE_URL =>
            {
                if let Ok(status) = camera::decode_status(&payload) {
                    let packet = serde_json::json!({
                        "plugin_id": "cameraplugin", "type": "camera_stream_info",
                        "width": status.width, "height": status.height,
                        "fps": status.fps, "bitrate_kbps": status.bitrate_kbps,
                    });
                    let _ = broker_tx.send(AnchorEvent {
                        target: AnchorTarget::Service("cameraplugin".into()),
                        message: AnchorMessage::Json(packet.to_string()),
                    });
                    log::info!(
                        "SDK camera status received from {device_id}: {}x{}",
                        status.width,
                        status.height
                    );
                }
            }
            SessionEvent::CapabilityRecord { capability_session_id: _, type_url, payload }
                if type_url == sms::CONVERSATION_SNAPSHOT_TYPE_URL =>
            {
                if let Ok(snapshot) = sms::decode_conversation_snapshot(payload.as_slice()) {
                    forward_typed_sms_messages(snapshot.messages, &broker_tx);
                    log::info!("SDK SMS snapshot received from {device_id}");
                } else {
                    log::warn!("Ignoring malformed SDK SMS snapshot from {device_id}");
                }
            }
            SessionEvent::CapabilityRecord { capability_session_id: _, type_url, payload }
                if type_url == sms::CONVERSATION_LIST_TYPE_URL =>
            {
                if let Ok(list) = sms::decode_conversation_list(payload.as_slice()) {
                    forward_typed_sms_messages(list.latest_messages, &broker_tx);
                    log::info!("SDK SMS conversation list received from {device_id}");
                } else {
                    log::warn!("Ignoring malformed SDK SMS conversation list from {device_id}");
                }
            }
            SessionEvent::CapabilityRecord { capability_session_id: _, type_url, payload }
                if type_url == sms::MESSAGE_TYPE_URL =>
            {
                if let Ok(message) = sms::decode_message(&payload) {
                    let packet = serde_json::json!({
                        "plugin_id": "smsplugin", "type": "anchor.sms.messages", "messages": [{
                            "id": message.message_id, "thread_id": message.conversation_id,
                            "address": message.address, "body": message.body,
                            "timestamp": message.timestamp_unix_ms, "outgoing": message.outgoing,
                        }]
                    });
                    let _ = broker_tx.send(AnchorEvent {
                        target: AnchorTarget::Service("smsplugin".into()),
                        message: AnchorMessage::Json(packet.to_string()),
                    });
                    log::info!("SDK SMS message received from {device_id}");
                }
            }
            SessionEvent::CapabilityRecord { capability_session_id: _, type_url, payload }
                if type_url == commands::RUN_TYPE_URL =>
            {
                if let Ok(run) = commands::decode_run(&payload) {
                    let packet = serde_json::json!({ "plugin_id": "commands", "type": "run_command", "id": run.command_id, "args": run.arguments });
                    let _ = broker_tx.send(AnchorEvent {
                        target: AnchorTarget::Service("commands".into()),
                        message: AnchorMessage::Json(packet.to_string()),
                    });
                    log::info!("SDK command request received from {device_id}: {}", run.command_id);
                }
            }
            SessionEvent::CapabilityRecord { capability_session_id, type_url, payload }
                if command_sessions.contains(&capability_session_id)
                    && type_url == commands::KILL_TYPE_URL =>
            {
                if let Ok(kill) = commands::decode_kill(&payload) {
                    let packet = serde_json::json!({
                        "plugin_id": "commands",
                        "type": "kill_command",
                        "exec_id": kill.execution_id,
                    });
                    let _ = broker_tx.send(AnchorEvent {
                        target: AnchorTarget::Service("commands".into()),
                        message: AnchorMessage::Json(packet.to_string()),
                    });
                    log::info!("SDK command kill request received from {device_id}");
                }
            }
            // Android uses this round trip for the latency metric shown in its
            // stream UI. Ping/Pong is session-level (not a capability record),
            // so it must be handled here alongside the capability event loop.
            SessionEvent::Ping { nonce } => {
                session.send_pong(nonce).await?;
            }
            SessionEvent::Pong { .. } => {}
            SessionEvent::SessionClosed { .. } => {
                // Best-effort release before dropping this session.  This is
                // deliberately emitted through the normal input broker so it
                // uses the same Wayland backend and event ordering as a real
                // release record.
                for button in pressed_buttons.drain() {
                    let _ = broker_tx.send(AnchorEvent {
                        target: AnchorTarget::Service("input".into()),
                        message: AnchorMessage::Json(
                            serde_json::json!({
                                "plugin_id": "input",
                                "type": "anchor.input.button",
                                "button": button,
                                "state": 0,
                                "time": now_ms(),
                            })
                            .to_string(),
                        ),
                    });
                    log::warn!("Released SDK input button {button} after session close");
                }
                files_binding.detach(&device_id);
                clipboard_binding.detach(&device_id);
                media_binding.detach(&device_id);
                notifications_binding.detach(&device_id);
                sms_binding.detach(&device_id);
                commands_binding.detach(&device_id);
                camera_binding.detach(&device_id);
                device_binding.detach(&device_id);
                screen_binding.detach_device(&device_id);
                return Ok(());
            }
            _ => {}
        }
    }
}

/// Pairing is a separate control phase: it never reaches a capability binding.
/// The desktop's existing UI remains the authority that grants or rejects the
/// certificate pin; this task only bridges that answer back over the same QUIC
/// connection that carried the requester identity.
async fn handle_pairing(
    pairing: PairingSession,
    registry: DeviceRegistry,
    broker_tx: std::sync::mpsc::Sender<AnchorEvent>,
    pairing_request: SharedPairingRequest,
    desktop_safety_number: String,
    desktop_certificate_der: Vec<u8>,
    desktop_device_id: String,
) -> Result<(), anchor_sdk::SessionError> {
    let hello = pairing.hello().clone();
    if hello.display_name.trim().is_empty() {
        pairing.reject().await?;
        return Ok(());
    }
    let certificate = pairing.peer_certificate_der().ok_or(
        anchor_sdk::SessionError::UnexpectedRecord("pairing peer did not present a certificate"),
    )?;
    let device_id = hex_bytes(&hello.node_id);
    let certificate_pem = format!(
        "-----BEGIN CERTIFICATE-----\n{}\n-----END CERTIFICATE-----\n",
        base64::engine::general_purpose::STANDARD.encode(&certificate),
    );
    let fingerprint = crate::anchorapp::tls::TrustedStore::fingerprint(&certificate);
    let display_name = hello.display_name.clone();
    let request_device_id = device_id.clone();
    let request_certificate_pem = certificate_pem.clone();
    let pairing_event_tx = broker_tx.clone();
    let accepted = tokio::task::spawn_blocking(move || {
        let (response_tx, response_rx) = std::sync::mpsc::sync_channel(1);
        let mut pending = pairing_request.lock().unwrap();
        if pending.is_some() {
            return false;
        }
        *pending = Some(PairingRequest {
            device_id: request_device_id,
            device_name: display_name,
            device_type: "mobile".into(),
            fingerprint,
            safety_number: desktop_safety_number,
            certificate_pem: request_certificate_pem,
            response_tx,
        });
        drop(pending);
        // Pairing requests are written directly into shared state rather than
        // arriving through the regular GUI event stream. Notify the frontend
        // explicitly so its approval dialog is refreshed immediately.
        let _ = pairing_event_tx.send(AnchorEvent {
            target: AnchorTarget::Gui,
            message: AnchorMessage::Json(r#"{"type":"pairing_request"}"#.into()),
        });
        response_rx.recv_timeout(Duration::from_secs(60)).unwrap_or(false)
    })
    .await
    .map_err(|_| anchor_sdk::SessionError::UnexpectedRecord("pairing approval task failed"))?;
    if accepted {
        registry.add_paired_device(&device_id, &hello.display_name, &certificate_pem);
        pairing
            .approve_with_identity(
                hello.transcript_hash.to_vec(),
                desktop_certificate_der,
                desktop_device_id,
            )
            .await?;
        log::info!("SDK pairing approved for {} ({device_id})", hello.display_name);
    } else {
        pairing.reject().await?;
        log::info!("SDK pairing rejected or expired for {}", hello.display_name);
    }
    // The initiator reads the resolution before it closes this short-lived
    // connection. Avoid dropping the stream immediately after its write.
    let _ = tokio::time::timeout(Duration::from_secs(5), pairing.closed()).await;
    Ok(())
}

/// Adapt the typed SMS data model at the desktop UI boundary.  This is
/// deliberately local-only: QUIC carries protobuf records end-to-end; the
/// existing desktop SMS store happens to consume its historical JSON shape.
fn forward_typed_sms_messages(
    messages: Vec<sms::ConversationMessage>,
    broker_tx: &std::sync::mpsc::Sender<AnchorEvent>,
) {
    let messages = messages
        .into_iter()
        .map(|message| {
            serde_json::json!({
                "_id": message.message_id,
                "thread_id": message.conversation_id,
                "event": message.event,
                "body": message.body,
                "date": message.timestamp_unix_ms,
                "type": message.message_type,
                "read": message.read,
                "sub_id": message.subscription_id,
                "addresses": message.addresses.into_iter().map(|address| serde_json::json!({
                    "address": address.address,
                    "name": address.display_name,
                })).collect::<Vec<_>>(),
                "attachments": message.attachments.into_iter().map(|attachment| serde_json::json!({
                    "part_id": attachment.part_id,
                    "mime_type": attachment.mime_type,
                    "unique_identifier": attachment.transfer_id,
                    "encoded_thumbnail": base64::engine::general_purpose::STANDARD.encode(attachment.thumbnail_jpeg),
                })).collect::<Vec<_>>(),
            })
        })
        .collect::<Vec<_>>();
    let packet = serde_json::json!({
        "plugin_id": "smsplugin",
        "type": "anchor.sms.messages",
        "messages": messages,
    });
    let _ = broker_tx.send(AnchorEvent {
        target: AnchorTarget::Service("smsplugin".into()),
        message: AnchorMessage::Json(packet.to_string()),
    });
}

fn session_identity(identity: &AnchorIdentity) -> SessionIdentity {
    let mut endpoint = clipboard::endpoint_advertisement();
    endpoint.capabilities.push(device::advertisement());
    endpoint.capabilities.push(notifications::advertisement());
    endpoint.capabilities.push(input::advertisement());
    endpoint.capabilities.push(media::advertisement());
    endpoint.capabilities.push(screen::advertisement());
    endpoint.capabilities.push(camera::advertisement());
    endpoint.capabilities.push(files::advertisement());
    endpoint.capabilities.push(sms::advertisement());
    endpoint.capabilities.push(commands::advertisement());
    SessionIdentity {
        node_id: node_id(&identity.device_id),
        display_name: super::mdns::desktop_name(),
        device_kind: 1,
        endpoints: vec![endpoint],
    }
}

fn node_id(device_id: &str) -> [u8; 32] {
    Sha256::digest(device_id.as_bytes()).into()
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |duration| duration.as_millis() as u64)
}

fn hex_bytes(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn pairing_safety_number(certificate_der: &[u8]) -> String {
    let digest = Sha256::digest(certificate_der);
    let number = (u32::from(digest[0]) << 16) | (u32::from(digest[1]) << 8) | u32::from(digest[2]);
    format!("{:06}", number % 1_000_000)
}

fn valid_file_offer(offer: &files::FileOffer) -> bool {
    const MAX_FILE_BYTES: u64 = 256 * 1024 * 1024;
    offer.transfer_id.len() == 16
        && offer.sha256.len() == 32
        && offer.byte_length <= MAX_FILE_BYTES
        && !offer.filename.is_empty()
}

fn accepts_device_state(
    device_sessions: &HashSet<u64>,
    capability_session_id: u64,
    type_url: &str,
) -> bool {
    device_sessions.contains(&capability_session_id) && type_url == device::STATE_TYPE_URL
}

#[cfg(test)]
mod tests {
    use super::{
        accepts_device_state, device, forward_typed_sms_messages, handle_pairing,
        pairing_safety_number, screen, session_identity, v1, valid_file_offer,
    };
    use crate::anchorapp::event::{AnchorMessage, AnchorTarget};
    use anchor_sdk::{
        AcceptedSession, AnchorListener, ListenerConfig, ListenerIdentity, PairingSession,
        files::FileOffer, pairing, quinn_transport,
    };
    use quinn::Endpoint;
    use rustls::{RootCertStore, pki_types::CertificateDer};
    use std::{
        collections::HashSet,
        sync::{Arc, Mutex, mpsc},
        time::Duration,
    };

    #[test]
    fn device_state_requires_its_open_capability_session_and_type() {
        let sessions = HashSet::from([7_u64]);

        assert!(accepts_device_state(&sessions, 7, device::STATE_TYPE_URL));
        assert!(!accepts_device_state(&sessions, 8, device::STATE_TYPE_URL));
        assert!(!accepts_device_state(&sessions, 7, screen::STATUS_TYPE_URL));
    }

    fn client_config_for(
        server: &crate::anchorapp::tls::AnchorIdentity,
        client: &crate::anchorapp::tls::AnchorIdentity,
    ) -> quinn::ClientConfig {
        let server_certificate =
            rustls_pemfile::certs(&mut server.certificate_pem.as_bytes()).next().unwrap().unwrap();
        let client_certificate = rustls_pemfile::certs(&mut client.certificate_pem.as_bytes())
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        let client_key =
            rustls_pemfile::private_key(&mut client.private_key_pem.as_bytes()).unwrap().unwrap();
        let mut roots = RootCertStore::empty();
        roots.add(CertificateDer::from(server_certificate.as_ref().to_vec())).unwrap();
        let tls = rustls::ClientConfig::builder()
            .with_root_certificates(roots)
            .with_client_auth_cert(client_certificate, client_key)
            .unwrap();
        quinn_transport::client_config(tls).unwrap()
    }

    #[test]
    fn file_offer_policy_rejects_bad_identity_and_oversized_payloads() {
        let mut offer = FileOffer {
            transfer_id: vec![1; 16],
            filename: "photo.jpg".into(),
            mime_type: "image/jpeg".into(),
            byte_length: 10,
            sha256: vec![2; 32],
        };
        assert!(valid_file_offer(&offer));
        offer.transfer_id.pop();
        assert!(!valid_file_offer(&offer));
        offer.transfer_id = vec![1; 16];
        offer.byte_length = 256 * 1024 * 1024 + 1;
        assert!(!valid_file_offer(&offer));
    }

    #[test]
    fn typed_sms_list_is_adapted_only_at_the_desktop_service_boundary() {
        let (tx, rx) = mpsc::channel();
        forward_typed_sms_messages(
            vec![v1::capabilities::sms::ConversationMessage {
                message_id: 7,
                conversation_id: 11,
                body: "hello".into(),
                ..Default::default()
            }],
            &tx,
        );
        let event = rx.recv().unwrap();
        assert_eq!(event.target, AnchorTarget::Service("smsplugin".into()));
        let AnchorMessage::Json(packet) = event.message else {
            panic!("expected JSON adapter event")
        };
        let value: serde_json::Value = serde_json::from_str(&packet).unwrap();
        assert_eq!(value["type"], "anchor.sms.messages");
        assert_eq!(value["messages"][0]["_id"], 7);
        assert_eq!(value["messages"][0]["thread_id"], 11);
    }

    #[test]
    fn session_identity_advertises_device_and_clipboard() {
        let identity = crate::anchorapp::tls::generate_test_identity();
        let session_identity = super::session_identity(&identity);
        assert_eq!(session_identity.endpoints.len(), 1);
        let names = session_identity
            .endpoints
            .iter()
            .flat_map(|endpoint| endpoint.capabilities.iter())
            .map(|capability| capability.name.as_str())
            .collect::<Vec<_>>();
        assert!(names.contains(&anchor_sdk::clipboard::CAPABILITY_NAME));
        assert!(names.contains(&anchor_sdk::device::CAPABILITY_NAME));
    }

    #[test]
    fn sdk_listener_binds_wildcard() {
        let identity = crate::anchorapp::tls::generate_test_identity();
        let session_identity = session_identity(&identity);
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .enable_all()
            .build()
            .unwrap();
        runtime.block_on(async {
            let listener = AnchorListener::bind(ListenerConfig {
                bind_address: "0.0.0.0:0".parse().unwrap(),
                identity: ListenerIdentity::from_pem(
                    identity.certificate_pem,
                    identity.private_key_pem,
                ),
                session_identity,
            });
            assert!(listener.is_ok(), "SDK listener failed to bind");
        });
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn nearby_pairing_requires_desktop_approval_then_persists_the_peer_pin() {
        let desktop = crate::anchorapp::tls::generate_test_identity();
        let phone = crate::anchorapp::tls::generate_test_identity();
        let directory =
            std::env::temp_dir().join(format!("anchor-nearby-pairing-{}", uuid::Uuid::new_v4()));
        let registry =
            super::DeviceRegistry::new(crate::anchorapp::tls::TrustedStore::load(&directory));
        let pairing_request = Arc::new(Mutex::new(None));
        let (broker_tx, broker_rx) = std::sync::mpsc::channel();
        let listener = AnchorListener::bind(ListenerConfig {
            bind_address: "127.0.0.1:0".parse().unwrap(),
            identity: ListenerIdentity::from_pem(
                desktop.certificate_pem.clone(),
                desktop.private_key_pem.clone(),
            ),
            session_identity: session_identity(&desktop),
        })
        .unwrap();
        let address = listener.local_addr().unwrap();
        let server_registry = registry.clone();
        let server_request = pairing_request.clone();
        let listener_identity = ListenerIdentity::from_pem(
            desktop.certificate_pem.clone(),
            desktop.private_key_pem.clone(),
        );
        let desktop_certificate_der = listener_identity.certificate_der().unwrap();
        let desktop_safety_number = pairing_safety_number(&desktop_certificate_der);
        let desktop_device_id = desktop.device_id.clone();
        let expected_certificate_der = desktop_certificate_der.clone();
        let expected_device_id = desktop_device_id.clone();
        let server_task = tokio::spawn(async move {
            let incoming = listener.accept().await.unwrap().unwrap();
            let AcceptedSession::Pairing(pairing) = incoming.accept().await.unwrap() else {
                panic!("a PairingHello must not be accepted as a normal SDK session");
            };
            handle_pairing(
                *pairing,
                server_registry,
                broker_tx,
                server_request,
                desktop_safety_number,
                desktop_certificate_der,
                desktop_device_id,
            )
            .await
            .unwrap();
        });

        let mut client_endpoint = Endpoint::client("0.0.0.0:0".parse().unwrap()).unwrap();
        client_endpoint.set_default_client_config(client_config_for(&desktop, &phone));
        let pairing = PairingSession::connect(
            &client_endpoint,
            address,
            "anchor.test",
            pairing::Hello {
                invitation_id: vec![7; 16],
                node_id: [4; 32],
                display_name: "Test Phone".into(),
                device_kind: 2,
                transcript_hash: [9; 32],
            },
        )
        .await
        .unwrap();

        let pairing_event = broker_rx
            .recv_timeout(Duration::from_secs(2))
            .expect("desktop UI was not notified of the pairing request");
        assert_eq!(pairing_event.target, AnchorTarget::Gui);
        let AnchorMessage::Json(packet) = pairing_event.message else {
            panic!("pairing notification must be a GUI JSON event")
        };
        assert_eq!(packet, r#"{"type":"pairing_request"}"#);

        let request = tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                if let Some(request) = pairing_request.lock().unwrap().take() {
                    break request;
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .expect("desktop pairing prompt was not created");
        assert_eq!(request.device_name, "Test Phone");
        assert_eq!(request.safety_number.len(), 6);
        request.response_tx.send(true).unwrap();

        let v1::control_envelope::Body::PairingApprove(approval) =
            pairing.next_resolution().await.unwrap()
        else {
            panic!("desktop approval must be sent over the pairing-only stream");
        };
        assert_eq!(approval.transcript_hash, vec![9; 32]);
        assert_eq!(approval.approver_certificate_der, expected_certificate_der);
        assert_eq!(approval.approver_device_id, expected_device_id);
        pairing.close(0, b"test finished");
        server_task.await.unwrap();
        assert_eq!(registry.paired_devices().len(), 1);
        assert_eq!(registry.paired_devices()[0].device_name, "Test Phone");
        let _ = std::fs::remove_dir_all(directory);
    }
}
