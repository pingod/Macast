//! Channel tests. Every test opens with `crate::pin_test_dir();` — the same
//! rule `proto` and `cert` follow — and none of them touches the real config
//! directory or the real ports: message handlers are driven directly, replies
//! are read back from the test outbox (the wire bytes, not a mock of the
//! builder), and the end-to-end shape binds an OS-assigned port.

use super::*;
use serde_json::Value;
use serde_json::json;
use std::net::{TcpListener, TcpStream};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use macast_core::logging::Logger;
use macast_upnp::store::{self, Broadcast, SharedBroadcast, SharedRenderer, SharedStore, StateStore};

use crate::proto::{
    encode_cast_message, parse_cast_message, APP_NAMESPACES,
};

/// A store that answers the watcher's two transport questions, and a renderer
/// that records every call the channel makes on its way to the player.
struct Harness {
    store: Arc<RecordingStore>,
    renderer: Arc<RecordingRenderer>,
    channel: Arc<CastChannel>,
}

struct RecordingStore {
    transport_state: Mutex<String>,
    transport_status: Mutex<String>,
}

impl StateStore for RecordingStore {
    fn read(&self, name: &str) -> Option<Value> {
        match name {
            "TransportState" => Some(json!(self.transport_state.lock().unwrap().clone())),
            "TransportStatus" => Some(json!(self.transport_status.lock().unwrap().clone())),
            _ => None,
        }
    }
    fn write(&self, _name: &str, _value: Value) -> bool {
        false
    }
    fn playlist_push(&self, _uri: &str) -> (i64, i64) {
        (0, 0)
    }
    fn playlist(&self) -> Vec<String> {
        Vec::new()
    }
    fn playlist_index(&self) -> i64 {
        -1
    }
    fn set_playlist_index(&self, _index: i64) {}
    fn add_history(&self, _title: &str, _uri: &str) {}
}

struct RecordingRenderer {
    calls: Mutex<Vec<String>>,
}

impl RecordingRenderer {
    fn record(&self, line: String) {
        self.calls.lock().unwrap().push(line);
    }
    fn has(&self, needle: &str) -> bool {
        self.calls.lock().unwrap().iter().any(|c| c.contains(needle))
    }
}

impl store::Renderer for RecordingRenderer {
    fn note_media_live(&self, live: Option<bool>) {
        self.record(format!("note_media_live={live:?}"));
    }
    fn set_media_url(&self, uri: &str, start: Option<&str>) {
        self.record(format!("set_media_url={uri} start={start:?}"));
    }
    fn set_media_title(&self, title: &str) {
        self.record(format!("set_media_title={title}"));
    }
    fn set_media_position(&self, target: &str) {
        self.record(format!("set_media_position={target}"));
    }
    fn set_media_volume(&self, value: Value) {
        self.record(format!("set_media_volume={}", store::wire(&value)));
    }
    fn set_media_mute(&self, value: Value) {
        self.record(format!("set_media_mute={}", store::wire(&value)));
    }
    fn set_media_pause(&self) {
        self.record("set_media_pause".into());
    }
    fn set_media_resume(&self) {
        self.record("set_media_resume".into());
    }
    fn set_media_stop(&self) {
        self.record("set_media_stop".into());
    }
    fn title_for_uri(&self, uri: &str) -> String {
        format!("name-of-{uri}")
    }
}

struct NoBroadcast;
impl Broadcast for NoBroadcast {
    fn enqueue(&self, _name: &str, _value: &Value) {}
}

impl Harness {
    fn new() -> Self {
        crate::pin_test_dir();
        let log = Logger::named("Chromecast");
        let store = Arc::new(RecordingStore {
            transport_state: Mutex::new(String::new()),
            transport_status: Mutex::new(String::new()),
        });
        let renderer = Arc::new(RecordingRenderer {
            calls: Mutex::new(Vec::new()),
        });
        let channel = CastChannel::new(
            log,
            Arc::clone(&store) as SharedStore,
            Arc::clone(&renderer) as SharedRenderer,
            Arc::new(NoBroadcast) as SharedBroadcast,
        );
        Harness {
            store,
            renderer,
            channel,
        }
    }

    /// Install a fake connection (a slot with no TLS stream) and return its id.
    fn fake_conn(&self) -> usize {
        let mut conns = self.channel.conns.lock().unwrap();
        let id = conns.keys().copied().max().map_or(0, |m| m + 1);
        conns.insert(id, Arc::new(Mutex::new(None)));
        id
    }

    /// Drive `on_message` through the same bytes a sender would have put on
    /// the wire, and return nothing — replies are read from the outbox.
    fn deliver(&self, conn_id: usize, source_id: &str, namespace: &str, payload: Value) {
        let frame = encode_cast_message(
            source_id,
            "receiver-0",
            namespace,
            payload.to_string(),
        );
        let msg = parse_cast_message(&frame);
        self.channel.on_message(conn_id, &msg);
    }

    /// The frames still sitting in the outbox, without draining.
    fn test_frame_count(&self, conn_id: usize) -> usize {
        self.channel
            .test_outbox
            .lock()
            .unwrap()
            .get(&conn_id)
            .map(|v| v.len())
            .unwrap_or(0)
    }

    /// The replies written for one connection: `(namespace, payload)` pairs.
    fn replies(&self, conn_id: usize) -> Vec<(String, Value)> {
        self.channel
            .test_outbox
            .lock()
            .unwrap()
            .remove(&conn_id)
            .unwrap_or_default()
            .iter()
            .map(|frame| {
                // frame = 4-byte length + CastMessage; strip the header.
                let msg = parse_cast_message(&frame[FRAME_HEADER_LEN..]);
                assert_eq!(msg.payload_type, 0);
                let value: Value = serde_json::from_str(&msg.payload_utf8).unwrap();
                (msg.namespace.clone(), value)
            })
            .collect()
    }

    /// The first reply for one connection.
    fn reply(&self, conn_id: usize) -> (String, Value) {
        let mut replies = self.replies(conn_id);
        assert_eq!(replies.len(), 1, "expected exactly one reply frame");
        replies.pop().unwrap()
    }
}

// -- pure functions ------------------------------------------------------

#[test]
fn launch_refusal_names_the_app_and_gives_the_next_step() {
    crate::pin_test_dir();
    let refusal = launch_refusal(Some(&json!("0F5096E8")), Some(json!(7))).unwrap();
    assert_eq!(refusal["type"], "LAUNCH_ERROR");
    assert_eq!(refusal["requestId"], 7);
    assert_eq!(refusal["appId"], "0F5096E8");
    assert_eq!(refusal["reason"], "ERROR");
    let message = refusal["message"].as_str().unwrap();
    assert!(message.contains("0F5096E8"));
    // The human half must give a next step: which knob to turn.
    assert!(message.contains("投屏方式"));
    assert!(message.contains("低延迟"));
}

#[test]
fn launch_refusal_falls_back_to_the_default_app_for_an_unnamable_appid() {
    crate::pin_test_dir();
    // The reference maps an unnamable appId onto the default media receiver —
    // which is a served app, so the answer is "no refusal", not a refusal
    // naming an app the sender never asked for.
    for asked in [None, Some(&json!(null)), Some(&json!("")), Some(&json!(42))] {
        assert!(launch_refusal(asked, None).is_none());
    }
}

#[test]
fn launch_refusal_is_none_for_the_served_apps() {
    crate::pin_test_dir();
    for app in SERVED_APP_IDS {
        assert!(launch_refusal(Some(&json!(app)), None).is_none());
    }
    assert!(launch_refusal(None, None).is_none());
}

#[test]
fn live_from_stream_type_is_three_state() {
    crate::pin_test_dir();
    assert_eq!(live_from_stream_type(&json!({"streamType": "LIVE"})), Some(true));
    assert_eq!(live_from_stream_type(&json!({"streamType": " live "})), Some(true));
    assert_eq!(live_from_stream_type(&json!({"streamType": "BUFFERED"})), Some(false));
    // None is not false: it means "keep today's behaviour".
    assert_eq!(live_from_stream_type(&json!({})), None);
    assert_eq!(live_from_stream_type(&json!({"streamType": 7})), None);
    assert_eq!(live_from_stream_type(&json!("not an object")), None);
}

#[test]
fn sender_media_title_prefers_the_sender_and_falls_back_to_the_uri() {
    crate::pin_test_dir();
    let media = json!({"metadata": {"title": "  Never Gonna Give You Up  "}});
    assert_eq!(
        sender_media_title(&media, "http://x/v", |u| format!("name-of-{u}")),
        "Never Gonna Give You Up"
    );
    // A blank or absent title still answers — mpv's title survives a later
    // loadfile, a name never set stays wrong for the whole session.
    assert_eq!(
        sender_media_title(&json!({}), "http://x/v", |u| format!("name-of-{u}")),
        "name-of-http://x/v"
    );
    let blank = json!({"metadata": {"title": "   "}});
    assert_eq!(
        sender_media_title(&blank, "http://x/v", |u| format!("name-of-{u}")),
        "name-of-http://x/v"
    );
    // The cap: TITLE_MAX_CHARS, counted in characters, not bytes.
    let long = "长".repeat(TITLE_MAX_CHARS + 10);
    let media = json!({"metadata": {"title": long}});
    let got = sender_media_title(&media, "", |u| format!("name-of-{u}"));
    assert_eq!(got.chars().count(), TITLE_MAX_CHARS);
}

#[test]
fn random_session_is_32_lowercase_hex() {
    crate::pin_test_dir();
    for _ in 0..4 {
        let s = random_session();
        assert_eq!(s.len(), 32);
        assert!(s.chars().all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()));
    }
    assert_ne!(random_session(), random_session());
}

// -- receiver status: key order is the wire format ------------------------

#[test]
fn receiver_status_without_a_session_reports_empty_applications() {
    crate::pin_test_dir();
    let h = Harness::new();
    let conn = h.fake_conn();
    h.channel
        .send_receiver_status(conn, "sender-1", Some(json!(3)));
    let source = msg_source(&h, conn);
    let (ns, status) = h.reply(conn);
    assert_eq!(ns, NS_RECEIVER);
    assert_eq!(source, "receiver-0");
    assert_eq!(status["type"], "RECEIVER_STATUS");
    assert_eq!(status["requestId"], 3);
    // The stale-session regression's answer shape: no app, no lies.
    assert_eq!(status["status"]["applications"], json!([]));
    assert_eq!(status["status"]["isActiveInput"], true);
    assert_eq!(status["status"]["isStandBy"], false);
    assert_eq!(status["status"]["volume"]["level"], 1.0);
    assert_eq!(status["status"]["volume"]["muted"], false);
    // Key order: the reference's insertion order is the wire format.
    let keys: Vec<&String> = status.as_object().unwrap().keys().collect();
    assert_eq!(keys, ["requestId", "type", "status"]);
    let status_keys: Vec<&String> = status["status"].as_object().unwrap().keys().collect();
    assert_eq!(
        status_keys,
        ["applications", "isActiveInput", "isStandBy", "volume"]
    );
}

#[test]
fn receiver_status_after_launch_carries_one_app_in_reference_order() {
    crate::pin_test_dir();
    let h = Harness::new();
    h.channel.ledger.lock().unwrap().session_id = Some("sess0123".into());
    let conn = h.fake_conn();
    h.channel.send_receiver_status(conn, "sender-1", None);
    let (_, status) = h.reply(conn);
    let apps = status["status"]["applications"].as_array().unwrap();
    assert_eq!(apps.len(), 1);
    let app = &apps[0];
    let app_keys: Vec<&String> = app.as_object().unwrap().keys().collect();
    assert_eq!(
        app_keys,
        [
            "appId",
            "displayName",
            "sessionId",
            "statusText",
            "transportId",
            "namespaces"
        ]
    );
    assert_eq!(app["appId"], DEFAULT_MEDIA_APP_ID);
    // mkchromecast's --hijack keys off this exact name: any other string reads
    // as "someone stole my receiver" and re-issues LOAD every 5 seconds, so the
    // literal is pinned, not just "whatever the constant says".
    assert_eq!(app["displayName"], "Default Media Receiver");
    assert_eq!(app["sessionId"], "sess0123");
    assert_eq!(app["statusText"], DEFAULT_MEDIA_APP_NAME);
    assert_eq!(app["transportId"], TRANSPORT_ID);
    // Namespaces must be an array of objects — the single mistake that makes a
    // device discoverable and uncastable (§4.1).
    let namespaces = app["namespaces"].as_array().unwrap();
    assert_eq!(namespaces.len(), APP_NAMESPACES.len());
    for ns in namespaces {
        assert_eq!(ns.as_object().unwrap().len(), 1);
        assert!(ns.get("name").and_then(Value::as_str).is_some());
    }
}

/// The source id a reply was written from — the bytes carry it, so read it
/// back from the frame rather than trusting the builder.
fn msg_source(h: &Harness, conn_id: usize) -> String {
    let frames = h
        .channel
        .test_outbox
        .lock()
        .unwrap()
        .get(&conn_id)
        .cloned()
        .unwrap_or_default();
    assert!(!frames.is_empty());
    let msg = parse_cast_message(&frames[0][FRAME_HEADER_LEN..]);
    msg.source_id
}

/// The source id of the first frame, without draining — the same question
/// `msg_source` asks, for tests that still need the frames afterwards.
fn peek_source(h: &Harness, conn_id: usize) -> String {
    let frames = h
        .channel
        .test_outbox
        .lock()
        .unwrap()
        .get(&conn_id)
        .cloned()
        .unwrap_or_default();
    assert!(!frames.is_empty());
    parse_cast_message(&frames[0][FRAME_HEADER_LEN..]).source_id
}

// -- dispatch -------------------------------------------------------------

#[test]
fn connect_registers_the_sender_and_is_acked_from_receiver_zero() {
    crate::pin_test_dir();
    let h = Harness::new();
    let conn = h.fake_conn();
    h.deliver(conn, "sender-1", NS_CONNECTION, json!({"type": "CONNECT"}));
    assert!(h
        .channel
        .ledger
        .lock()
        .unwrap()
        .senders
        .contains_key("sender-1"));
    // Read the ack before draining: the source id lives in the same frame.
    let source = msg_source(&h, conn);
    let (ns, ack) = h.reply(conn);
    assert_eq!(ns, NS_CONNECTION);
    assert_eq!(ack["type"], "CONNECT");
    assert_eq!(source, RECEIVER_SOURCE);
}

#[test]
fn close_from_the_last_sender_clears_the_session() {
    crate::pin_test_dir();
    let h = Harness::new();
    let conn = h.fake_conn();
    {
        let mut ledger = h.channel.ledger.lock().unwrap();
        ledger.senders.insert("sender-1".into(), conn);
        ledger.session_id = Some("s".into());
        ledger.media = Some(json!({"contentId": "http://x"}));
        ledger.media_live = Some(true);
    }
    h.deliver(conn, "sender-1", NS_CONNECTION, json!({"type": "CLOSE"}));
    let ledger = h.channel.ledger.lock().unwrap();
    assert!(ledger.senders.is_empty());
    assert!(ledger.session_id.is_none());
    assert!(ledger.media.is_none());
    assert_eq!(ledger.media_live, None);
    // A teardown bumps the generation, so a stale watcher stops itself.
    assert!(ledger.watch_generation > 0);
}

#[test]
fn close_from_one_of_two_senders_keeps_the_session() {
    crate::pin_test_dir();
    let h = Harness::new();
    let conn_a = h.fake_conn();
    let conn_b = h.fake_conn();
    {
        let mut ledger = h.channel.ledger.lock().unwrap();
        ledger.senders.insert("a".into(), conn_a);
        ledger.senders.insert("b".into(), conn_b);
        ledger.session_id = Some("s".into());
    }
    h.deliver(conn_a, "a", NS_CONNECTION, json!({"type": "CLOSE"}));
    let ledger = h.channel.ledger.lock().unwrap();
    assert!(ledger.senders.contains_key("b"));
    assert!(ledger.session_id.is_some());
}

#[test]
fn ping_is_answered_with_pong() {
    crate::pin_test_dir();
    let h = Harness::new();
    let conn = h.fake_conn();
    h.deliver(conn, "sender-1", NS_HEARTBEAT, json!({"type": "PING"}));
    let (ns, pong) = h.reply(conn);
    assert_eq!(ns, NS_HEARTBEAT);
    assert_eq!(pong["type"], "PONG");
}

#[test]
fn dispatch_is_by_namespace_and_never_consults_destination_id() {
    crate::pin_test_dir();
    let h = Harness::new();
    let conn = h.fake_conn();
    // A real sender CONNECTs to our transportId but replies to whatever
    // source_id we used; some senders address the destination they invented.
    // Routing must not filter on it.
    for dest in [TRANSPORT_ID, "receiver-0", "someone-elses-transport", ""] {
        let frame = encode_cast_message(
            "sender-1",
            dest,
            NS_HEARTBEAT,
            json!({"type": "PING"}).to_string(),
        );
        let msg = parse_cast_message(&frame);
        h.channel.on_message(conn, &msg);
        let (ns, pong) = h.reply(conn);
        assert_eq!(ns, NS_HEARTBEAT, "destination {dest:?} must not be filtered");
        assert_eq!(pong["type"], "PONG");
    }
}

#[test]
fn deviceauth_challenge_gets_the_binary_auth_response() {
    crate::pin_test_dir();
    let h = Harness::new();
    let conn = h.fake_conn();
    // The challenge is a binary payload; build it the way proto.rs does.
    let challenge = crate::proto::encode_auth_challenge();
    let frame = encode_cast_message(
        "sender-1",
        RECEIVER_SOURCE,
        NS_DEVICEAUTH,
        crate::proto::Payload::Binary(challenge),
    );
    let msg = parse_cast_message(&frame);
    h.channel.on_message(conn, &msg);
    let frames = h
        .channel
        .test_outbox
        .lock()
        .unwrap()
        .remove(&conn)
        .unwrap();
    assert_eq!(frames.len(), 1);
    let reply = parse_cast_message(&frames[0][FRAME_HEADER_LEN..]);
    assert_eq!(reply.namespace, NS_DEVICEAUTH);
    assert_eq!(reply.payload_type, 1, "the auth response is binary");
    assert_eq!(reply.payload_binary, crate::proto::encode_auth_response());
}

#[test]
fn unhandled_namespaces_and_types_are_recorded_not_answered() {
    crate::pin_test_dir();
    let h = Harness::new();
    let conn = h.fake_conn();
    h.deliver(
        conn,
        "sender-1",
        "urn:x-cast:com.google.cast.something.else",
        json!({"type": "WHATEVER"}),
    );
    h.deliver(conn, "sender-1", NS_MEDIA, json!({"type": "QUEUE_UPDATE"}));
    h.deliver(conn, "sender-1", NS_RECEIVER, json!({"type": "SET_ACTIVE_INPUT_STATE"}));
    assert!(h.replies(conn).is_empty());
}

// -- receiver namespace ---------------------------------------------------

#[test]
fn launch_of_a_served_app_opens_a_session_and_reports_it() {
    crate::pin_test_dir();
    let h = Harness::new();
    let conn = h.fake_conn();
    h.deliver(
        conn,
        "sender-1",
        NS_RECEIVER,
        json!({"type": "LAUNCH", "appId": DEFAULT_MEDIA_APP_ID, "requestId": 11}),
    );
    let (_, status) = h.reply(conn);
    assert_eq!(status["type"], "RECEIVER_STATUS");
    assert_eq!(status["requestId"], 11);
    let apps = status["status"]["applications"].as_array().unwrap();
    assert_eq!(apps.len(), 1);
    assert!(h.channel.ledger.lock().unwrap().session_id.is_some());
}

#[test]
fn launch_of_the_mirror_app_is_refused_on_the_same_connection() {
    crate::pin_test_dir();
    let h = Harness::new();
    let conn = h.fake_conn();
    h.deliver(
        conn,
        "sender-1",
        NS_RECEIVER,
        json!({"type": "LAUNCH", "appId": "0F5096E8", "requestId": 5}),
    );
    let (ns, refusal) = h.reply(conn);
    assert_eq!(ns, NS_RECEIVER);
    assert_eq!(refusal["type"], "LAUNCH_ERROR");
    assert_eq!(refusal["requestId"], 5);
    assert_eq!(refusal["appId"], "0F5096E8");
    assert!(refusal["message"].as_str().unwrap().contains("投屏方式"));
    // No session, no application row: a refusal is not a launch.
    assert!(h.channel.ledger.lock().unwrap().session_id.is_none());
}

#[test]
fn get_app_availability_is_derived_from_the_served_table() {
    crate::pin_test_dir();
    let h = Harness::new();
    let conn = h.fake_conn();
    h.deliver(
        conn,
        "sender-1",
        NS_RECEIVER,
        json!({"type": "GET_APP_AVAILABILITY", "requestId": 9}),
    );
    let (_, answer) = h.reply(conn);
    assert_eq!(answer["type"], "GET_APP_AVAILABILITY");
    assert_eq!(answer["requestId"], 9);
    let availability = answer["availability"].as_object().unwrap();
    assert_eq!(availability.len(), SERVED_APP_IDS.len());
    for app in SERVED_APP_IDS {
        assert_eq!(availability[app], "APP_AVAILABLE");
    }
    // The mirroring app is deliberately not advertised as available.
    assert!(!availability.contains_key("0F5096E8"));
}

#[test]
fn stop_and_quit_app_clear_the_ledger_and_stop_the_player() {
    crate::pin_test_dir();
    for stop_type in ["STOP", "QUIT_APP"] {
        let h = Harness::new();
        h.channel.ledger.lock().unwrap().session_id = Some("s".into());
        h.channel.ledger.lock().unwrap().media = Some(json!({"contentId": "http://x"}));
        let conn = h.fake_conn();
        h.deliver(conn, "sender-1", NS_RECEIVER, json!({"type": stop_type, "requestId": 2}));
        let (_, status) = h.reply(conn);
        assert_eq!(status["type"], "RECEIVER_STATUS");
        assert_eq!(status["status"]["applications"], json!([]));
        assert!(h.renderer.has("set_media_stop"));
        let ledger = h.channel.ledger.lock().unwrap();
        assert!(ledger.session_id.is_none());
        assert!(ledger.media.is_none());
        assert_eq!(ledger.media_live, None);
    }
}

#[test]
fn set_volume_updates_real_state_answers_the_request_and_reaches_the_player() {
    crate::pin_test_dir();
    let h = Harness::new();
    let conn = h.fake_conn();
    h.deliver(
        conn,
        "sender-1",
        NS_RECEIVER,
        json!({"type": "SET_VOLUME", "requestId": 4, "volume": {"level": 0.42, "muted": true}}),
    );
    let (_, status) = h.reply(conn);
    // The answer carries the new values — senders compute their next step from
    // what we report, so a constant 1.0 would freeze their slider.
    assert_eq!(status["status"]["volume"]["level"], 0.42);
    assert_eq!(status["status"]["volume"]["muted"], true);
    assert!(h.renderer.has("set_media_volume=42"));
    assert!(h.renderer.has("set_media_mute=True"));
    // Clamped into range.
    h.deliver(
        conn,
        "sender-1",
        NS_RECEIVER,
        json!({"type": "SET_VOLUME", "volume": {"level": 5.0}}),
    );
    assert_eq!(h.channel.ledger.lock().unwrap().volume, 1.0);
    // Malformed levels are ignored, not fatal.
    h.deliver(
        conn,
        "sender-1",
        NS_RECEIVER,
        json!({"type": "SET_VOLUME", "volume": {"level": "loud"}}),
    );
    assert_eq!(h.channel.ledger.lock().unwrap().volume, 1.0);
}

// -- media namespace ------------------------------------------------------

#[test]
fn load_without_a_url_answers_load_failed() {
    crate::pin_test_dir();
    let h = Harness::new();
    let conn = h.fake_conn();
    h.deliver(
        conn,
        "sender-1",
        NS_MEDIA,
        json!({"type": "LOAD", "requestId": 1, "media": {}}),
    );
    let (ns, failure) = h.reply(conn);
    assert_eq!(ns, NS_MEDIA);
    assert_eq!(failure["type"], "LOAD_FAILED");
    assert_eq!(failure["requestId"], 1);
    assert!(h.renderer.calls.lock().unwrap().is_empty());
}

#[test]
fn load_hands_the_url_title_and_start_to_the_shared_renderer() {
    crate::pin_test_dir();
    let h = Harness::new();
    let conn = h.fake_conn();
    h.deliver(
        conn,
        "sender-1",
        NS_MEDIA,
        json!({
            "type": "LOAD",
            "requestId": 1,
            "currentTime": 12.7,
            "media": {
                "contentId": "http://example.com/v.mp4",
                "streamType": "LIVE",
                "metadata": {"title": "Big Buck Bunny"},
            },
        }),
    );
    // BUFFERING first — PLAYING comes from the watcher, not blindly.
    let (ns, status) = h.reply(conn);
    assert_eq!(ns, NS_MEDIA);
    assert_eq!(status["type"], "MEDIA_STATUS");
    assert_eq!(status["status"][0]["playerState"], "BUFFERING");
    // Two doors into one living room: the calls land on the shared Renderer.
    assert!(h.renderer.has("note_media_live=Some(true)"));
    assert!(h
        .renderer
        .has("set_media_url=http://example.com/v.mp4 start=Some(\"12\")"));
    assert!(h.renderer.has("set_media_title=Big Buck Bunny"));
    let ledger = h.channel.ledger.lock().unwrap();
    assert!(ledger.session_id.is_some());
    assert_eq!(ledger.media_live, Some(true));
    assert_eq!(ledger.position, 12.7);
}

#[test]
fn load_without_a_title_asks_title_for_uri() {
    crate::pin_test_dir();
    let h = Harness::new();
    let conn = h.fake_conn();
    h.deliver(
        conn,
        "sender-1",
        NS_MEDIA,
        json!({
            "type": "LOAD",
            "media": {"contentId": "http://example.com/v.mp4"},
        }),
    );
    assert!(h
        .renderer
        .has("set_media_title=name-of-http://example.com/v.mp4"));
}

#[test]
fn play_pause_and_seek_reach_the_player_and_answer_media_status() {
    crate::pin_test_dir();
    let h = Harness::new();
    let conn = h.fake_conn();
    h.deliver(conn, "sender-1", NS_MEDIA, json!({"type": "PAUSE"}));
    assert!(h.renderer.has("set_media_pause"));
    assert_eq!(h.reply(conn).1["status"][0]["playerState"], "PAUSED");
    h.deliver(conn, "sender-1", NS_MEDIA, json!({"type": "PLAY"}));
    assert!(h.renderer.has("set_media_resume"));
    assert_eq!(h.reply(conn).1["status"][0]["playerState"], "PLAYING");
    h.deliver(
        conn,
        "sender-1",
        NS_MEDIA,
        json!({"type": "SEEK", "currentTime": 30.5}),
    );
    assert!(h.renderer.has("set_media_position=30.5"));
    assert_eq!(h.reply(conn).1["status"][0]["playerState"], "PLAYING");
}

#[test]
fn media_stop_reports_idle_cancelled_after_clearing_the_session() {
    crate::pin_test_dir();
    let h = Harness::new();
    h.channel.ledger.lock().unwrap().media = Some(json!({"contentId": "http://x"}));
    let conn = h.fake_conn();
    h.deliver(conn, "sender-1", NS_MEDIA, json!({"type": "STOP", "requestId": 3}));
    let (_, status) = h.reply(conn);
    assert_eq!(status["status"][0]["playerState"], "IDLE");
    assert_eq!(status["status"][0]["idleReason"], "CANCELLED");
    assert!(h.renderer.has("set_media_stop"));
}

#[test]
fn media_get_status_answers_what_is_really_happening() {
    crate::pin_test_dir();
    let h = Harness::new();
    let conn = h.fake_conn();
    // Nothing cast: IDLE, no idleReason (pychromecast clears the key when it
    // is absent, so only terminal states carry it).
    h.deliver(conn, "sender-1", NS_MEDIA, json!({"type": "GET_STATUS"}));
    let (_, status) = h.reply(conn);
    assert_eq!(status["status"][0]["playerState"], "IDLE");
    assert!(status["status"][0].get("idleReason").is_none());
}

#[test]
fn media_status_entry_key_order_and_media_echo() {
    crate::pin_test_dir();
    let h = Harness::new();
    {
        let mut ledger = h.channel.ledger.lock().unwrap();
        ledger.session_id = Some("sess".into());
        ledger.media = Some(json!({"contentId": "http://x/v"}));
        ledger.observed_transport = Some("PLAYING".into());
    }
    let conn = h.fake_conn();
    h.channel
        .send_media_status(conn, "sender-1", None, Some(json!(8)));
    let (ns, status) = h.reply(conn);
    assert_eq!(ns, NS_MEDIA);
    let entry = &status["status"][0];
    let entry_keys: Vec<&String> = entry.as_object().unwrap().keys().collect();
    assert_eq!(
        entry_keys,
        [
            "mediaSessionId",
            "playbackRate",
            "playerState",
            "currentTime",
            "supportedMediaCommands",
            "volume",
            "sessionId",
            "media"
        ]
    );
    assert_eq!(entry["playerState"], "PLAYING");
    assert_eq!(entry["supportedMediaCommands"], 15);
    assert_eq!(entry["media"]["contentId"], "http://x/v");
    assert_eq!(entry["sessionId"], "sess");
    let keys: Vec<&String> = status.as_object().unwrap().keys().collect();
    assert_eq!(keys, ["requestId", "type", "status"]);
}

// -- observation window ---------------------------------------------------

#[test]
fn observed_state_follows_the_transport_and_the_terminal_reasons() {
    crate::pin_test_dir();
    let h = Harness::new();
    // No media at all.
    h.channel
        .send_media_status(h.fake_conn(), "s", None, None);
    // (replies consumed above; work on the ledger directly for the rest)
    {
        let mut ledger = h.channel.ledger.lock().unwrap();
        ledger.media = Some(json!({}));
        // Never started playing: the optimistic default.
        assert_eq!(ledger.observed_state().0, "PLAYING");
        ledger.observed_transport = Some("PAUSED_PLAYBACK".into());
        assert_eq!(ledger.observed_state().0, "PAUSED_PLAYBACK");
    }
    h.channel.set_state_eof();
    {
        let ledger = h.channel.ledger.lock().unwrap();
        let (state, reason) = ledger.observed_state();
        assert_eq!(state, "IDLE");
        assert_eq!(reason.as_deref(), Some("FINISHED"));
    }
}

#[test]
fn a_stop_before_playback_does_not_overwrite_a_real_reason() {
    crate::pin_test_dir();
    let h = Harness::new();
    {
        let mut ledger = h.channel.ledger.lock().unwrap();
        ledger.media = Some(json!({}));
        // The media did start playing — that is what makes the later `idle`
        // a real end rather than a superseded LOAD.
        ledger.observed_transport = Some("PLAYING".into());
    }
    // mpv fires `idle` right after a normal end-of-file: CANCELLED must not
    // clobber FINISHED.
    h.channel.set_state_eof();
    h.channel.set_state_stop();
    let ledger = h.channel.ledger.lock().unwrap();
    assert_eq!(ledger.idle_reason.as_deref(), Some("FINISHED"));
}

#[test]
fn superseded_load_ignores_a_stale_stop() {
    crate::pin_test_dir();
    let h = Harness::new();
    // A media that never started playing (observed_transport is None): a stop
    // for it is a superseded LOAD, not a real end.
    h.channel.ledger.lock().unwrap().media = Some(json!({}));
    h.channel.set_state_stop();
    let ledger = h.channel.ledger.lock().unwrap();
    assert!(ledger.idle_reason.is_none());
}

#[test]
fn release_playback_clears_only_the_live_opinion() {
    crate::pin_test_dir();
    let h = Harness::new();
    {
        let mut ledger = h.channel.ledger.lock().unwrap();
        ledger.media = Some(json!({"contentId": "http://x"}));
        ledger.media_live = Some(true);
    }
    h.channel.release_playback();
    let ledger = h.channel.ledger.lock().unwrap();
    assert_eq!(ledger.media_live, None);
    assert!(ledger.media.is_some());
}

#[test]
fn media_is_live_reads_the_stored_answer_not_a_rederivation() {
    crate::pin_test_dir();
    let h = Harness::new();
    assert_eq!(h.channel.media_is_live(), None);
    h.channel.ledger.lock().unwrap().media_live = Some(false);
    assert_eq!(h.channel.media_is_live(), Some(false));
}

// -- the watcher ----------------------------------------------------------

#[test]
fn the_watcher_reports_playing_only_after_the_table_confirms() {
    crate::pin_test_dir();
    let h = Harness::new();
    let conn = h.fake_conn();
    let media = json!({"contentId": "http://x/v"});
    let mut data = json!({"type": "LOAD", "requestId": 6});
    data["media"] = media;
    h.deliver(conn, "sender-1", NS_MEDIA, data);
    // The table says nothing yet: the watcher is inside its grace window and
    // has not answered. Peek, do not drain — the frames must survive for the
    // wait below.
    std::thread::sleep(Duration::from_millis(100));
    assert!(
        h.test_frame_count(conn) <= 1,
        "BUFFERING only, no final answer yet"
    );
    // The table confirms: poll the transport until the watcher's reply lands.
    *h.store.transport_state.lock().unwrap() = "PLAYING".into();
    let deadline = Instant::now() + Duration::from_secs(LOAD_GRACE_SECONDS);
    loop {
        let frames = h
            .channel
            .test_outbox
            .lock()
            .unwrap()
            .get(&conn)
            .cloned()
            .unwrap_or_default();
        if frames.len() >= 2 {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "the watcher never confirmed playback"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
    let (_, status) = h.reply2(conn);
    assert_eq!(status["status"][0]["playerState"], "PLAYING");
}

#[test]
fn an_error_transport_becomes_load_failed() {
    crate::pin_test_dir();
    let h = Harness::new();
    let conn = h.fake_conn();
    let mut data = json!({"type": "LOAD", "requestId": 6, "media": {"contentId": "http://x"}});
    data["media"] = json!({"contentId": "http://x"});
    h.deliver(conn, "sender-1", NS_MEDIA, data);
    *h.store.transport_status.lock().unwrap() = "ERROR_OCCURRED".into();
    let deadline = Instant::now() + Duration::from_secs(LOAD_GRACE_SECONDS);
    loop {
        let frames = h
            .channel
            .test_outbox
            .lock()
            .unwrap()
            .get(&conn)
            .cloned()
            .unwrap_or_default();
        if frames.len() >= 2 {
            break;
        }
        assert!(Instant::now() < deadline, "the watcher never answered");
        std::thread::sleep(Duration::from_millis(20));
    }
    let (ns, status) = h.reply2(conn);
    assert_eq!(ns, NS_MEDIA);
    assert_eq!(status["type"], "LOAD_FAILED");
}

#[test]
fn a_newer_load_supersedes_the_previous_watcher() {
    crate::pin_test_dir();
    let h = Harness::new();
    let conn = h.fake_conn();
    h.deliver(
        conn,
        "sender-1",
        NS_MEDIA,
        json!({"type": "LOAD", "requestId": 1, "media": {"contentId": "http://a"}}),
    );
    // A second LOAD bumps the generation; the first watcher must exit without
    // answering, even though the table never confirms.
    h.deliver(
        conn,
        "sender-1",
        NS_MEDIA,
        json!({"type": "LOAD", "requestId": 2, "media": {"contentId": "http://b"}}),
    );
    // Only the two BUFFERING replies may ever appear.
    std::thread::sleep(Duration::from_millis(300));
    let frames = h
        .channel
        .test_outbox
        .lock()
        .unwrap()
        .get(&conn)
        .cloned()
        .unwrap_or_default();
    assert_eq!(frames.len(), 2, "a superseded watcher must stay silent");
}

impl Harness {
    /// Like [`Harness::reply`], but for the watcher's frame: the outbox may
    /// hold BUFFERING + one final answer; drop the first.
    fn reply2(&self, conn_id: usize) -> (String, Value) {
        let frames = self
            .channel
            .test_outbox
            .lock()
            .unwrap()
            .remove(&conn_id)
            .unwrap_or_default();
        assert!(frames.len() >= 2);
        let msg = parse_cast_message(&frames[1][FRAME_HEADER_LEN..]);
        let value: Value = serde_json::from_str(&msg.payload_utf8).unwrap();
        (msg.namespace, value)
    }
}

// -- the wire, end to end over a real socket ------------------------------

#[test]
fn end_to_end_over_a_real_tls_socket() {
    crate::pin_test_dir();
    let dir = std::env::temp_dir().join(format!("macast-cast-e2e-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let log = Logger::named("Chromecast");
    let pair = crate::cert::ensure_pair(&dir, &log).expect("generate a test pair");
    let config = pair.server_config().expect("server config");

    let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let addr = listener.local_addr().unwrap();
    let server = std::thread::spawn(move || {
        let (stream, _addr) = listener.accept().unwrap();
        stream.set_read_timeout(Some(HANDSHAKE_TIMEOUT)).unwrap();
        let conn = rustls::ServerConnection::new(config).unwrap();
        let mut tls = rustls::StreamOwned::new(conn, stream);
        // One framed exchange: CONNECT … CONNECT ack.
        let mut header = [0u8; FRAME_HEADER_LEN];
        tls.read_exact(&mut header).unwrap();
        let len = proto::frame_length(&header);
        let mut body = vec![0u8; len];
        tls.read_exact(&mut body).unwrap();
        let msg = parse_cast_message(&body);
        assert_eq!(msg.namespace, NS_CONNECTION);
        let payload: Value = serde_json::from_str(&msg.payload_utf8).unwrap();
        assert_eq!(payload["type"], "CONNECT");
        // Answer the way the channel does: a frame back on the same session.
        let reply = wrap_frame(&encode_cast_message(
            RECEIVER_SOURCE,
            &msg.source_id,
            NS_CONNECTION,
            json!({"type": "CONNECT"}).to_string(),
        ));
        tls.write_all(&reply).unwrap();
        tls.conn.send_close_notify();
    });

    let client = std::thread::spawn(move || {
        let stream = TcpStream::connect(addr).unwrap();
        // The self-signed pair is its own trust anchor for this exchange.
        let mut roots = rustls::RootCertStore::empty();
        let cert = crate::cert::read_certs(&pair.cert).unwrap();
        for c in &cert {
            roots.add(c.clone()).unwrap();
        }
        let client_config = rustls::ClientConfig::builder()
            .with_root_certificates(roots)
            .with_no_client_auth();
        let server_name = rustls::pki_types::ServerName::try_from("localhost".to_string())
            .expect("server name");
        let conn = rustls::ClientConnection::new(Arc::new(client_config), server_name).unwrap();
        let mut tls = rustls::StreamOwned::new(conn, stream);
        let hello = wrap_frame(&encode_cast_message(
            "sender-1",
            RECEIVER_SOURCE,
            NS_CONNECTION,
            json!({"type": "CONNECT"}).to_string(),
        ));
        tls.write_all(&hello).unwrap();
        let mut header = [0u8; FRAME_HEADER_LEN];
        tls.read_exact(&mut header).unwrap();
        let len = proto::frame_length(&header);
        let mut body = vec![0u8; len];
        tls.read_exact(&mut body).unwrap();
        let msg = parse_cast_message(&body);
        assert_eq!(msg.source_id, RECEIVER_SOURCE);
        let payload: Value = serde_json::from_str(&msg.payload_utf8).unwrap();
        assert_eq!(payload["type"], "CONNECT");
    });

    server.join().unwrap();
    client.join().unwrap();
    let _ = std::fs::remove_dir_all(&dir);
}
