use bridge_core::wire::{Envelope, Kind, MAX_FRAME, Side, WireError};

#[test]
fn translates_only_outer_routing_and_preserves_business_bytes() {
    let payload = r#"{ "id":9007199254740993, "number":-0.0, "huge":1e400, "duplicate":1, "duplicate":2, "text":"\u4f60" }"#;
    let extension = r#"[1e-02, { "future": true }]"#;
    let input = format!(
        r#"{{"type":"client_message","client_id":"phone","stream_id":"original","seq_id":18446744073709551615,"cursor":"relay-a","message":{payload},"future":{extension}}}"#
    );
    let frame = Envelope::parse(Side::In, &input).unwrap();
    assert_eq!(frame.seq(), u64::MAX);
    let forwarded = frame
        .translate("controller", "mapped", Some("target-b"), u64::MAX)
        .unwrap();
    let parsed = Envelope::parse(Side::In, &forwarded).unwrap();
    assert_eq!(parsed.client(), "controller");
    assert_eq!(parsed.stream(), "mapped");
    assert_eq!(parsed.seq(), u64::MAX);
    assert_eq!(parsed.raw("message"), Some(payload));
    assert_eq!(parsed.raw("future"), Some(extension));
    assert!(parsed.raw("cursor").is_none());
    assert_eq!(parsed.required::<String>("env_id").unwrap(), "target-b");
}

#[test]
fn ack_cannot_be_forwarded_and_out_owner_is_checked_even_for_ack() {
    let ack = r#"{"type":"ack","client_id":"controller","stream_id":"mapped","env_id":"target-b","seq_id":3}"#;
    let frame = Envelope::parse(Side::Out, ack).unwrap();
    assert_eq!(frame.kind(), Kind::Ack);
    assert_eq!(
        frame.check_owner("other", "target-b"),
        Err(WireError::OwnerMismatch)
    );
    assert_eq!(
        frame.check_owner("controller", "other"),
        Err(WireError::OwnerMismatch)
    );
    frame.check_owner("controller", "target-b").unwrap();
    assert!(matches!(
        frame.translate("phone", "original", None, 3),
        Err(WireError::Unknown)
    ));
}

#[test]
fn malformed_and_unknown_control_are_distinct() {
    let base = r#"{"type":"ping","client_id":"phone","stream_id":"stream","seq_id":3}"#;
    for bad in [
        base.replace("\"seq_id\":3", "\"seq_id\":3.0"),
        base.replace("\"seq_id\":3", "\"seq_id\":18446744073709551616"),
        base.replace("\"seq_id\":3", "\"seq_id\":true"),
        base.replace("\"seq_id\":3", "\"seq_id\":-1"),
        base.replace("\"seq_id\":3", "\"seq_id\":3,\"seq_id\":4"),
        base.replace("\"seq_id\":3", "\"seq_id\":3,\"\\u0073eq_id\":4"),
        format!("{base} trailing"),
        base.replace("\"phone\"", "null"),
    ] {
        assert!(matches!(
            Envelope::parse(Side::In, &bad),
            Err(WireError::Malformed)
        ));
    }
    assert!(matches!(
        Envelope::parse(Side::In, &base.replace("ping", "future_control")),
        Err(WireError::Unknown)
    ));
    assert!(matches!(
        Envelope::parse(Side::Out, base),
        Err(WireError::Unknown)
    ));
    assert!(matches!(
        Envelope::parse(Side::In, &" ".repeat(MAX_FRAME + 1)),
        Err(WireError::Malformed)
    ));
}

#[test]
fn chunk_payload_and_extensions_remain_opaque() {
    let chunk = r#"{"type":"server_message_chunk","client_id":"controller","stream_id":"mapped","env_id":"target-b","seq_id":4,"cursor":"relay-b","segment_id":0,"segment_count":2,"message_size_bytes":90,"message_chunk_base64":"AA==","extra": {"a":1e-09}}"#;
    let frame = Envelope::parse(Side::Out, chunk).unwrap();
    let translated = frame.translate("phone", "original", None, 4).unwrap();
    assert!(translated.contains(r#""message_chunk_base64":"AA==""#));
    assert!(translated.contains(r#""extra":{"a":1e-09}"#));
    assert!(!translated.contains("env_id"));
    assert!(!translated.contains("cursor"));
    for bad in [
        chunk.replace("\"segment_id\":0", "\"segment_id\":2"),
        chunk.replace("\"segment_count\":2", "\"segment_count\":1"),
        chunk.replace("\"message_size_bytes\":90", "\"message_size_bytes\":0"),
        chunk.replace("\"AA==\"", "null"),
    ] {
        assert!(matches!(
            Envelope::parse(Side::Out, &bad),
            Err(WireError::Malformed)
        ));
    }
}
