//! Real WebSocket framing over an in-memory duplex transport; no accounts or external network.
use bridge_core::{
    delivery::{Delivery, Limits},
    presence::Presence,
    routing::{Router, StreamKey},
    storage::SqliteStore,
    transport::{self, TransportError},
    wire::{Envelope, Side},
};
use futures_util::{SinkExt, StreamExt};
use sqlx::sqlite::SqliteConnectOptions;
use std::sync::Arc;
use tokio::{
    io::DuplexStream,
    sync::{Mutex, watch},
    time::{Duration, Instant, timeout},
};
use tokio_tungstenite::{
    WebSocketStream,
    tungstenite::{Message, protocol::Role},
};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;
type Socket = WebSocketStream<DuplexStream>;

async fn sockets() -> (Socket, Socket) {
    let (a, b) = tokio::io::duplex(1024 * 1024);
    tokio::join!(
        WebSocketStream::from_raw_socket(a, Role::Client, Some(transport::socket_config())),
        WebSocketStream::from_raw_socket(b, Role::Server, Some(transport::socket_config()))
    )
}
async fn delivery() -> Arc<Mutex<Delivery>> {
    let store = Arc::new(
        SqliteStore::open(SqliteConnectOptions::new().in_memory(true))
            .await
            .unwrap(),
    );
    let tenant = Uuid::from_u128(1);
    store.create_tenant(tenant).await.unwrap();
    Arc::new(Mutex::new(Delivery::new(
        Router::new(
            store,
            tenant,
            Uuid::from_u128(2),
            "controller".into(),
            "target".into(),
        ),
        Limits::default(),
    )))
}
fn presence() -> watch::Sender<Presence> {
    let mut p = Presence::default();
    p.report(true, Instant::now());
    watch::channel(p).0
}
async fn text(socket: &mut Socket) -> String {
    timeout(Duration::from_secs(3), async {
        loop {
            match socket.next().await.expect("socket ended").unwrap() {
                Message::Text(raw) => return raw.to_string(),
                Message::Ping(data) => socket.send(Message::Pong(data)).await.unwrap(),
                Message::Pong(_) => {}
                _ => panic!("unexpected non-text frame"),
            }
        }
    })
    .await
    .expect("relay did not produce a frame")
}
fn input() -> String {
    r#"{"type":"client_message","client_id":"phone","stream_id":"source","seq_id":1,"cursor":"cursor-a","message":{"id":9007199254740993,"n":1e-09}}"#.into()
}
fn key() -> StreamKey {
    StreamKey {
        client: "phone".into(),
        stream: "source".into(),
    }
}

#[tokio::test]
async fn websocket_roundtrip_preserves_payload_consumes_ack_and_stops_cleanly() {
    let (input_socket, mut a) = sockets().await;
    let (output_socket, mut b) = sockets().await;
    let delivery = delivery().await;
    let stop = CancellationToken::new();
    let task = tokio::spawn(transport::relay(
        input_socket,
        output_socket,
        delivery.clone(),
        presence(),
        stop.clone(),
    ));
    a.send(Message::Text(input().into())).await.unwrap();
    let request = text(&mut b).await;
    let request = Envelope::publication(Side::Out, &request).unwrap();
    assert_eq!(
        request.raw("message"),
        Some(r#"{"id":9007199254740993,"n":1e-09}"#)
    );
    assert_eq!(request.client(), "controller");
    assert_ne!(request.stream(), "source");
    assert!(request.raw("cursor").is_none());
    b.send(Message::Text(serde_json::json!({"type":"ack","client_id":"controller","env_id":"target","stream_id":request.stream(),"seq_id":request.seq()}).to_string().into())).await.unwrap();
    let response = format!(
        r#"{{"type":"server_message","client_id":"controller","env_id":"target","stream_id":"{}","seq_id":1,"cursor":"cursor-b","message":{{ "result":1e400, "dup":1,"dup":2 }}}}"#,
        request.stream()
    );
    b.send(Message::Text(response.into())).await.unwrap();
    let received = text(&mut a).await;
    let received = Envelope::publication(Side::In, &received).unwrap();
    assert_eq!(received.client(), "phone");
    assert_eq!(received.stream(), "source");
    assert_eq!(
        received.raw("message"),
        Some(r#"{ "result":1e400, "dup":1,"dup":2 }"#)
    );
    assert!(delivery.lock().await.outboxes[1].is_empty());
    assert_eq!(
        delivery.lock().await.subscribe_cursor(Side::Out),
        Some("cursor-b")
    );
    stop.cancel();
    let close = text(&mut b).await;
    assert!(close.contains("client_closed"));
    timeout(Duration::from_secs(3), task)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert!(delivery.lock().await.router.keys().is_empty());
}

#[tokio::test]
async fn explicit_offline_closes_in_and_reconnect_replays_the_same_unacked_frame() {
    let delivery = delivery().await;
    let presence = presence();
    let stop = CancellationToken::new();
    let (input_socket, mut a) = sockets().await;
    let (output_socket, mut b) = sockets().await;
    let task = tokio::spawn(transport::relay(
        input_socket,
        output_socket,
        delivery.clone(),
        presence.clone(),
        stop.clone(),
    ));
    a.send(Message::Text(input().into())).await.unwrap();
    let original = text(&mut b).await;
    let mapped = delivery
        .lock()
        .await
        .router
        .mapped(&key())
        .unwrap()
        .to_owned();
    presence.send_modify(|p| p.report(false, Instant::now()));
    assert!(matches!(
        timeout(Duration::from_secs(3), task)
            .await
            .unwrap()
            .unwrap(),
        Err(TransportError::Offline)
    ));
    assert_eq!(
        delivery.lock().await.router.mapped(&key()),
        Some(mapped.as_str())
    );
    // No healthy socket is inferred from mere output connectivity; an HTTP report reopens availability.
    presence.send_modify(|p| p.activity(Instant::now()));
    assert!(!presence.borrow().online());
    presence.send_modify(|p| p.report(true, Instant::now()));
    let (input_socket, _a) = sockets().await;
    let (output_socket, mut b) = sockets().await;
    let task = tokio::spawn(transport::relay(
        input_socket,
        output_socket,
        delivery.clone(),
        presence,
        stop.clone(),
    ));
    assert_eq!(text(&mut b).await, original);
    let subscription = text(&mut b).await;
    assert!(subscription.contains("\"type\":\"ping\""));
    stop.cancel();
    assert!(text(&mut b).await.contains("client_closed"));
    timeout(Duration::from_secs(3), task)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
}

#[tokio::test]
async fn malformed_outer_frame_is_ignored_but_unknown_control_stops_the_chain() {
    let delivery = delivery().await;
    let (input_socket, mut a) = sockets().await;
    let (output_socket, mut b) = sockets().await;
    let task = tokio::spawn(transport::relay(
        input_socket,
        output_socket,
        delivery.clone(),
        presence(),
        CancellationToken::new(),
    ));
    a.send(Message::Text(
        r#"{"type":"client_message","seq_id":false}"#.into(),
    ))
    .await
    .unwrap();
    a.send(Message::Text(input().into())).await.unwrap();
    let _ = text(&mut b).await;
    a.send(Message::Text(
        r#"{"type":"future_control","client_id":"phone","stream_id":"source","seq_id":2}"#.into(),
    ))
    .await
    .unwrap();
    assert!(text(&mut b).await.contains("client_closed"));
    let error = timeout(Duration::from_secs(3), task)
        .await
        .unwrap()
        .unwrap()
        .unwrap_err();
    assert!(!error.retryable());
    assert!(delivery.lock().await.router.keys().is_empty());
}

#[tokio::test]
async fn unverified_presence_never_accepts_a_buffered_command() {
    let delivery = delivery().await;
    let (input_socket, mut a) = sockets().await;
    let (output_socket, _b) = sockets().await;
    a.send(Message::Text(input().into())).await.unwrap();
    let result = transport::relay(
        input_socket,
        output_socket,
        delivery.clone(),
        watch::channel(Presence::default()).0,
        CancellationToken::new(),
    )
    .await;
    assert!(matches!(result, Err(TransportError::Offline)));
    assert!(delivery.lock().await.router.keys().is_empty());
}
