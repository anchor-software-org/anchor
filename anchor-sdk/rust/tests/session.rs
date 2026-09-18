use anchor_sdk::{
    AcceptedSession, PairingSession, SessionEvent, SessionIdentity, quinn_transport,
    session::Session,
};
use quinn::Endpoint;
use rcgen::generate_simple_self_signed;
use rustls::{
    RootCertStore,
    pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer},
};

fn configs() -> (quinn::ServerConfig, quinn::ClientConfig) {
    let certificate = generate_simple_self_signed(vec!["anchor.test".into()]).unwrap();
    let certificate_der = CertificateDer::from(certificate.cert.der().to_vec());
    let private_key = PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(
        certificate.signing_key.serialize_der(),
    ));
    let server_tls = rustls::ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(vec![certificate_der.clone()], private_key)
        .unwrap();
    let mut roots = RootCertStore::empty();
    roots.add(certificate_der).unwrap();
    let client_tls = rustls::ClientConfig::builder()
        .with_root_certificates(roots)
        .with_no_client_auth();
    (
        quinn_transport::server_config(server_tls).unwrap(),
        quinn_transport::client_config(client_tls).unwrap(),
    )
}

fn identity(endpoints: Vec<anchor_sdk::v1::EndpointAdvertisement>) -> SessionIdentity {
    SessionIdentity {
        node_id: [7; 32],
        display_name: "test-peer".into(),
        device_kind: 1,
        endpoints,
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn session_handshake_and_capability_record_round_trip() {
    let (server_config, client_config) = configs();
    let server = Endpoint::server(server_config, "127.0.0.1:0".parse().unwrap()).unwrap();
    let address = server.local_addr().unwrap();
    let advertisement = anchor_sdk::v1::EndpointAdvertisement {
        endpoint_id: "io.anchor.desktop".into(),
        capabilities: vec![anchor_sdk::v1::CapabilityAdvertisement {
            name: "org.anchor.clipboard".into(),
            major: 1,
            record_type_urls: vec![anchor_sdk::clipboard::PUBLISH_TYPE_URL.into()],
            supports_datagrams: false,
        }],
    };
    let server_task = tokio::spawn(async move {
        let connecting = server.accept().await.unwrap();
        let session = Session::accept(connecting, identity(vec![advertisement.clone()]))
            .await
            .unwrap();
        let SessionEvent::CapabilityOpenRequested {
            request_id,
            capability_session_id,
            ..
        } = session.next_event().await.unwrap()
        else {
            panic!("expected capability open request");
        };
        session.send_ping(42).await.unwrap();
        session
            .send_capability_opened(request_id, capability_session_id)
            .await
            .unwrap();
        let SessionEvent::CapabilityRecord {
            capability_session_id: received_id,
            type_url,
            payload,
        } = session.next_event().await.unwrap()
        else {
            panic!("expected capability record");
        };
        assert_eq!(received_id, capability_session_id);
        assert_eq!(type_url, anchor_sdk::clipboard::PUBLISH_TYPE_URL);
        assert_eq!(payload, b"clipboard".to_vec());
        let SessionEvent::StreamOpenRequested {
            request_id,
            quic_stream_id,
            capability_session_id: stream_capability,
            payload_type_url,
        } = session.next_event().await.unwrap()
        else {
            panic!("expected stream open request");
        };
        assert_eq!(stream_capability, capability_session_id);
        assert_eq!(payload_type_url, anchor_sdk::clipboard::PUBLISH_TYPE_URL);
        let stream = session
            .accept_stream(request_id, quic_stream_id)
            .await
            .unwrap();
        let (_send, mut recv) = stream.into_parts();
        assert_eq!(
            recv.read_chunk(1024, true).await.unwrap().unwrap().as_ref(),
            b"stream-data"
        );
    });

    let mut client_endpoint = Endpoint::client("0.0.0.0:0".parse().unwrap()).unwrap();
    client_endpoint.set_default_client_config(client_config);
    let client = Session::connect(&client_endpoint, address, "anchor.test", identity(vec![]))
        .await
        .unwrap();
    let capability = client
        .open_capability("io.anchor.desktop", "org.anchor.clipboard", 1)
        .await
        .unwrap();
    assert_eq!(
        client.next_event().await.unwrap(),
        SessionEvent::Ping { nonce: 42 }
    );
    capability
        .send_record(
            anchor_sdk::clipboard::PUBLISH_TYPE_URL,
            b"clipboard".to_vec(),
        )
        .await
        .unwrap();
    let stream = capability
        .open_stream(anchor_sdk::clipboard::PUBLISH_TYPE_URL)
        .await
        .unwrap();
    let (mut stream_send, _stream_recv) = stream.into_parts();
    stream_send.write_all(b"stream-data").await.unwrap();
    stream_send.finish().unwrap();
    server_task.await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn pairing_first_record_is_classified_and_cannot_open_capabilities() {
    let (server_config, client_config) = configs();
    let server = Endpoint::server(server_config, "127.0.0.1:0".parse().unwrap()).unwrap();
    let address = server.local_addr().unwrap();
    let server_task = tokio::spawn(async move {
        let incoming = server.accept().await.unwrap();
        let AcceptedSession::Pairing(pairing) =
            Session::accept_classified(incoming, identity(vec![]))
                .await
                .unwrap()
        else {
            panic!("PairingHello must not become a normal capability session");
        };
        assert_eq!(pairing.hello().display_name, "Phone");
        pairing.approve(vec![9; 32]).await.unwrap();
        // A pairing decision is a reliable control record. Keep the server
        // connection alive until the initiator observes it instead of dropping
        // the SendStream immediately after flush.
        let _ = pairing.closed().await;
    });

    let mut client_endpoint = Endpoint::client("0.0.0.0:0".parse().unwrap()).unwrap();
    client_endpoint.set_default_client_config(client_config);
    let pairing = PairingSession::connect(
        &client_endpoint,
        address,
        "anchor.test",
        anchor_sdk::pairing::Hello {
            invitation_id: vec![1; 16],
            node_id: [2; 32],
            display_name: "Phone".into(),
            device_kind: 2,
            transcript_hash: [3; 32],
        },
    )
    .await
    .unwrap();
    let anchor_sdk::v1::control_envelope::Body::PairingApprove(approve) =
        pairing.next_resolution().await.unwrap()
    else {
        panic!("expected explicit pairing approval");
    };
    assert_eq!(approve.transcript_hash, vec![9; 32]);
    pairing.close(0, b"pairing complete");
    server_task.await.unwrap();
}
