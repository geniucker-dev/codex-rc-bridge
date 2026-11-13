use bridge_core::{
    delivery::{Delivery, DeliveryError, Limits, Outbox, Outcome},
    presence::Presence,
    routing::{Router, StreamKey},
    storage::SqliteStore,
    wire::{Envelope, Side, WireError},
};
use sqlx::sqlite::SqliteConnectOptions;
use std::sync::Arc;
use tokio::time::{Duration, Instant};
use uuid::Uuid;

async fn store() -> Arc<SqliteStore> {
    let store = Arc::new(
        SqliteStore::open(SqliteConnectOptions::new().in_memory(true))
            .await
            .unwrap(),
    );
    store.create_tenant(Uuid::from_u128(1)).await.unwrap();
    store
}
fn router(store: Arc<SqliteStore>) -> Router {
    Router::new(
        store,
        Uuid::from_u128(1),
        Uuid::from_u128(2),
        "controller-b".into(),
        "environment-b".into(),
    )
}
fn delivery(store: Arc<SqliteStore>) -> Delivery {
    Delivery::new(router(store), Limits::default())
}
fn source() -> StreamKey {
    StreamKey {
        client: "phone-a".into(),
        stream: "stream-a".into(),
    }
}
fn input(seq: u64, cursor: &str) -> String {
    serde_json::json!({"type":"client_message","client_id":"phone-a","stream_id":"stream-a","seq_id":seq,"cursor":cursor,"message":{"id":seq,"test":true}}).to_string()
}
fn output(mapped: &str, seq: u64, cursor: &str) -> String {
    serde_json::json!({"type":"server_message","client_id":"controller-b","env_id":"environment-b","stream_id":mapped,"seq_id":seq,"cursor":cursor,"message":{"id":seq,"test":true}}).to_string()
}
fn pong(mapped: &str, seq: u64, status: &str) -> String {
    serde_json::json!({"type":"pong","client_id":"controller-b","env_id":"environment-b","stream_id":mapped,"seq_id":seq,"status":status}).to_string()
}
fn chunk(seq: u64, part: u64, cursor: &str) -> String {
    serde_json::json!({"type":"client_message_chunk","client_id":"phone-a","stream_id":"stream-a","seq_id":seq,"cursor":cursor,"segment_id":part,"segment_count":2,"message_size_bytes":8,"message_chunk_base64":"e30="}).to_string()
}
fn ack(seq: u64, part: Option<u64>, count: Option<u64>) -> Envelope {
    let mut value =
        serde_json::json!({"type":"ack","client_id":"phone-a","stream_id":"stream-a","seq_id":seq});
    if let Some(part) = part {
        value["segment_id"] = part.into();
    }
    if let Some(count) = count {
        value["segment_count"] = count.into();
    }
    Envelope::parse(Side::In, &value.to_string()).unwrap()
}
fn drain(outbox: &mut Outbox) -> Vec<String> {
    let mut frames = Vec::new();
    while let Some((id, segment, raw)) = outbox.next_frame() {
        frames.push(raw);
        outbox.sent(&id, segment);
    }
    frames
}

#[tokio::test]
async fn hard_switch_discards_partial_chunks_and_queues_and_rejects_old_target_responses() {
    let store = store().await;
    let now = Instant::now();
    let mut old = delivery(store.clone());
    old.receive(Side::In, &chunk(1, 0, "old-a"), now)
        .await
        .unwrap();
    let old_mapping = old.router.mapped(&source()).unwrap().to_owned();
    assert!(!old.outboxes[1].is_empty());
    old.retire_all().unwrap();
    drop(old);
    let mut next = Delivery::new(
        Router::new(
            store.clone(),
            Uuid::from_u128(1),
            Uuid::from_u128(2),
            "controller-c".into(),
            "environment-c".into(),
        ),
        Limits::default(),
    );
    assert_eq!(
        next.receive(Side::In, &chunk(1, 1, "late-old-a"), now)
            .await
            .unwrap(),
        Outcome::Retired
    );
    assert!(next.outboxes[1].is_empty());
    assert!(matches!(
        next.receive(Side::Out, &output(&old_mapping, 1, "late-old-b"), now)
            .await,
        Err(DeliveryError::Wire(WireError::OwnerMismatch))
    ));
    let fresh = input(1, "fresh-a").replace("stream-a", "new-source");
    assert_eq!(
        next.receive(Side::In, &fresh, now).await.unwrap(),
        Outcome::Forwarded { activity: false }
    );
    let frames = drain(&mut next.outboxes[1]);
    assert_eq!(frames.len(), 1);
    let forwarded = Envelope::publication(Side::Out, &frames[0]).unwrap();
    forwarded
        .check_owner("controller-c", "environment-c")
        .unwrap();
    assert_ne!(forwarded.stream(), old_mapping);
    assert_eq!(next.subscribe_cursor(Side::Out), None);
    next.retire_all().unwrap();
    drop(next);
    let mut back = delivery(store);
    assert_eq!(
        back.receive(Side::In, &input(2, "old-replay"), now)
            .await
            .unwrap(),
        Outcome::Retired
    );
    assert_eq!(
        back.receive(Side::In, &fresh, now).await.unwrap(),
        Outcome::Retired
    );
    assert!(back.outboxes[1].is_empty());
}

#[test]
fn cumulative_ack_does_not_drain_past_incomplete_chunk_group() {
    let mut outbox = Outbox::new(Limits::default());
    outbox.put(Side::Out, chunk(1, 0, "c1"), false).unwrap();
    outbox.put(Side::Out, chunk(1, 1, "c2"), false).unwrap();
    outbox.put(Side::Out, input(2, "c3"), false).unwrap();
    drain(&mut outbox);
    outbox.acknowledge(&ack(2, None, None)).unwrap();
    assert_eq!(outbox.len(), 3);
    outbox.acknowledge(&ack(1, Some(0), Some(2))).unwrap();
    assert_eq!(outbox.len(), 3);
    outbox.reconnect();
    assert_eq!(drain(&mut outbox).len(), 3);
    assert!(matches!(
        outbox.acknowledge(&ack(1, Some(2), Some(2))),
        Err(DeliveryError::Wire(WireError::Malformed))
    ));
    assert!(matches!(
        outbox.acknowledge(&ack(1, Some(1), Some(3))),
        Err(DeliveryError::Wire(WireError::Malformed))
    ));
    outbox.acknowledge(&ack(1, Some(1), Some(2))).unwrap();
    assert!(outbox.is_empty());
    // Old/unknown ACKs cannot set the watermark for a newly queued publication.
    outbox.acknowledge(&ack(100, None, None)).unwrap();
    outbox.put(Side::Out, input(3, "c4"), false).unwrap();
    assert_eq!(outbox.len(), 1);
}

#[tokio::test]
async fn reconnect_keeps_mapping_and_replays_identical_bytes_but_restart_retires_source() {
    let store = store().await;
    let mut link = delivery(store.clone());
    let now = Instant::now();
    assert_eq!(
        link.receive(Side::In, &input(1, "a1"), now).await.unwrap(),
        Outcome::Forwarded { activity: false }
    );
    let first = drain(&mut link.outboxes[1]);
    let mapped = link.router.mapped(&source()).unwrap().to_owned();
    assert_ne!(mapped, source().stream);
    assert_eq!(link.subscribe_cursor(Side::In), Some("a1"));
    link.reconnect().unwrap();
    let replay = drain(&mut link.outboxes[1]);
    assert_eq!(first[0], replay[0]);
    assert_eq!(link.router.mapped(&source()), Some(mapped.as_str()));
    assert!(replay.iter().any(|r| {
        Envelope::publication(Side::Out, r)
            .unwrap()
            .raw("skip_history")
            == Some("true")
    }));
    let mut restart = delivery(store);
    assert_eq!(
        restart
            .receive(Side::In, &input(1, "a1"), now)
            .await
            .unwrap(),
        Outcome::Retired
    );
    assert!(restart.router.keys().is_empty());
    assert!(restart.outboxes[1].is_empty());
    let notice = restart.outboxes[0].next_frame().unwrap().2;
    assert!(notice.contains("unknown"));
}

#[tokio::test]
async fn side_local_acks_and_cursors_never_cross_accounts() {
    let mut link = delivery(store().await);
    let now = Instant::now();
    link.receive(Side::In, &input(1, "a-only"), now)
        .await
        .unwrap();
    let mapped = link.router.mapped(&source()).unwrap().to_owned();
    let raw = output(&mapped, 1, "b-only");
    assert_eq!(
        link.receive(Side::Out, &raw, now).await.unwrap(),
        Outcome::Forwarded { activity: true }
    );
    assert_eq!(link.subscribe_cursor(Side::In), Some("a-only"));
    assert_eq!(link.subscribe_cursor(Side::Out), Some("b-only"));
    let forwarded = link.outboxes[0].next_frame().unwrap().2;
    assert!(!forwarded.contains("b-only"));
    assert!(!forwarded.contains("env_id"));
    let ack=serde_json::json!({"type":"ack","client_id":"controller-b","stream_id":mapped,"env_id":"environment-b","seq_id":1,"cursor":"not-a-checkpoint"}).to_string();
    assert_eq!(
        link.receive(Side::Out, &ack, now).await.unwrap(),
        Outcome::Ack
    );
    assert!(link.outboxes[1].is_empty());
    assert_eq!(link.outboxes[0].len(), 1);
    assert_eq!(link.subscribe_cursor(Side::Out), Some("b-only"));
    assert_eq!(
        link.receive(Side::Out, &raw, now).await.unwrap(),
        Outcome::Duplicate
    );
    assert_eq!(link.outboxes[0].len(), 1);
    assert!(matches!(
        link.receive(Side::Out, &raw.replace("controller-b", "foreign"), now)
            .await,
        Err(DeliveryError::Wire(WireError::OwnerMismatch))
    ));
}

#[tokio::test]
async fn partial_chunks_freeze_cursor_and_defer_subscription_ping() {
    let mut link = delivery(store().await);
    let now = Instant::now();
    link.receive(Side::In, &input(1, "safe"), now)
        .await
        .unwrap();
    link.receive(Side::In, &chunk(2, 0, "partial"), now)
        .await
        .unwrap();
    assert_eq!(link.subscribe_cursor(Side::In), Some("safe"));
    link.reconnect().unwrap();
    assert!(
        !drain(&mut link.outboxes[1])
            .iter()
            .any(|r| r.contains("\"type\":\"ping\""))
    );
    assert_eq!(
        link.receive(Side::In, &chunk(2, 0, "partial"), now)
            .await
            .unwrap(),
        Outcome::Duplicate
    );
    link.receive(Side::In, &chunk(2, 1, "complete"), now)
        .await
        .unwrap();
    assert_eq!(link.subscribe_cursor(Side::In), Some("complete"));
    let sent = drain(&mut link.outboxes[1]);
    assert_eq!(sent.len(), 2);
    assert!(sent[1].contains("\"type\":\"ping\""));
}

#[tokio::test]
async fn gap_rewinds_only_its_side_and_expires_without_replaying_old_commands() {
    let mut link = delivery(store().await);
    let now = Instant::now();
    link.receive(Side::In, &input(1, "a1"), now).await.unwrap();
    assert!(matches!(
        link.receive(Side::In, &input(3, "a3"), now).await,
        Err(DeliveryError::ReplayGap)
    ));
    assert_eq!(link.subscribe_cursor(Side::In), Some("a1"));
    link.receive(Side::In, &input(2, "a2"), now).await.unwrap();
    link.receive(Side::In, &input(3, "a3"), now).await.unwrap();
    assert_eq!(link.subscribe_cursor(Side::In), Some("a3"));
    assert!(matches!(
        link.receive(Side::In, &input(5, "a5"), now).await,
        Err(DeliveryError::ReplayGap)
    ));
    link.maintain(now + Duration::from_secs(31), false).unwrap();
    assert!(link.router.keys().is_empty());
    assert_eq!(
        link.receive(Side::In, &input(4, "a4"), now + Duration::from_secs(32))
            .await
            .unwrap(),
        Outcome::Retired
    );
    assert!(
        drain(&mut link.outboxes[1])
            .iter()
            .all(|r| r.contains("client_closed"))
    );
}

#[tokio::test]
async fn unknown_initial_pong_preserves_initialization_but_established_unknown_retires() {
    let mut link = delivery(store().await);
    let now = Instant::now();
    link.receive(Side::In, &input(1, "a1"), now).await.unwrap();
    let mapped = link.router.mapped(&source()).unwrap().to_owned();
    assert_eq!(
        link.receive(Side::Out, &pong(&mapped, 0, "unknown"), now)
            .await
            .unwrap(),
        Outcome::Forwarded { activity: false }
    );
    assert_eq!(
        link.receive(Side::Out, &output(&mapped, 1, "b1"), now)
            .await
            .unwrap(),
        Outcome::Forwarded { activity: true }
    );
    assert_eq!(
        link.receive(Side::Out, &pong(&mapped, 0, "unknown"), now)
            .await
            .unwrap(),
        Outcome::Retired
    );
    assert!(link.router.keys().is_empty());
    assert_eq!(
        link.receive(Side::Out, &output(&mapped, 2, "b2"), now)
            .await
            .unwrap(),
        Outcome::Unmapped
    );
}

#[tokio::test]
async fn capacity_failure_retires_affected_stream_and_partial_data_is_not_activity() {
    let store = store().await;
    let now = Instant::now();
    let mut full = Delivery::new(
        router(store.clone()),
        Limits {
            bytes: 1,
            frames: 1,
        },
    );
    assert_eq!(
        full.receive(Side::In, &input(1, "a1"), now).await.unwrap(),
        Outcome::Retired
    );
    assert!(full.router.keys().is_empty());
    let mut link = Delivery::new(
        Router::new(
            store,
            Uuid::from_u128(1),
            Uuid::from_u128(3),
            "controller-b".into(),
            "environment-b".into(),
        ),
        Limits::default(),
    );
    link.receive(Side::In, &input(1, "a1"), now).await.unwrap();
    let mapped = link.router.mapped(&source()).unwrap().to_owned();
    for part in 0..2 {
        let mut value: serde_json::Value = serde_json::from_str(&chunk(1, part, "b1")).unwrap();
        value["type"] = "server_message_chunk".into();
        value["client_id"] = "controller-b".into();
        value["env_id"] = "environment-b".into();
        value["stream_id"] = mapped.clone().into();
        assert_eq!(
            link.receive(Side::Out, &value.to_string(), now)
                .await
                .unwrap(),
            Outcome::Forwarded {
                activity: part == 1
            }
        );
    }
}

#[test]
fn only_http_opens_presence_and_explicit_offline_overrides_activity() {
    let now = Instant::now();
    let mut presence = Presence::default();
    presence.activity(now);
    assert!(!presence.online());
    presence.report(true, now);
    presence.activity(now + Duration::from_secs(29));
    presence.expire(now + Duration::from_secs(31));
    assert!(presence.online());
    presence.expire(now + Duration::from_secs(119));
    assert!(!presence.online());
    presence.activity(now + Duration::from_secs(120));
    assert!(!presence.online());
    presence.report(true, now + Duration::from_secs(121));
    presence.activity(now + Duration::from_secs(122));
    presence.report(false, now + Duration::from_secs(123));
    presence.activity(now + Duration::from_secs(124));
    assert!(!presence.online());
}

#[tokio::test]
async fn unseen_fragment_gap_remains_retired_across_restart_and_rejects_failed_commit() {
    let path = std::env::temp_dir().join(format!("bridge-gap-{}.sqlite", Uuid::new_v4()));
    let options = SqliteConnectOptions::new()
        .filename(&path)
        .create_if_missing(true);
    let store = Arc::new(SqliteStore::open(options.clone()).await.unwrap());
    store.create_tenant(Uuid::from_u128(1)).await.unwrap();
    let probe = sqlx::SqlitePool::connect_with(options.clone())
        .await
        .unwrap();
    let now = Instant::now();
    let mut link = delivery(store.clone());
    sqlx::query("CREATE TRIGGER fail_rejection BEFORE INSERT ON used_streams BEGIN SELECT RAISE(ABORT, 'synthetic rejection write failure'); END")
        .execute(&probe).await.unwrap();
    assert!(matches!(
        link.receive(Side::In, &chunk(1, 1, "first-gap"), now).await,
        Err(DeliveryError::Routing(
            bridge_core::routing::RoutingError::Storage
        ))
    ));
    assert!(link.router.keys().is_empty());
    assert!(
        link.outboxes.iter().all(Outbox::is_empty),
        "failed commit cannot announce completed retirement"
    );
    sqlx::query("DROP TRIGGER fail_rejection")
        .execute(&probe)
        .await
        .unwrap();
    assert_eq!(
        link.receive(Side::In, &chunk(1, 1, "first-gap"), now)
            .await
            .unwrap(),
        Outcome::Retired
    );
    assert_eq!(
        link.receive(Side::In, &input(2, "late-complete"), now)
            .await
            .unwrap(),
        Outcome::Retired
    );
    assert!(link.router.keys().is_empty());
    assert!(link.outboxes[1].is_empty());
    drop(link);
    probe.close().await;
    Arc::try_unwrap(store).ok().unwrap().close().await;
    let reopened = Arc::new(SqliteStore::open(options).await.unwrap());
    let mut next = delivery(reopened.clone());
    assert_eq!(
        next.receive(Side::In, &input(3, "after-restart"), now)
            .await
            .unwrap(),
        Outcome::Retired
    );
    assert!(next.outboxes[1].is_empty());
    // A genuinely new source still initializes normally.
    let fresh = input(1, "new-initialization").replace("stream-a", "new-stream");
    assert_eq!(
        next.receive(Side::In, &fresh, now).await.unwrap(),
        Outcome::Forwarded { activity: false }
    );
    drop(next);
    Arc::try_unwrap(reopened).ok().unwrap().close().await;
    std::fs::remove_file(path).unwrap();
}
