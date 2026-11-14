//! Unit tests for the serve subsystem.
use super::*;
use crate::transport::serve::connection::{DeltaKind, queue_delta, take_delta};
use crate::transport::serve::tcp::{TcpTransport, classify_head};
use nuo_protocol::{AgentRequest, AgentResponse, ERR_PROTOCOL_MISMATCH};
use std::sync::Arc;
use tokio_tungstenite::tungstenite;
use tokio_tungstenite::tungstenite::handshake::server::Request;

#[tokio::test]
async fn conn_table_tracks_interactive_clients_and_notifies() {
    let table = ConnTable::new();
    assert_eq!(table.interactive_count(), 0);
    assert!(!table.has_had_interactive());

    let guard1 = table.register_interactive();
    assert_eq!(table.interactive_count(), 1);
    assert!(table.has_had_interactive());

    let guard2 = table.register_interactive();
    assert_eq!(table.interactive_count(), 2);

    drop(guard1);
    assert_eq!(table.interactive_count(), 1);

    drop(guard2);
    assert_eq!(table.interactive_count(), 0);
    assert!(table.has_had_interactive());
}

#[tokio::test]
async fn dynamic_tcp_manager_lifecycle() {
    let registry = Arc::new(crate::runtime::registry::ThreadRegistry::prehost_only());
    let conns = Arc::new(ConnTable::new());
    let gate = Arc::new(ShutdownGate::new());
    let tasks = Arc::new(crate::runtime::shutdown::TaskBook::new());
    let mgr = TcpManager {
        port: Arc::new(AtomicU16::new(0)),
        token: Arc::new(RwLock::new(None)),
        cancel: Arc::new(RwLock::new(None)),
        expose: ServeExpose::Local,
        local_endpoint: None,
        registry,
        conns,
        gate,
        tasks,
    };

    let initial = mgr.get_endpoints().await;
    assert_eq!(initial.tcp_port, 0);
    assert!(!initial.tcp_active);

    // Open ephemeral port
    let opened = mgr
        .open_tcp(Some(0))
        .await
        .expect("dynamic open tcp succeeds");
    assert!(opened.tcp_port > 0);
    assert!(opened.tcp_active);
    assert!(opened.token.is_some());

    // Close TCP listener
    let closed = mgr.close_tcp().await.expect("close tcp succeeds");
    assert_eq!(closed.tcp_port, 0);
    assert!(!closed.tcp_active);
}
/// Deltas coalesce only while they continue the same channel; a channel or
/// thread change displaces the run so the caller can preserve order.
#[test]
fn stream_deltas_coalesce_until_the_channel_changes() {
    let mut pending = None;
    let context = nuo_protocol::TurnContext::new(1, 0, 1_700_000_000_000);
    assert!(
        queue_delta(
            &mut pending,
            "t".into(),
            context,
            DeltaKind::Text,
            "he".into()
        )
        .is_none(),
        "the first delta opens a run"
    );
    assert!(
        queue_delta(
            &mut pending,
            "t".into(),
            context,
            DeltaKind::Text,
            "llo".into()
        )
        .is_none(),
        "a contiguous text delta joins the run"
    );
    assert_eq!(pending.as_ref().expect("run open").text, "hello");

    let displaced = queue_delta(
        &mut pending,
        "t".into(),
        context,
        DeltaKind::Reasoning,
        "why".into(),
    )
    .expect("a channel change displaces the text run");
    assert_eq!(displaced.kind, DeltaKind::Text);
    assert_eq!(displaced.text, "hello");
    assert_eq!(
        pending.as_ref().expect("run open").kind,
        DeltaKind::Reasoning
    );

    let displaced = queue_delta(
        &mut pending,
        "u".into(),
        context,
        DeltaKind::Reasoning,
        "x".into(),
    )
    .expect("a thread change displaces the run");
    assert_eq!(displaced.thread_id, "t");
    assert_eq!(displaced.text, "why");

    let next_turn = nuo_protocol::TurnContext::new(1, 1, 1_700_000_000_001);
    let displaced = queue_delta(
        &mut pending,
        "u".into(),
        next_turn,
        DeltaKind::Reasoning,
        "y".into(),
    )
    .expect("a turn change displaces the run: positions are not coalescible");
    assert_eq!(displaced.context, context);
}

/// Only the two streamed delta variants are coalescible; everything else
/// must be flushed through in order.
#[test]
fn take_delta_matches_only_stream_events() {
    for (event, expected_kind) in [
        (
            nuo_protocol::RoundEvent::StreamDelta("x".into()),
            DeltaKind::Text,
        ),
        (
            nuo_protocol::RoundEvent::StreamReasoningDelta("r".into()),
            DeltaKind::Reasoning,
        ),
    ] {
        let context = nuo_protocol::TurnContext::new(1, 0, 0);
        let (thread_id, seen, kind, text) = take_delta(AgentResponse::Round {
            thread_id: "s".into(),
            scope: nuo_protocol::RoundScope::Turn { context, event },
        })
        .expect("a stream delta");
        assert_eq!(thread_id, "s");
        assert_eq!(seen, context);
        assert_eq!(kind, expected_kind);
        assert!(!text.is_empty());
    }

    let non_delta = AgentResponse::Round {
        thread_id: "s".into(),
        scope: nuo_protocol::RoundScope::Turn {
            context: nuo_protocol::TurnContext::new(1, 0, 0),
            event: nuo_protocol::RoundEvent::StreamEnd("done".into()),
        },
    };
    assert!(
        take_delta(non_delta).is_err(),
        "a stream end is an ordering barrier, not a delta"
    );

    let session_delta = AgentResponse::Round {
        thread_id: "s".into(),
        scope: nuo_protocol::RoundScope::Session {
            event: nuo_protocol::RoundEvent::StreamDelta("x".into()),
        },
    };
    assert!(
        take_delta(session_delta).is_err(),
        "a stream delta without a turn context is a producer bug, not a coalescible run"
    );
}

#[test]
fn end_thread_serializes_as_a_nested_unit_variant() {
    // The envelope carries its payload in a named field; flattening it
    // forced the decoder to buffer every frame into an intermediate map.
    // A unit `AgentRequest` is an externally tagged variant, hence the
    // string body.
    let text = serde_json::to_string(&Wire::Request {
        request: AgentRequest::EndThread,
    })
    .expect("serialize EndThread");
    assert_eq!(text, r#"{"type":"Request","request":"EndThread"}"#);
}
#[test]
fn generate_token_is_nonempty_hex() {
    let t = generate_token();
    // Two UUIDv4 simple encodings: 2 × 32 hex chars.
    assert_eq!(t.len(), 64);
    assert!(t.chars().all(|c| c.is_ascii_hexdigit()));
}
#[test]
fn generate_token_never_repeats() {
    let mut seen = std::collections::HashSet::new();
    for _ in 0..1024 {
        let t = generate_token();
        assert_eq!(t.len(), 64);
        assert!(seen.insert(t), "token collision");
    }
}
#[test]
fn constant_time_eq_matches_only_exact_equality() {
    assert!(constant_time_eq(b"", b""));
    assert!(constant_time_eq(b"sekret", b"sekret"));
    // Same length, one byte off.
    assert!(!constant_time_eq(b"sekret", b"sekreT"));
    // Prefix / extension / empty must not match (no length leak via an
    // early return: the fold covers length too).
    assert!(!constant_time_eq(b"sek", b"sekret"));
    assert!(!constant_time_eq(b"sekrets", b"sekret"));
    assert!(!constant_time_eq(b"", b"sekret"));
}
#[test]
fn check_bearer_accepts_only_the_exact_token() {
    fn req(auth: Option<&str>) -> Request {
        let mut builder = tungstenite::http::Request::builder();
        if let Some(value) = auth {
            builder = builder.header("Authorization", value);
        }
        builder.body(()).unwrap()
    }
    let expected = "0123456789abcdef";
    assert!(check_bearer(
        &req(Some(&format!("Bearer {expected}"))),
        expected
    ));
    // Surrounding whitespace on the header value is tolerated.
    assert!(check_bearer(
        &req(Some(&format!("Bearer  {expected} "))),
        expected
    ));
    assert!(!check_bearer(
        &req(Some("Bearer 0123456789abcde")),
        expected
    ));
    assert!(!check_bearer(
        &req(Some("Bearer 0123456789abcdef0")),
        expected
    ));
    assert!(!check_bearer(
        &req(Some("Basic 0123456789abcdef")),
        expected
    ));
    assert!(!check_bearer(&req(None), expected));
}
#[test]
fn attach_action_roundtrips() {
    assert_eq!(
        serde_json::to_string(&AttachAction::New(None)).unwrap(),
        "\"new\""
    );
    assert_eq!(
        serde_json::to_string(&AttachAction::Attach(Some("abc".into()))).unwrap(),
        r#"{"attach":"abc"}"#
    );
    let back: AttachAction = serde_json::from_str(r#"{"attach":"abc"}"#).unwrap();
    assert_eq!(back, AttachAction::Attach(Some("abc".into())));
    // The picker action serializes as a bare unit variant;
    // an older server that does not know it fails the Select frame with
    // a clear deserialize error instead of a mid-handshake protocol
    // fault.
    assert_eq!(
        serde_json::to_string(&AttachAction::Picker(None)).unwrap(),
        "\"picker\""
    );
    let back: AttachAction = serde_json::from_str("\"picker\"").unwrap();
    assert_eq!(back, AttachAction::Picker(None));
}

fn handshake_req(headers: &[(&str, &str)]) -> Request {
    let mut builder = tungstenite::http::Request::builder();
    for (name, value) in headers {
        builder = builder.header(*name, *value);
    }
    builder.body(()).unwrap()
}

#[test]
fn loopback_host_detection() {
    assert!(is_loopback_host("127.0.0.1"));
    assert!(is_loopback_host("localhost"));
    assert!(is_loopback_host("foo.localhost"));
    assert!(is_loopback_host("::1"));
    assert!(!is_loopback_host("example.com"));
    assert!(!is_loopback_host("127.0.0.1.evil.com"));
    assert_eq!(host_part("127.0.0.1:9800"), "127.0.0.1");
    assert_eq!(host_part("[::1]:9800"), "::1");
    assert_eq!(host_part("Example.COM:443"), "example.com");
}

#[test]
fn origin_policy_refuses_foreign_browser_pages_on_loopback() {
    // A page served from an external site: rejected.
    let foreign = handshake_req(&[("Origin", "https://evil.example")]);
    assert!(validate_origin(&foreign, ServeExpose::Local).is_err());
    // The panel served by this server (or a local dev server): allowed.
    let local = handshake_req(&[("Origin", "http://127.0.0.1:9800")]);
    assert!(validate_origin(&local, ServeExpose::Local).is_ok());
    let localhost = handshake_req(&[("Origin", "http://localhost:5173")]);
    assert!(validate_origin(&localhost, ServeExpose::Local).is_ok());
    // Non-browser clients send no Origin: governed by the token instead.
    let none = handshake_req(&[]);
    assert!(validate_origin(&none, ServeExpose::Local).is_ok());
    // A sandboxed/file page ("null" origin) is foreign.
    let null = handshake_req(&[("Origin", "null")]);
    assert!(validate_origin(&null, ServeExpose::Local).is_err());
    // Public listeners skip the origin policy (token is the boundary).
    assert!(validate_origin(&foreign, ServeExpose::Public).is_ok());
}

#[test]
fn credentials_accept_bearer_or_subprotocol() {
    let expected = "0123456789abcdef";
    // Authorization header wins.
    let bearer = handshake_req(&[("Authorization", &format!("Bearer {expected}"))]);
    assert_eq!(check_credentials(&bearer, expected).unwrap(), None);
    // The browser channel: bearer.<token> subprotocol offer is accepted
    // and must be echoed (Ok(Some(..))).
    let sub = handshake_req(&[("Sec-WebSocket-Protocol", &format!("bearer.{expected}"))]);
    assert_eq!(
        check_credentials(&sub, expected).unwrap(),
        Some(format!("bearer.{expected}"))
    );
    // Among several offers, the matching one is picked.
    let multi = handshake_req(&[(
        "Sec-WebSocket-Protocol",
        &format!("chat, bearer.{expected}, other"),
    )]);
    assert!(check_credentials(&multi, expected).unwrap().is_some());
    // Wrong token / no credentials: rejected.
    let wrong = handshake_req(&[("Sec-WebSocket-Protocol", "bearer.nope")]);
    assert!(check_credentials(&wrong, expected).is_err());
    let none = handshake_req(&[]);
    assert!(check_credentials(&none, expected).is_err());
}

#[test]
fn classify_splits_websocket_from_plain_http() {
    let ws = b"GET / HTTP/1.1\r\nHost: 127.0.0.1:9800\r\nUpgrade: websocket\r\nConnection: Upgrade\r\n\r\n";
    assert_eq!(classify_head(ws), TcpTransport::WebSocket);
    let ws_lower = b"GET / HTTP/1.1\r\nupgrade: WebSocket\r\n\r\n";
    assert_eq!(classify_head(ws_lower), TcpTransport::WebSocket);
    let http = b"GET /index.html HTTP/1.1\r\nHost: 127.0.0.1\r\n\r\n";
    assert_eq!(classify_head(http), TcpTransport::Http);
    let head = b"HEAD /healthz HTTP/1.0\r\n\r\n";
    assert_eq!(classify_head(head), TcpTransport::Http);
    // Not HTTP at all: handed to the WS path to reject properly.
    let garbage = b"nonsense\r\n\r\n";
    assert_eq!(classify_head(garbage), TcpTransport::WebSocket);
}

#[test]
fn error_frame_carries_an_optional_code() {
    let with_code = serde_json::to_string(&Wire::Error {
        message: "m".to_string(),
        code: Some(ERR_PROTOCOL_MISMATCH.to_string()),
    })
    .unwrap();
    assert_eq!(
        with_code,
        r#"{"type":"Error","message":"m","code":"protocol_mismatch"}"#
    );
    // Absent code stays absent on the wire (older clients unaffected).
    let without = serde_json::to_string(&Wire::Error {
        message: "m".to_string(),
        code: None,
    })
    .unwrap();
    assert_eq!(without, r#"{"type":"Error","message":"m"}"#);
    // And a frame from an older server (no code) still parses.
    let back: Wire = serde_json::from_str(r#"{"type":"Error","message":"m"}"#).unwrap();
    assert!(matches!(back, Wire::Error { code: None, .. }));
}

#[test]
fn attach_sync_buffer_is_latest_value_and_non_destructive() {
    let mut buf = AttachSyncBuffer::new();
    assert!(buf.is_empty());
    assert_eq!(buf.len(), 0);

    let keys1 = AgentResponse::ProviderKeys(vec![("openai".to_string(), true)]);
    buf.observe(&keys1);
    assert_eq!(buf.len(), 1);

    // Snapshot is non-destructive (calling it repeatedly does not drain)
    let snap1 = buf.snapshot();
    assert_eq!(snap1.len(), 1);
    let snap2 = buf.snapshot();
    assert_eq!(snap2.len(), 1);

    // Observe multiple turns of ContextTokens
    for i in 1..=10 {
        let tokens = AgentResponse::Round {
            thread_id: "s1".to_string(),
            scope: nuo_protocol::RoundScope::Session {
                event: nuo_protocol::RoundEvent::ContextTokens(
                    nuo_protocol::ContextTokenSnapshot::new(
                        i * 100,
                        nuo_protocol::ContextTokenSource::Projection,
                    ),
                ),
            },
        };
        buf.observe(&tokens);
    }

    // Buffer length has at most keys + context tokens = 2
    assert_eq!(buf.len(), 2);

    // Later update to keys overwrites rather than growing unbounded
    let keys2 = AgentResponse::ProviderKeys(vec![("anthropic".to_string(), true)]);
    buf.observe(&keys2);
    assert_eq!(buf.len(), 2);
    let snap3 = buf.snapshot();
    assert_eq!(snap3.len(), 2);
    match &snap3[0] {
        AgentResponse::ProviderKeys(s) => {
            assert_eq!(s, &vec![("anthropic".to_string(), true)])
        }
        _ => panic!("expected ProviderKeys"),
    }
}
