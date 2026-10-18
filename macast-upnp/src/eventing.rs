//! The GENA face: subscription table, init events, and the NOTIFY sender.
//!
//! Python counterparts live in `macast/protocol.py`: an [`ObserveClient`] there
//! is one subscription here ([`Subscriber`]), `add_subscribe` /
//! `renew_subscribe` / `remove_subscribe` answer the SUBSCRIBE verbs, and the
//! `event()` thread folds the state queue into NOTIFY requests every 250 ms.
//!
//! Who decides what: [`crate::http`] decides *when* a verb is answered; this
//! module owns *whether the SID exists*. Two asymmetries are kept verbatim
//! from Python: an unknown SID is renewed with 412 but removed with 200, and
//! a subscription becomes visible only after the append queue is synced into
//! the published table — so a removal queued before the first sync cannot
//! land. [`Broadcast::enqueue`] gates nothing here: only [`crate::soap`]
//! already filtered the enqueue by the observed-variable table, and a second
//! gate would be a second copy of the same rule that would drift.
//!
//! The wire: one TCP connection per NOTIFY, written and closed without
//! reading a reply. The header order mirrors Python's builder with one
//! deliberate semantically inert divergence — `Content-Length` directly after
//! `Host`. `SEQ` starts 0 with the init event and advances only after a
//! successful send. A non-ConnectionManager body is one `LastChange` holding
//! a text-escaped detached `<Event>` — attribute entities are therefore
//! escaped twice — and RenderingControl rides in the AVT namespace (upstream
//! quirk, kept). A ConnectionManager subscriber gets its init event but never
//! change announcements, because no observed variable names a CM variable.
//! Init events are bounded at twenty concurrent; a failed init only logs,
//! while a failed broadcast counts, and the client is dropped after more
//! than ten accumulated failures.

use std::collections::hash_map::RandomState;
use std::collections::{BTreeMap, VecDeque};
use std::hash::{BuildHasher, Hasher};
use std::io::{self, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use macast_core::logging::Logger;
use serde_json::Value;

use crate::http::Subscription;
use crate::store::{observed_for, wire, Broadcast, SharedStore, StateStore};

const INIT_PERMITS: usize = 20;
const TICK: Duration = Duration::from_millis(250);
const MAX_CLIENT_ERRORS: u32 = 10;
const DELIVER_TIMEOUT: Duration = Duration::from_secs(5);
const EVENT_NS: &str = "urn:schemas-upnp-org:event-1-0";
const AVT_NS: &str = "urn:schemas-upnp-org:metadata-1-0/AVT/";
const XML_DECL: &str = "<?xml version='1.0' encoding='UTF-8'?>\n";

struct Semaphore {
    count: Mutex<usize>,
    cond: Condvar,
}

impl Semaphore {
    fn new(permits: usize) -> Arc<Self> {
        Arc::new(Semaphore {
            count: Mutex::new(permits),
            cond: Condvar::new(),
        })
    }

    fn acquire(&self) {
        let mut count = self.count.lock().unwrap();
        while *count == 0 {
            count = self.cond.wait(count).unwrap();
        }
        *count -= 1;
    }

    fn release(&self) {
        *self.count.lock().unwrap() += 1;
        self.cond.notify_one();
    }
}

struct Lease {
    seq: u64,
    timeout: i64,
    start: i64,
    error: u32,
}

struct Subscriber {
    sid: String,
    service: String,
    url: String,
    host: String,
    path: String,
    lease: Mutex<Lease>,
}

impl Subscriber {
    fn new(service: &str, url: &str, timeout: i64) -> Self {
        let (host, path) = split_callback(url);
        Subscriber {
            sid: format!("uuid:{}", uuid_v4()),
            service: service.to_string(),
            url: url.to_string(),
            host,
            path,
            lease: Mutex::new(Lease {
                seq: 0,
                timeout,
                start: now_secs(),
                error: 0,
            }),
        }
    }

    fn is_timed_out(&self, now: i64) -> bool {
        let lease = self.lease.lock().unwrap();
        now > lease.start + lease.timeout
    }

    fn update(&self, timeout: i64) {
        let mut lease = self.lease.lock().unwrap();
        lease.timeout = timeout;
        lease.start = now_secs();
    }

    fn timeout(&self) -> i64 {
        self.lease.lock().unwrap().timeout
    }

    fn seq(&self) -> u64 {
        self.lease.lock().unwrap().seq
    }

    fn advance_seq(&self) {
        self.lease.lock().unwrap().seq += 1;
    }

    fn note_error(&self) -> u32 {
        let mut lease = self.lease.lock().unwrap();
        lease.error += 1;
        lease.error
    }
}

/// The GENA state machine's counters, as one snapshot —
/// the read behind [`crate::http::Subscription::diagnostics`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Diagnostics {
    /// We asked the event thread to run (`start()` set it, `stop()` clears it).
    pub running: bool,
    /// The thread is still running itself. Read **with** `running`, never instead.
    pub event_thread_alive: bool,
    /// Accepted subscriptions not yet in the published table. Non-zero after a
    /// few seconds means the appender is stuck or gone.
    pub pending_append: usize,
    /// State changes waiting for the next broadcast tick.
    pub pending_state: usize,
    /// Removals queued and not yet published. **Not in the reference** — a stuck
    /// reaper has the same shape as a stuck appender and would otherwise read as
    /// "subscribers that never go away".
    pub pending_remove: usize,
}

/// One row of the read-only subscriber view [`Eventing::subscribers`] returns.
///
/// These are facts, not a page shape: the JSON the settings page reads is owned by
/// whoever serves it — `src-tauri/src/main.rs` for the reference's
/// `query=subscribers` keys, `crate::http`'s `query=status` for the six keys
/// `get_status` writes (`host, url, service, sid, path, timeout`). A row is
/// therefore read by two documents and shaped by neither. Every lease-backed field
/// of one row comes out of **one** lock acquisition, so a row is a point-in-time
/// snapshot: it can be a moment behind the event thread, never a mix of two moments.
#[derive(Clone, Debug)]
pub struct SubscriberRow {
    pub sid: String,
    pub service: String,
    pub url: String,
    /// The callback's host part, read the way `ObserveClient.__init__` reads it
    /// (`protocol.py:452-453`): the run of digits, colons and dots right after the
    /// `//`, which is empty for anything else — a hostname with letters contributes
    /// `''` there and *everything* to `path`. Both are here because the status
    /// document's `clients` rows carry them, and the page's 客户端信息 card answers
    /// "who is watching" from `host`: recomputing the split a second time in the
    /// HTTP face would be a second owner of a rule that has one measured reading
    /// (`split_callback`, including the `[::1]` case where Python's regex matches
    /// the empty string and the brackets fall into `path`).
    pub host: String,
    /// The callback's path part — the rest of the URL after [`SubscriberRow::host`].
    pub path: String,
    /// The timeout this subscription was last granted, in seconds.
    pub timeout: i64,
    /// `NTSEQ` of the next event this subscriber would receive.
    pub seq: u64,
    /// Broadcast failures accumulated since the subscription began; the sender
    /// drops a client once this passes `MAX_CLIENT_ERRORS`.
    pub errors: u32,
    /// Its lease has run out but the event thread has not reaped it yet.
    pub timed_out: bool,
}

/// The GENA state machine: who is subscribed, what changed, and the thread
/// that turns one into the other.
pub struct Eventing {
    log: Arc<Logger>,
    store: SharedStore,
    table: Mutex<Arc<BTreeMap<String, Arc<Subscriber>>>>,
    append_queue: Mutex<Vec<Arc<Subscriber>>>,
    remove_queue: Mutex<Vec<String>>,
    state_queue: Mutex<VecDeque<(String, Value)>>,
    state_event: Condvar,
    init_sem: Arc<Semaphore>,
    running: AtomicBool,
    /// The event thread itself, kept so [`Eventing::event_thread_alive`] can ask
    /// it rather than read our own flag. `running` is a *request* to stop; a
    /// thread that panicked on the way through `loop_once` leaves it set, which
    /// is precisely the state the page exists to be able to see. The reference
    /// asks the real thing (`protocol.py` reports `thread.is_alive()`).
    event_thread: Mutex<Option<std::thread::JoinHandle<()>>>,
}

impl Eventing {
    pub fn new(store: SharedStore) -> Arc<Self> {
        Arc::new(Eventing {
            log: Logger::named("Protocol"),
            store,
            table: Mutex::new(Arc::new(BTreeMap::new())),
            append_queue: Mutex::new(Vec::new()),
            remove_queue: Mutex::new(Vec::new()),
            state_queue: Mutex::new(VecDeque::new()),
            state_event: Condvar::new(),
            init_sem: Semaphore::new(INIT_PERMITS),
            running: AtomicBool::new(false),
            event_thread: Mutex::new(None),
        })
    }

    fn published(&self) -> Arc<BTreeMap<String, Arc<Subscriber>>> {
        self.table.lock().unwrap().clone()
    }

    /// Single writer: drain both queues, log what moved, publish one new
    /// snapshot. Both queues are taken together so a removal and an append
    /// queued in the same instant resolve in the removal-first order.
    fn sync_subscribe_list(&self) {
        let removals = std::mem::take(&mut *self.remove_queue.lock().unwrap());
        let appends = std::mem::take(&mut *self.append_queue.lock().unwrap());
        if removals.is_empty() && appends.is_empty() {
            return;
        }
        for sid in &removals {
            self.log.error(&format!("Remove client: {sid}"));
        }
        for client in &appends {
            self.log
                .error(&format!("Add client: {} ({})", client.sid, client.url));
        }
        let mut next = self.published().as_ref().clone();
        for sid in &removals {
            next.remove(sid);
        }
        for client in appends {
            next.insert(client.sid.clone(), client);
        }
        *self.table.lock().unwrap() = Arc::new(next);
    }

    fn remove_subscribe(&self, sid: &str) -> u16 {
        if self.published().contains_key(sid) {
            self.remove_queue.lock().unwrap().push(sid.to_string());
        }
        200
    }

    fn reap_timed_out(&self) {
        let now = now_secs();
        for (sid, client) in self.published().iter() {
            if client.is_timed_out(now) {
                self.remove_queue.lock().unwrap().push(sid.clone());
            }
        }
    }

    fn send_states_to_clients(&self, changes: &[(String, Value)]) {
        if changes.is_empty() {
            return;
        }
        self.sync_subscribe_list();
        for (sid, client) in self.published().iter() {
            if client.is_timed_out(now_secs()) {
                self.remove_queue.lock().unwrap().push(sid.clone());
                continue;
            }
            let relevant: Vec<(String, Value)> = changes
                .iter()
                .filter(|(name, _)| {
                    observed_for(&client.service)
                        .iter()
                        .any(|observed| *observed == name.as_str())
                })
                .cloned()
                .collect();
            if relevant.is_empty() {
                continue;
            }
            let body = propertyset_body(&client.service, &relevant);
            let seq = client.seq();
            match deliver(client, seq, &body) {
                Ok(()) => client.advance_seq(),
                Err(e) => {
                    self.log.error(&format!("send event error: {e}"));
                    if client.note_error() > MAX_CLIENT_ERRORS {
                        self.remove_subscribe(sid);
                    }
                }
            }
        }
        self.sync_subscribe_list();
    }

    fn tick(&self) {
        self.reap_timed_out();
        self.sync_subscribe_list();
        let pending = std::mem::take(&mut *self.state_queue.lock().unwrap());
        if pending.is_empty() {
            return;
        }
        let mut folded: Vec<(String, Value)> = Vec::new();
        for (name, value) in pending {
            if let Some(slot) = folded.iter_mut().find(|slot| slot.0 == name) {
                slot.1 = value;
            } else {
                folded.push((name, value));
            }
        }
        self.send_states_to_clients(&folded);
    }

    fn loop_once(&self) {
        {
            let queue = self.state_queue.lock().unwrap();
            if queue.is_empty() {
                let _ = self
                    .state_event
                    .wait_timeout(queue, TICK)
                    .unwrap();
            }
        }
        self.tick();
    }

    fn loop_forever(&self) {
        while self.running.load(Ordering::SeqCst) {
            self.loop_once();
        }
    }

    pub fn start(self: &Arc<Self>) {
        if self.running.swap(true, Ordering::SeqCst) {
            return;
        }
        let eventing = Arc::clone(self);
        let spawned = std::thread::Builder::new()
            .name("gena-event".to_string())
            .spawn(move || eventing.loop_forever());
        match spawned {
            Ok(handle) => {
                *self.event_thread.lock().unwrap() = Some(handle);
            }
            Err(e) => {
                self.running.store(false, Ordering::SeqCst);
                self.log.error(&format!("event thread did not start: {e}"));
            }
        }
    }

    pub fn stop(&self) {
        self.running.store(false, Ordering::SeqCst);
        drop(self.state_queue.lock().unwrap());
        self.state_event.notify_all();
    }

    /// The published subscriber table, as rows.
    ///
    /// It binds the snapshot **once** (`published()`) and then reads from that
    /// `Arc`, which is the contract the reference's Part 49 exists for: a reader
    /// that re-reads the live table while the event thread republishes is a
    /// reader that iterates a structure changing under it. Nothing here takes the
    /// table lock while holding a subscriber lease, so no order of these two
    /// locks can deadlock against the event thread.
    ///
    /// Each row takes the subscriber's lease **once**. `std::sync::Mutex` is not
    /// reentrant, so a second `lease.lock()` inside the same row — whichever
    /// accessor it goes through — is this reader deadlocking against a guard it
    /// already holds, and it deadlocks only once the table has a subscriber in
    /// it, which is exactly the state a unit test over an empty table never
    /// reaches. This is not a background reader: a synchronous Tauri command
    /// runs on the main thread, so the page's 5-second poll would freeze the
    /// window (and its accessibility tree) while the HTTP services keep
    /// answering — a live port is not evidence the UI thread is alive.
    ///
    /// Queued-but-unpublished subscriptions are deliberately **not** in these
    /// rows — they are counted by [`Eventing::pending_append`], because "the
    /// connection was accepted but no client is listed" and "nobody subscribed"
    /// are different failures and the page has to be able to tell them apart.
    pub fn subscribers(&self) -> Vec<SubscriberRow> {
        let now = now_secs();
        self.published()
            .values()
            .map(|client| {
                let lease = client.lease.lock().unwrap();
                SubscriberRow {
                    sid: client.sid.clone(),
                    service: client.service.clone(),
                    url: client.url.clone(),
                    host: client.host.clone(),
                    path: client.path.clone(),
                    timeout: lease.timeout,
                    seq: lease.seq,
                    errors: lease.error,
                    timed_out: now > lease.start + lease.timeout,
                }
            })
            .collect()
    }

    /// Subscriptions accepted this instant but not yet in the table.
    pub fn pending_append(&self) -> usize {
        self.append_queue.lock().unwrap().len()
    }

    /// The machinery's four counters and two flags, as one read.
    ///
    /// One read because `query=subscribers` prints them **as a set**: the reference
    /// takes each with a separate `getattr` on a live object and can therefore
    /// report a half-changed picture, while here a diagnostic whose numbers come
    /// from three moments is worth less than one that admits it is a snapshot. The
    /// pair that matters is `running` against `event_thread_alive` — the useful
    /// state is the disagreement, exactly as
    /// [`Eventing::is_running`] says — and the third that matters is
    /// `pending_append` staying non-zero, which is "the connection was accepted but
    /// nobody is listed" rather than "nobody subscribed" (AGENTS.md §4.2's
    /// subscriber-queue trap, the reason this query exists at all).
    pub fn diagnostics(&self) -> Diagnostics {
        Diagnostics {
            running: self.is_running(),
            event_thread_alive: self.event_thread_alive(),
            pending_append: self.pending_append(),
            pending_state: self.pending_state(),
            pending_remove: self.pending_remove(),
        }
    }

    /// State changes waiting for the next tick's broadcast.
    pub fn pending_state(&self) -> usize {
        self.state_queue.lock().unwrap().len()
    }

    /// Removals queued and not yet published. The reference does not report this
    /// one; it is here because a stuck reaper is the same shape of failure as a
    /// stuck appender, and it would otherwise show up as "subscribers that never
    /// go away".
    pub fn pending_remove(&self) -> usize {
        self.remove_queue.lock().unwrap().len()
    }

    /// Whether we have asked the event thread to run — `start()` sets it,
    /// `stop()` clears it. Read it **together with**
    /// [`Eventing::event_thread_alive`], never instead of it: the useful state is
    /// the disagreement between the two ("we think it is running, the thread is
    /// gone"), which is exactly the silently-dead GENA the reference's subscriber
    /// page exists to catch.
    pub fn is_running(&self) -> bool {
        self.running.load(Ordering::SeqCst)
    }

    /// Whether the event thread is still running itself, as opposed to whether we
    /// asked it to. False before `start()` and after the thread has returned, by
    /// panic or by clean stop alike.
    pub fn event_thread_alive(&self) -> bool {
        self.event_thread
            .lock()
            .unwrap()
            .as_ref()
            .is_some_and(|handle| !handle.is_finished())
    }
}

impl Subscription for Eventing {
    fn add(&self, service: &str, url: &str, timeout: i64) -> (String, String) {
        self.log.error(&format!("SUBSCRIBE: {url}"));
        for client in self.published().values() {
            if client.url == url && client.service == service {
                client.update(timeout);
                self.log
                    .error(&format!("SUBSCRIBE UPDATE {} {}", client.sid, url));
                return (client.sid.clone(), format!("Second-{}", client.timeout()));
            }
        }
        self.log.error(&format!("SUBSCRIBE ADD {url}"));
        let client = Arc::new(Subscriber::new(service, url, timeout));
        self.append_queue.lock().unwrap().push(Arc::clone(&client));
        let sem = Arc::clone(&self.init_sem);
        let store = Arc::clone(&self.store);
        let log = Arc::clone(&self.log);
        let init_client = Arc::clone(&client);
        let spawned = std::thread::Builder::new()
            .name("gena-init".to_string())
            .spawn(move || {
                sem.acquire();
                send_init_event(store.as_ref(), log.as_ref(), &init_client);
                sem.release();
            });
        if let Err(e) = spawned {
            self.log
                .error(&format!("init thread did not start: {e}"));
        }
        (client.sid.clone(), format!("Second-{timeout}"))
    }

    fn renew(&self, sid: &str, timeout: i64) -> u16 {
        match self.published().get(sid) {
            None => 412,
            Some(client) => {
                client.update(timeout);
                200
            }
        }
    }

    fn remove(&self, sid: &str) -> u16 {
        self.remove_subscribe(sid)
    }

    /// The same snapshot [`Eventing::subscribers`] takes, handed to the HTTP face
    /// for `query=status`'s `clients` list. It is not a second read of the table:
    /// one `published()` binding, one lease per row — a face that walked the live
    /// map instead would be iterating a structure the event thread is republishing
    /// (the failure Python's Part 49 was written about, and the reason this door
    /// returns rows rather than a guard).
    fn rows(&self) -> Vec<SubscriberRow> {
        self.subscribers()
    }

    /// The inherent snapshot of the same name; `self.x()` binds to that, so this
    /// is a forward and not the trait calling itself.
    fn diagnostics(&self) -> Diagnostics {
        Eventing::diagnostics(self)
    }
}

impl Broadcast for Eventing {
    fn enqueue(&self, name: &str, value: &Value) {
        self.state_queue
            .lock()
            .unwrap()
            .push_back((name.to_string(), value.clone()));
        self.state_event.notify_all();
    }
}

/// Greet a fresh subscriber with the values it observes right now, skipping
/// the ones the store cannot answer. Failures only log.
fn send_init_event(store: &dyn StateStore, log: &Logger, client: &Subscriber) {
    let mut changes: Vec<(String, Value)> = Vec::new();
    for name in observed_for(&client.service) {
        if let Some(value) = store.read(name) {
            changes.push((name.to_string(), value));
        }
    }
    let body = propertyset_body(&client.service, &changes);
    match deliver(client, client.seq(), &body) {
        Ok(()) => client.advance_seq(),
        Err(e) => log.error(&format!("{e}")),
    }
}

fn now_secs() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// Python reads the callback with two regexes: host = every digit, `:` and
/// `.` after `//`, path = everything that follows. An IPv6 literal therefore
/// lands entirely in the path half — kept as is.
fn split_callback(url: &str) -> (String, String) {
    let Some((_, rest)) = url.split_once("//") else {
        return (String::new(), String::new());
    };
    let host: String = rest
        .chars()
        .take_while(|c| c.is_ascii_digit() || *c == ':' || *c == '.')
        .collect();
    (host.clone(), rest[host.len()..].to_string())
}

fn escape_text(text: &str) -> String {
    text.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;")
}

fn escape_attr(text: &str) -> String {
    escape_text(text).replace('"', "&quot;")
}

fn uuid_v4() -> String {
    let mut hasher = RandomState::new().build_hasher();
    let marker = ();
    hasher.write_usize(&marker as *const () as usize);
    let mut bits = hasher.finish() as u128;
    hasher.write_u128(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0),
    );
    bits ^= (hasher.finish() as u128) << 64;
    bits ^= (std::process::id() as u128) << 96;
    for _ in 0..3 {
        bits ^= bits << 13;
        bits ^= bits >> 7;
        bits ^= bits << 17;
    }
    bits = (bits & !(0xf_u128 << 76)) | (4_u128 << 76);
    bits = (bits & !(0x3_u128 << 62)) | (2_u128 << 62);
    format!(
        "{:08x}-{:04x}-{:04x}-{:04x}-{:012x}",
        (bits >> 96) as u32,
        ((bits >> 80) & 0xffff) as u16,
        ((bits >> 64) & 0xffff) as u16,
        ((bits >> 48) & 0xffff) as u16,
        bits & 0xffff_ffff_ffff
    )
}

fn deliver(client: &Subscriber, seq: u64, body: &[u8]) -> io::Result<()> {
    let host = if client
        .host
        .rsplit(':')
        .next()
        .is_some_and(|port| !port.is_empty() && port.chars().all(|c| c.is_ascii_digit()))
    {
        client.host.clone()
    } else {
        format!("{}:80", client.host)
    };
    let mut last_err: Option<io::Error> = None;
    let mut stream: Option<TcpStream> = None;
    for addr in host
        .to_socket_addrs()
        .map_err(|e| io::Error::new(e.kind(), format!("{}: {e}", client.url)))?
    {
        match TcpStream::connect_timeout(&addr, DELIVER_TIMEOUT) {
            Ok(s) => {
                stream = Some(s);
                break;
            }
            Err(e) => last_err = Some(e),
        }
    }
    let mut stream = match stream {
        Some(s) => s,
        None => {
            return Err(match last_err {
                Some(e) => io::Error::new(e.kind(), format!("{}: {e}", client.url)),
                None => io::Error::new(
                    io::ErrorKind::Other,
                    format!("{}: no address resolved", client.url),
                ),
            })
        }
    };
    stream.set_write_timeout(Some(DELIVER_TIMEOUT))?;
    let request = format!(
        "NOTIFY {} HTTP/1.1\r\nHost: {}\r\nContent-Length: {}\r\n\
         NT: upnp:event\r\nNTS: upnp:propchange\r\n\
         CONTENT-TYPE: text/xml; charset=\"utf-8\"\r\n\
         SERVER: {}\r\nSID: {}\r\nSEQ: {}\r\nTIMEOUT: Second-{}\r\n\r\n",
        client.path,
        client.host,
        body.len(),
        macast_core::paths::server_info(),
        client.sid,
        seq,
        client.timeout(),
    );
    stream.write_all(request.as_bytes())?;
    stream.write_all(body)?;
    stream.flush()?;
    // Python closed the socket right after the request; no reply is read.
    Ok(())
}

fn propertyset_body(service: &str, changes: &[(String, Value)]) -> Vec<u8> {
    let mut body = String::new();
    body.push_str(XML_DECL);
    body.push_str(&format!("<e:propertyset xmlns:e=\"{EVENT_NS}\">"));
    if service == "ConnectionManager" {
        if changes.is_empty() {
            body.pop();
            body.push_str("/></e:propertyset>");
            return body.into_bytes();
        }
        for (name, value) in changes {
            body.push_str(&format!(
                "<e:property><{name}>{}</{name}></e:property>",
                escape_text(&wire(value))
            ));
        }
        body.push_str("</e:propertyset>");
        return body.into_bytes();
    }
    body.push_str("<e:property><LastChange>");
    let mut inner = String::new();
    inner.push_str(XML_DECL);
    inner.push_str(&format!("<Event xmlns=\"{AVT_NS}\">"));
    inner.push_str("<InstanceID val=\"0\"");
    if changes.is_empty() {
        inner.push_str("/></Event>");
    } else {
        inner.push('>');
        for (name, value) in changes {
            inner.push_str(&format!(
                "<{name} val=\"{}\"/>",
                escape_attr(&wire(value))
            ));
        }
        inner.push_str("</InstanceID></Event>");
    }
    body.push_str(&escape_text(&inner));
    body.push_str("</LastChange></e:property></e:propertyset>");
    body.into_bytes()
}

#[cfg(test)]
mod tests {
    use super::*;
    use macast_core::{logging, paths};
    use serde_json::{json, Value};
    use std::collections::HashMap;
    use std::io::{self, Read};
    use std::net::TcpListener;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Arc, Mutex};
    use std::thread::JoinHandle;
    use std::time::{Duration, Instant};

    const DEAD_URL: &str = "http://127.0.0.1:1/dead";
    const INIT_DEAD_URL: &str = "http://127.0.0.1:2/init-dead";

    struct FakeStore {
        values: Mutex<HashMap<String, Value>>,
    }

    impl FakeStore {
        fn new() -> Self {
            FakeStore {
                values: Mutex::new(HashMap::new()),
            }
        }

        fn seeded(entries: &[(String, Value)]) -> Self {
            let store = FakeStore::new();
            {
                let mut values = store.values.lock().unwrap();
                for (name, value) in entries {
                    values.insert(name.clone(), value.clone());
                }
            }
            store
        }
    }

    impl StateStore for FakeStore {
        fn read(&self, name: &str) -> Option<Value> {
            self.values.lock().unwrap().get(name).cloned()
        }

        fn write(&self, name: &str, value: Value) -> bool {
            self.values.lock().unwrap().insert(name.to_string(), value);
            true
        }

        fn playlist_push(&self, _uri: &str) -> (i64, i64) {
            (0, 0)
        }

        fn playlist(&self) -> Vec<String> {
            Vec::new()
        }

        fn playlist_index(&self) -> i64 {
            0
        }

        fn set_playlist_index(&self, _index: i64) {}

        fn add_history(&self, _title: &str, _uri: &str) {}

        fn play_history(&self) -> Vec<(String, String, i64)> {
            Vec::new()
        }
    }

    struct Collector {
        url: String,
        host: String,
        stop: Arc<AtomicBool>,
        handle: Option<JoinHandle<()>>,
        received: Arc<Mutex<Vec<String>>>,
    }

    impl Collector {
        fn start() -> Collector {
            let listener = TcpListener::bind("127.0.0.1:0").expect("bind the collector");
            let addr = listener.local_addr().expect("collector address");
            let stop = Arc::new(AtomicBool::new(false));
            let received: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
            let thread_stop = Arc::clone(&stop);
            let thread_received = Arc::clone(&received);
            let handle = std::thread::Builder::new()
                .name("test-collector".to_string())
                .spawn(move || {
                    listener
                        .set_nonblocking(true)
                        .expect("collector listener nonblocking");
                    while !thread_stop.load(Ordering::SeqCst) {
                        match listener.accept() {
                            Ok((mut stream, _addr)) => {
                                let _ = stream.set_read_timeout(Some(Duration::from_secs(2)));
                                let mut buf = Vec::new();
                                let _ = stream.read_to_end(&mut buf);
                                thread_received
                                    .lock()
                                    .unwrap()
                                    .push(String::from_utf8_lossy(&buf).into_owned());
                            }
                            Err(ref e) if e.kind() == io::ErrorKind::WouldBlock => {
                                std::thread::sleep(Duration::from_millis(10));
                            }
                            Err(_e) => break,
                        }
                    }
                })
                .expect("spawn the collector thread");
            Collector {
                url: format!("http://{addr}"),
                host: addr.to_string(),
                stop,
                handle: Some(handle),
                received,
            }
        }

        fn wait_for(&self, needle: &str, secs: u64) {
            let deadline = Instant::now() + Duration::from_secs(secs);
            while Instant::now() < deadline {
                if self.all().iter().any(|req| req.contains(needle)) {
                    return;
                }
                std::thread::sleep(Duration::from_millis(25));
            }
            panic!(
                "the collector never saw {needle:?} within {secs}s ({} request(s) arrived)",
                self.count()
            );
        }

        fn count(&self) -> usize {
            self.received.lock().unwrap().len()
        }

        fn all(&self) -> Vec<String> {
            self.received.lock().unwrap().clone()
        }
    }

    impl Drop for Collector {
        fn drop(&mut self) {
            self.stop.store(true, Ordering::SeqCst);
            if let Some(handle) = self.handle.take() {
                let _ = handle.join();
            }
        }
    }

    fn log_tail() -> String {
        logging::read_tail(&paths::log_file(), 4000).text
    }

    fn wait_for_log(needle: &str, secs: u64) {
        let deadline = Instant::now() + Duration::from_secs(secs);
        while Instant::now() < deadline {
            if log_tail().contains(needle) {
                return;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        panic!("the log never mentioned {needle:?} within {secs}s");
    }

    fn wait_for_seq(client: &Subscriber, want: u64, secs: u64) -> u64 {
        let deadline = Instant::now() + Duration::from_secs(secs);
        loop {
            let seq = client.lease.lock().unwrap().seq;
            if seq >= want {
                return seq;
            }
            if Instant::now() >= deadline {
                panic!("seq never reached {want} (stuck at {seq})");
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    fn content_length(head: &str) -> usize {
        let line = head
            .lines()
            .find(|line| line.to_ascii_lowercase().starts_with("content-length:"))
            .expect("Content-Length header missing");
        line["content-length:".len()..]
            .trim()
            .parse()
            .expect("Content-Length is not a number")
    }

    #[test]
    fn subscriber_sid_looks_like_a_uuid_v4() {
        crate::pin_test_dir();
        let client = Subscriber::new("AVTransport", DEAD_URL, 1800);
        let raw = client.sid.strip_prefix("uuid:").expect("sid is uuid-prefixed");
        assert_eq!(raw.len(), 36, "{raw}");
        assert_eq!(&raw[8..9], "-");
        assert_eq!(&raw[13..14], "-");
        assert_eq!(&raw[18..19], "-");
        assert_eq!(&raw[23..24], "-");
        for (index, ch) in raw.char_indices() {
            if matches!(index, 8 | 13 | 18 | 23) {
                assert_eq!(ch, '-', "position {index}");
            } else {
                assert!(ch.is_ascii_hexdigit(), "position {index} is {ch:?}");
            }
        }
        assert_eq!(&raw[14..15], "4", "version nibble");
        assert!(
            matches!(&raw[19..20], "8" | "9" | "a" | "b"),
            "variant nibble"
        );
    }

    #[test]
    fn add_answers_the_requested_timeout_text() {
        crate::pin_test_dir();
        let ev = Eventing::new(Arc::new(FakeStore::new()));
        let (sid, timeout) = ev.add("AVTransport", DEAD_URL, 60);
        assert!(sid.starts_with("uuid:"), "{sid}");
        assert_eq!(timeout, "Second-60");
        let collector = Collector::start();
        let (sid2, timeout2) = ev.add("AVTransport", &collector.url, 1800);
        assert!(sid2.starts_with("uuid:"), "{sid2}");
        assert_eq!(timeout2, "Second-1800");
        assert_ne!(sid, sid2);
    }

    #[test]
    fn renew_answers_412_for_an_unknown_sid() {
        crate::pin_test_dir();
        let ev = Eventing::new(Arc::new(FakeStore::new()));
        assert_eq!(ev.renew("uuid:nobody", 60), 412);
    }

    #[test]
    fn renew_answers_200_once_the_sid_is_published() {
        crate::pin_test_dir();
        let ev = Eventing::new(Arc::new(FakeStore::new()));
        let (sid, _) = ev.add("AVTransport", DEAD_URL, 60);
        assert_eq!(ev.renew(&sid, 120), 412, "a queued append is not visible yet");
        ev.tick();
        assert_eq!(ev.renew(&sid, 120), 200);
        let client = ev.published().get(&sid).expect("the sid is published").clone();
        assert_eq!(client.timeout(), 120);
    }

    #[test]
    fn remove_answers_200_even_for_an_unknown_sid() {
        crate::pin_test_dir();
        let ev = Eventing::new(Arc::new(FakeStore::new()));
        assert_eq!(ev.remove("uuid:nobody"), 200);
    }

    #[test]
    fn a_remove_before_the_first_sync_keeps_the_subscriber() {
        crate::pin_test_dir();
        let ev = Eventing::new(Arc::new(FakeStore::new()));
        let (sid, _) = ev.add("AVTransport", DEAD_URL, 60);
        assert_eq!(ev.remove(&sid), 200);
        ev.tick();
        assert!(
            ev.published().contains_key(&sid),
            "the removal is still queued behind the append"
        );
    }

    #[test]
    fn remove_then_tick_empties_the_table() {
        crate::pin_test_dir();
        let ev = Eventing::new(Arc::new(FakeStore::new()));
        let (sid, _) = ev.add("AVTransport", DEAD_URL, 60);
        ev.tick();
        assert_eq!(ev.remove(&sid), 200);
        ev.tick();
        assert!(!ev.published().contains_key(&sid));
    }

    #[test]
    fn a_second_add_with_the_same_url_and_service_reuses_the_sid() {
        crate::pin_test_dir();
        let ev = Eventing::new(Arc::new(FakeStore::new()));
        let collector = Collector::start();
        let (sid, timeout) = ev.add("AVTransport", &collector.url, 1800);
        assert_eq!(timeout, "Second-1800");
        ev.tick();
        let (sid2, timeout2) = ev.add("AVTransport", &collector.url, 90);
        assert_eq!(sid2, sid, "the same url+service reuses the sid");
        assert_eq!(timeout2, "Second-90");
        ev.tick();
        assert_eq!(ev.published().len(), 1);
        let client = ev.published().get(&sid).expect("published").clone();
        assert_eq!(client.timeout(), 90);
    }

    #[test]
    fn before_the_first_sync_two_adds_get_two_sids() {
        crate::pin_test_dir();
        let ev = Eventing::new(Arc::new(FakeStore::new()));
        let avt = Collector::start();
        let rc = Collector::start();
        let (sid_a, _) = ev.add("AVTransport", &avt.url, 1800);
        let (sid_b, _) = ev.add("RenderingControl", &rc.url, 1800);
        assert_ne!(sid_a, sid_b);
        ev.tick();
        assert_eq!(ev.published().len(), 2);
    }

    #[test]
    fn the_same_url_under_a_different_service_is_a_new_subscription() {
        crate::pin_test_dir();
        let ev = Eventing::new(Arc::new(FakeStore::new()));
        let collector = Collector::start();
        let (sid_a, _) = ev.add("AVTransport", &collector.url, 1800);
        let (sid_b, _) = ev.add("RenderingControl", &collector.url, 1800);
        assert_ne!(sid_a, sid_b);
        ev.tick();
        assert_eq!(ev.published().len(), 2);
    }

    #[test]
    fn an_expired_lease_is_reaped_without_any_state_change() {
        crate::pin_test_dir();
        let ev = Eventing::new(Arc::new(FakeStore::new()));
        let (sid, _) = ev.add("AVTransport", DEAD_URL, 60);
        ev.tick();
        let client = ev.published().get(&sid).expect("published").clone();
        client.lease.lock().unwrap().start -= 2000;
        ev.tick();
        assert!(!ev.published().contains_key(&sid), "the expired lease is reaped");
    }

    #[test]
    fn a_renewal_restarts_an_expired_lease() {
        crate::pin_test_dir();
        let ev = Eventing::new(Arc::new(FakeStore::new()));
        let (sid, _) = ev.add("AVTransport", DEAD_URL, 60);
        ev.tick();
        let client = ev.published().get(&sid).expect("published").clone();
        client.lease.lock().unwrap().start -= 2000;
        assert_eq!(ev.renew(&sid, 300), 200, "renewal has no expiry gate");
        ev.tick();
        assert!(
            ev.published().contains_key(&sid),
            "the renewed lease survives the reap"
        );
        assert_eq!(client.timeout(), 300);
    }

    /// Read the subscriber rows on **another thread**, with a deadline.
    ///
    /// `subscribers()` is the thing under test, and a wrong lease lock there does
    /// not panic — `std::sync::Mutex` is not reentrant, so it *blocks*. A harness
    /// that waits for it forever therefore reports nothing at all, which is worse
    /// than a red test; this helper turns that hang into a loud failure. It is not
    /// a contrivance: in production this reader is the Tauri `subscriber_snapshot`
    /// command, which runs on the main thread, so the blocked lock is a frozen
    /// window while the HTTP services keep answering.
    fn read_rows_in_time(ev: &Arc<Eventing>, secs: u64) -> Vec<SubscriberRow> {
        let (tx, rx) = std::sync::mpsc::channel::<Vec<SubscriberRow>>();
        let reader = Arc::clone(ev);
        std::thread::Builder::new()
            .name("subscriber-rows-reader".to_string())
            .spawn(move || {
                let _ = tx.send(reader.subscribers());
            })
            .expect("the reader thread starts");
        match rx.recv_timeout(Duration::from_secs(secs)) {
            Ok(rows) => rows,
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => panic!(
                "the reader thread died before answering; its own panic message \
                 above is the real failure"
            ),
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => panic!(
                "`Eventing::subscribers()` did not answer within {secs}s, i.e. it is \
                 blocked on a lease it already holds. One lock per row: a second \
                 `lease.lock()` inside the row — through any accessor — is a deadlock \
                 that only appears once a control point is subscribed, which is the \
                 state a test over an empty table never reaches."
            ),
        }
    }

    #[test]
    fn the_subscriber_rows_answer_once_the_table_is_not_empty() {
        crate::pin_test_dir();
        let ev = Eventing::new(Arc::new(FakeStore::new()));
        let (sid, _) = ev.add("AVTransport", DEAD_URL, 300);
        ev.tick();
        assert!(
            ev.published().contains_key(&sid),
            "the subscription is published"
        );

        let rows = read_rows_in_time(&ev, 5);
        assert_eq!(rows.len(), 1, "one subscriber, one row");
        assert_eq!(rows[0].sid, sid);
        assert_eq!(rows[0].service, "AVTransport");
        assert_eq!(rows[0].url, DEAD_URL);
        assert_eq!(rows[0].timeout, 300, "the granted timeout, in seconds");
        assert_eq!(rows[0].seq, 0, "nothing has been acknowledged");
        assert_eq!(rows[0].errors, 0, "no broadcast attempted yet");
        assert!(!rows[0].timed_out, "the lease is young");

        ev.enqueue("TransportState", &json!("PLAYING"));
        ev.tick();
        let rows = read_rows_in_time(&ev, 5);
        assert_eq!(rows[0].errors, 1, "the dead callback counted one failure");
        assert_eq!(rows[0].seq, 0, "a failure never advances the seq");

        ev.published()
            .get(&sid)
            .expect("still published")
            .lease
            .lock()
            .unwrap()
            .start -= 2000;
        let rows = read_rows_in_time(&ev, 5);
        assert!(rows[0].timed_out, "the row says the lease ran out");
    }

    #[test]
    fn the_avtransport_body_is_one_lastchange_holding_a_detached_event() {
        crate::pin_test_dir();
        let expected = "<?xml version='1.0' encoding='UTF-8'?>\n\
                        <e:propertyset xmlns:e=\"urn:schemas-upnp-org:event-1-0\">\
                        <e:property><LastChange>\
                        &lt;?xml version='1.0' encoding='UTF-8'?&gt;\n\
                        &lt;Event xmlns=\"urn:schemas-upnp-org:metadata-1-0/AVT/\"&gt;\
                        &lt;InstanceID val=\"0\"&gt;\
                        &lt;TransportState val=\"PAUSED_PLAYBACK\"/&gt;\
                        &lt;/InstanceID&gt;&lt;/Event&gt;\
                        </LastChange></e:property></e:propertyset>";
        let body = propertyset_body(
            "AVTransport",
            &[("TransportState".to_string(), json!("PAUSED_PLAYBACK"))],
        );
        assert_eq!(String::from_utf8(body).unwrap(), expected);
    }

    #[test]
    fn the_connection_manager_body_is_one_property_per_variable() {
        crate::pin_test_dir();
        let expected = "<?xml version='1.0' encoding='UTF-8'?>\n\
                        <e:propertyset xmlns:e=\"urn:schemas-upnp-org:event-1-0\">\
                        <e:property><A_ARG_TYPE_Direction>Input</A_ARG_TYPE_Direction></e:property>\
                        <e:property><SinkProtocolInfo>http-get:*:*:*</SinkProtocolInfo></e:property>\
                        <e:property><CurrentConnectionIDs>0</CurrentConnectionIDs></e:property>\
                        </e:propertyset>";
        let changes = vec![
            ("A_ARG_TYPE_Direction".to_string(), json!("Input")),
            ("SinkProtocolInfo".to_string(), json!("http-get:*:*:*")),
            ("CurrentConnectionIDs".to_string(), json!("0")),
        ];
        let body = propertyset_body("ConnectionManager", &changes);
        assert_eq!(String::from_utf8(body).unwrap(), expected);
    }

    #[test]
    fn the_wire_spellings_reach_the_attributes() {
        crate::pin_test_dir();
        let changes = vec![
            ("Volume".to_string(), json!(30)),
            ("Mute".to_string(), json!(false)),
            ("Brightness".to_string(), json!(50)),
        ];
        let body = String::from_utf8(propertyset_body("RenderingControl", &changes)).unwrap();
        assert!(body.contains("&lt;Volume val=\"30\"/&gt;"), "{body}");
        assert!(body.contains("&lt;Mute val=\"False\"/&gt;"), "{body}");
        assert!(body.contains("&lt;Brightness val=\"50\"/&gt;"), "{body}");
    }

    #[test]
    fn attribute_entities_are_escaped_twice_through_lastchange() {
        crate::pin_test_dir();
        let body = String::from_utf8(propertyset_body(
            "AVTransport",
            &[
                ("TransportState".to_string(), json!("A&B<C>")),
                ("TransportStatus".to_string(), json!("sa\"y")),
            ],
        ))
        .unwrap();
        assert!(
            body.contains("&lt;TransportState val=\"A&amp;amp;B&amp;lt;C&amp;gt;\"/&gt;"),
            "{body}"
        );
        assert!(
            body.contains("&lt;TransportStatus val=\"sa&amp;quot;y\"/&gt;"),
            "{body}"
        );
    }

    #[test]
    fn an_empty_change_set_self_closes_each_shape() {
        crate::pin_test_dir();
        let cm = String::from_utf8(propertyset_body("ConnectionManager", &[])).unwrap();
        assert!(cm.ends_with("\"/></e:propertyset>"), "{cm}");
        assert!(!cm.contains("<e:property>"), "{cm}");
        let avt = String::from_utf8(propertyset_body("AVTransport", &[])).unwrap();
        assert!(avt.contains("<e:property><LastChange>"), "{avt}");
        assert!(avt.contains("&lt;InstanceID val=\"0\"/&gt;"), "{avt}");
    }

    #[test]
    fn an_init_event_arrives_with_seq_zero_and_the_observed_order() {
        crate::pin_test_dir();
        let changes: Vec<(String, Value)> = observed_for("AVTransport")
            .iter()
            .map(|name| (name.to_string(), json!(format!("s-{name}"))))
            .collect();
        let ev = Eventing::new(Arc::new(FakeStore::seeded(&changes)));
        let collector = Collector::start();
        let (sid, _) = ev.add("AVTransport", &collector.url, 1800);
        collector.wait_for("SEQ: 0", 5);
        let req = collector.all().remove(0);
        assert!(req.starts_with("NOTIFY "), "{req}");
        assert!(
            req.contains(&format!("Host: {}\r\n", collector.host)),
            "{req}"
        );
        assert!(req.contains(&format!("SID: {sid}\r\n")), "{req}");
        assert!(req.contains("TIMEOUT: Second-1800\r\n"), "{req}");
        assert!(req.contains("SERVER: "), "{req}");
        assert!(req.contains("UPnP/1.0 Macast/"), "{req}");
        let expected = propertyset_body("AVTransport", &changes);
        assert_eq!(content_length(&req), expected.len(), "{req}");
        let sep = req.find("\r\n\r\n").expect("header/body separator");
        assert_eq!(&req.as_bytes()[sep + 4..], expected.as_slice(), "{req}");
        let mut last = 0usize;
        for name in observed_for("AVTransport") {
            let marker = format!("&lt;{name} val=\"s-{name}\"/&gt;");
            let pos = req
                .find(&marker)
                .unwrap_or_else(|| panic!("{name} missing from the init body"));
            assert!(pos > last, "{name} appears out of order");
            last = pos;
        }
    }

    #[test]
    fn the_notify_headers_keep_the_wire_order_and_lengths() {
        crate::pin_test_dir();
        let changes = vec![
            ("Volume".to_string(), json!(30)),
            ("Mute".to_string(), json!(false)),
        ];
        let ev = Eventing::new(Arc::new(FakeStore::seeded(&changes)));
        let collector = Collector::start();
        let (_sid, _) = ev.add("RenderingControl", &collector.url, 60);
        collector.wait_for("SEQ: 0", 5);
        let broadcast: crate::store::SharedBroadcast = ev.clone();
        broadcast.enqueue("Volume", &json!(45));
        ev.tick();
        collector.wait_for("SEQ: 1", 5);
        let req = collector.all().remove(1);
        let sep = req.find("\r\n\r\n").expect("header/body separator");
        assert_eq!(content_length(&req), req.len() - sep - 4, "{req}");
        let needles = [
            "NOTIFY ",
            "Host: ",
            "Content-Length: ",
            "NT: ",
            "NTS: ",
            "CONTENT-TYPE: ",
            "SERVER: ",
            "SID: ",
            "SEQ: 1",
            "TIMEOUT: Second-60",
        ];
        let mut last: Option<usize> = None;
        for needle in needles {
            let from = last.unwrap_or(0);
            let pos = req[from..]
                .find(needle)
                .map(|p| p + from)
                .unwrap_or_else(|| panic!("{needle} missing from the request"));
            last = Some(pos);
        }
    }

    #[test]
    fn the_connection_manager_init_is_a_plain_propertyset() {
        crate::pin_test_dir();
        let changes = vec![
            ("A_ARG_TYPE_Direction".to_string(), json!("Input")),
            ("SinkProtocolInfo".to_string(), json!("http-get:*:*:*")),
        ];
        let ev = Eventing::new(Arc::new(FakeStore::seeded(&changes)));
        let collector = Collector::start();
        let (_sid, _) = ev.add("ConnectionManager", &collector.url, 1800);
        collector.wait_for("SEQ: 0", 5);
        let req = collector.all().remove(0);
        assert!(
            req.contains("<A_ARG_TYPE_Direction>Input</A_ARG_TYPE_Direction>"),
            "{req}"
        );
        assert!(
            req.contains("<SinkProtocolInfo>http-get:*:*:*</SinkProtocolInfo>"),
            "{req}"
        );
        assert!(!req.contains("LastChange"), "{req}");
    }

    #[test]
    fn only_the_service_that_observes_a_variable_hears_about_it() {
        crate::pin_test_dir();
        let ev = Eventing::new(Arc::new(FakeStore::new()));
        let collector = Collector::start();
        let (sid, _) = ev.add("ConnectionManager", &collector.url, 1800);
        collector.wait_for("SEQ: 0", 5);
        ev.tick();
        assert_eq!(ev.published().len(), 1, "{sid} is published");
        assert_eq!(collector.count(), 1);
        let broadcast: crate::store::SharedBroadcast = ev.clone();
        broadcast.enqueue("Volume", &json!(45));
        ev.tick();
        std::thread::sleep(Duration::from_millis(200));
        assert_eq!(collector.count(), 1, "Volume is not in the CM observed row");
    }

    #[test]
    fn a_change_reaches_the_observing_service_with_the_next_seq() {
        crate::pin_test_dir();
        let ev = Eventing::new(Arc::new(FakeStore::seeded(&[(
            "TransportState".to_string(),
            json!("STOPPED"),
        )])));
        let collector = Collector::start();
        let (sid, _) = ev.add("AVTransport", &collector.url, 1800);
        collector.wait_for("SEQ: 0", 5);
        ev.tick();
        let client = ev.published().get(&sid).expect("published").clone();
        assert_eq!(
            wait_for_seq(&client, 1, 5),
            1,
            "a successful init advances the seq"
        );
        let broadcast: crate::store::SharedBroadcast = ev.clone();
        broadcast.enqueue("TransportState", &json!("PLAYING"));
        ev.tick();
        collector.wait_for("SEQ: 1", 5);
        let req = collector.all().remove(1);
        assert!(req.contains("&lt;TransportState val=\"PLAYING\"/&gt;"), "{req}");
        assert_eq!(wait_for_seq(&client, 2, 5), 2);
    }

    #[test]
    fn later_writes_in_one_tick_win_and_fold_into_one_event() {
        crate::pin_test_dir();
        let ev = Eventing::new(Arc::new(FakeStore::seeded(&[(
            "TransportState".to_string(),
            json!("STOPPED"),
        )])));
        let collector = Collector::start();
        let (sid, _) = ev.add("AVTransport", &collector.url, 1800);
        collector.wait_for("SEQ: 0", 5);
        ev.tick();
        let client = ev.published().get(&sid).expect("published").clone();
        assert_eq!(
            wait_for_seq(&client, 1, 5),
            1,
            "a successful init advances the seq"
        );
        let broadcast: crate::store::SharedBroadcast = ev.clone();
        broadcast.enqueue("TransportState", &json!("PLAYING"));
        broadcast.enqueue("TransportStatus", &json!("OK"));
        broadcast.enqueue("TransportState", &json!("PAUSED_PLAYBACK"));
        ev.tick();
        collector.wait_for("SEQ: 1", 5);
        let req = collector.all().remove(1);
        assert!(
            req.contains("&lt;TransportState val=\"PAUSED_PLAYBACK\"/&gt;"),
            "{req}"
        );
        assert!(req.contains("&lt;TransportStatus val=\"OK\"/&gt;"), "{req}");
        assert!(!req.contains("val=\"PLAYING\""), "{req}");
        assert!(!req.contains("SEQ: 2"), "{req}");
        assert_eq!(collector.count(), 2);
        assert_eq!(wait_for_seq(&client, 2, 5), 2, "the fold sends one event");
    }

    #[test]
    fn a_failed_delivery_leaves_the_seq_untouched_and_counts_one_error() {
        crate::pin_test_dir();
        let ev = Eventing::new(Arc::new(FakeStore::new()));
        let (sid, _) = ev.add("AVTransport", DEAD_URL, 1800);
        ev.tick();
        let client = ev.published().get(&sid).expect("published").clone();
        assert_eq!(client.lease.lock().unwrap().error, 0);
        ev.send_states_to_clients(&[("TransportState".to_string(), json!("PLAYING"))]);
        assert_eq!(client.seq(), 0, "a failed delivery does not advance the seq");
        assert_eq!(client.lease.lock().unwrap().error, 1);
    }

    #[test]
    fn eleven_failed_deliveries_remove_the_client() {
        crate::pin_test_dir();
        let ev = Eventing::new(Arc::new(FakeStore::new()));
        let (sid, _) = ev.add("AVTransport", DEAD_URL, 1800);
        ev.tick();
        let client = ev.published().get(&sid).expect("published").clone();
        let change = [("TransportState".to_string(), json!("PLAYING"))];
        for _ in 0..10 {
            ev.send_states_to_clients(&change);
        }
        assert_eq!(
            client.lease.lock().unwrap().error,
            10,
            "ten failures, still subscribed"
        );
        assert!(
            ev.published().contains_key(&sid),
            "ten failures do not remove the client"
        );
        ev.send_states_to_clients(&change);
        assert_eq!(client.lease.lock().unwrap().error, 11);
        assert!(
            !ev.published().contains_key(&sid),
            "the eleventh failure removes the client"
        );
    }

    #[test]
    fn init_failures_never_count_as_client_errors() {
        crate::pin_test_dir();
        let ev = Eventing::new(Arc::new(FakeStore::new()));
        let (sid, _) = ev.add("AVTransport", INIT_DEAD_URL, 1800);
        ev.tick();
        let client = ev.published().get(&sid).expect("published").clone();
        wait_for_log(&format!("{INIT_DEAD_URL}: Connection refused"), 5);
        assert_eq!(
            client.lease.lock().unwrap().error,
            0,
            "init failures are not client errors"
        );
        assert_eq!(client.seq(), 0, "a failed init does not advance the seq");
    }

    #[test]
    fn the_loop_thread_delivers_without_an_explicit_tick() {
        crate::pin_test_dir();
        let ev = Eventing::new(Arc::new(FakeStore::seeded(&[(
            "TransportState".to_string(),
            json!("STOPPED"),
        )])));
        let collector = Collector::start();
        ev.start();
        let (sid, _) = ev.add("AVTransport", &collector.url, 1800);
        collector.wait_for("SEQ: 0", 5);
        ev.tick();
        assert_eq!(ev.published().len(), 1, "{sid} is published");
        let broadcast: crate::store::SharedBroadcast = ev.clone();
        broadcast.enqueue("TransportState", &json!("PLAYING"));
        collector.wait_for("SEQ: 1", 5);
        let req = collector.all().remove(1);
        assert!(req.contains("&lt;TransportState val=\"PLAYING\"/&gt;"), "{req}");
        ev.stop();
        assert_eq!(
            ev.published().len(),
            1,
            "stop locks the queue but drains nothing"
        );
    }

    #[test]
    fn split_callback_keeps_the_python_reading_of_the_url() {
        crate::pin_test_dir();
        assert_eq!(
            split_callback("http://192.168.1.5:8080/cb"),
            ("192.168.1.5:8080".to_string(), "/cb".to_string())
        );
        assert_eq!(split_callback("http://[::1]:9/cb"), ("".to_string(), "[::1]:9/cb".to_string()));
        assert_eq!(split_callback("nonsense"), ("".to_string(), "".to_string()));
        assert_eq!(
            split_callback("http://127.0.0.1:9/cb?sid=x"),
            ("127.0.0.1:9".to_string(), "/cb?sid=x".to_string())
        );
    }

    #[test]
    fn add_speaks_its_two_subscribe_lines_to_the_main_log() {
        crate::pin_test_dir();
        let ev = Eventing::new(Arc::new(FakeStore::new()));
        let collector = Collector::start();
        let (sid, first) = ev.add("AVTransport", &collector.url, 1800);
        assert_eq!(first, "Second-1800");
        ev.tick();
        let (sid2, second) = ev.add("AVTransport", &collector.url, 90);
        assert_eq!(sid2, sid);
        assert_eq!(second, "Second-90");
        ev.tick();
        let needle = format!("SUBSCRIBE: {}", collector.url);
        wait_for_log(&needle, 5);
        let tail = log_tail();
        assert_eq!(tail.matches(&needle).count(), 2, "{tail}");
        assert!(tail.contains("SUBSCRIBE ADD"), "{tail}");
        assert!(tail.contains("SUBSCRIBE UPDATE"), "{tail}");
        assert!(
            tail.contains(&format!("Add client: {sid} ({})", collector.url)),
            "{tail}"
        );
    }

    #[test]
    fn an_unobserved_change_name_never_reaches_a_subscriber() {
        crate::pin_test_dir();
        let ev = Eventing::new(Arc::new(FakeStore::new()));
        let collector = Collector::start();
        ev.add("AVTransport", &collector.url, 1800);
        collector.wait_for("SEQ: 0", 5);
        ev.tick();
        assert_eq!(collector.count(), 1);
        let broadcast: crate::store::SharedBroadcast = ev.clone();
        broadcast.enqueue("NotAnObservedVariable", &json!("x"));
        ev.tick();
        std::thread::sleep(Duration::from_millis(200));
        assert_eq!(
            collector.count(),
            1,
            "an unobserved name never reaches a subscriber"
        );
    }

    #[test]
    fn volume_goes_only_to_the_rendering_control_subscriber() {
        crate::pin_test_dir();
        let ev = Eventing::new(Arc::new(FakeStore::new()));
        let avt = Collector::start();
        let rc = Collector::start();
        ev.add("AVTransport", &avt.url, 1800);
        ev.add("RenderingControl", &rc.url, 1800);
        avt.wait_for("SEQ: 0", 5);
        rc.wait_for("SEQ: 0", 5);
        ev.tick();
        assert_eq!(ev.published().len(), 2);
        let broadcast: crate::store::SharedBroadcast = ev.clone();
        broadcast.enqueue("Volume", &json!(45));
        ev.tick();
        rc.wait_for("SEQ: 1", 5);
        let req = rc.all().remove(1);
        assert!(req.contains("&lt;Volume val=\"45\"/&gt;"), "{req}");
        std::thread::sleep(Duration::from_millis(200));
        assert_eq!(
            avt.count(),
            1,
            "Volume never reaches the AVTransport subscriber"
        );
    }
}
