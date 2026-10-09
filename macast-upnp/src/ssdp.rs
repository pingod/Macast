//! The receiver on UDP/1900: the registry of announced devices, `ssdp:alive`,
//! `ssdp:byebye`, and the M-SEARCH answer.
//!
//! Python counterpart: `macast/ssdp.py::SSDPServer`. Its module header carries a
//! third provenance layer (the Coherence author block — Tim Potter 2005,
//! John-Mark Gurney 2006, Fluendo 2006, Frank Scholz 2006-09, Erwan Martin 2016,
//! plus the fork's own lines), so rewriting this file away does not erase those
//! notices; see `docs/Provenance.md` and AGENTS.md §4.12.
//!
//! ## The three things that are not decoration
//!
//! * **Header order.** [`SsdpServer::register`] writes
//!   `USN, LOCATION, ST, EXT, SERVER, CACHE-CONTROL`, and the NT-rewrite in
//!   [`SsdpServer::do_notify`]/[`SsdpServer::do_byebye`] (`copy`, `NT = ST`,
//!   `del ST`) yields `USN, LOCATION, EXT, SERVER, CACHE-CONTROL, NT` with `NT`
//!   **last**, because Python's `del` leaves the key's original slot empty and a
//!   reassignment appends. Both orders reach the wire verbatim. That is why
//!   [`Entry::headers`] is a `Vec`.
//! * **The byebye flag is written *before* `running` is cleared.** See
//!   [`SsdpServer::stop`]: the recv thread only leaves its loop once it observes
//!   `running == false`, and it then calls [`SsdpServer::shutdown`], which reads
//!   the flag. The old order let a periodic `update_ip()` restart broadcast
//!   `ssdp:byebye` for a random prefix of the registered devices, and control
//!   points then treated the renderer as offline — the "casting drops after a
//!   while" report (AGENTS.md §4.2).
//! * **A bound port is not evidence of life.** [`SsdpServer::is_healthy`] exists
//!   because the recv loop only swallows *timeouts*: any other failure kills the
//!   thread while `running` stays `true` and UDP/1900 stays bound, so every
//!   socket-level probe still succeeds while nothing ever answers an M-SEARCH
//!   again. Only a thread check tells "idle" from "dead".
//!
//! ## What a failure here does *not* do
//!
//! Python's `run()` has no `try` around socket creation or the multicast
//! options, so those kill the thread outright. [`SsdpServer::serve`] keeps that
//! shape: on such a failure it returns [`Exit::Held`], which leaves the receiver,
//! the memberships, and the senders exactly as they are — the state
//! [`SsdpServer::is_healthy`] is written to notice. Only a clean exit (the loop
//! observing `running == false`) runs [`SsdpServer::shutdown`] and closes the
//! descriptors.

use std::collections::HashMap;
use std::io::ErrorKind;
use std::mem::MaybeUninit;
use std::net::{Ipv4Addr, SocketAddr, UdpSocket};
use std::slice;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread::ThreadId;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use macast_core::logging::{self, Logger};
use macast_core::net;
use macast_core::setting::Setting;
use macast_core::timefmt;
use socket2::{Domain, Protocol, Socket, Type};

use crate::sock::{set_reuse_for_ssdp, Sock};
use crate::store::SharedHooks;

/// `SSDP_PORT` — the well-known UDP port every control point sends M-SEARCH to.
pub const SSDP_PORT: u16 = 1900;

/// `SSDP_ADDR` — the admin-scoped multicast group. Kept as `&str` because Python
/// does, and because `%s` of it in `HOST:` must print as `239.255.255.250`.
pub const SSDP_ADDR: &str = "239.255.255.250";

/// `SERVER_ID` — the default `SERVER` header for a registered device.
pub const SERVER_ID: &str = "SSDP Server";

/// The group as an address, for the syscalls that need one.
fn ssdp_group() -> Ipv4Addr {
    // A const-checked parse: this literal is as fixed as it gets, and a typo here
    // would make every device undiscoverable rather than panic at startup.
    Ipv4Addr::new(239, 255, 255, 250)
}

/// The group:port every `NOTIFY` is addressed to.
fn group_destination() -> SocketAddr {
    SocketAddr::from((ssdp_group(), SSDP_PORT))
}

/// One announced device: its USN and its headers **in insertion order**.
///
/// Python's `self.known[usn]` is a plain dict, and dict order is the order the
/// six keys were written in `register()`. A `HashMap` here would reorder the
/// response headers, which no test on this machine can see and some control
/// points apparently do.
#[derive(Debug, Clone)]
pub struct Entry {
    /// `known[usn]['USN']` — always the key it is filed under.
    pub usn: String,
    /// `LOCATION, ST, EXT, SERVER, CACHE-CONTROL` follow `USN`, in that order.
    pub headers: Vec<(String, String)>,
}

impl Entry {
    /// `known[usn]['ST']` — the search target a control point's `ST` is matched
    /// against.
    pub fn st(&self) -> &str {
        for (k, v) in &self.headers {
            if k == "ST" {
                return v;
            }
        }
        ""
    }

    /// `dict(known[usn].items())` followed by `NT = ST` and `del ST`: the NT form
    /// of the header block, in the order Python would emit it.
    ///
    /// Returns `Err("ST")` when there is no `ST` to copy — the `KeyError` that
    /// Python's `stcpy['ST']` would raise. Unreachable for anything
    /// [`SsdpServer::register`] filed, and ported anyway because the two callers
    /// handle it *differently* (see [`SsdpServer::do_notify`] versus
    /// [`SsdpServer::do_byebye`]).
    fn nt_form(&self) -> Result<Vec<(String, String)>, String> {
        let mut out: Vec<(String, String)> = Vec::with_capacity(self.headers.len());
        let mut st: Option<String> = None;
        for (k, v) in &self.headers {
            if k == "ST" {
                st = Some(v.clone());
                continue;
            }
            out.push((k.clone(), v.clone()));
        }
        let st = st.ok_or_else(|| "ST".to_string())?;
        out.push(("NT".to_string(), st));
        Ok(out)
    }
}

/// Everything `run()` builds and `shutdown()` tears down, in one lock so the recv
/// thread and a restart cannot interleave through three separate fields.
#[derive(Default)]
struct SocketState {
    /// `self.sock` — the receiver. `Some` from the moment it is created, which is
    /// *before* the bind attempt, exactly as Python assigns `self.sock` on the
    /// line before its `try`. `do_byebye`'s `if self.sock:` reads this.
    receiver: Option<Arc<Socket>>,
    /// `self.ip_list` — the `(address, netmask)` pairs this announcement goes out
    /// through.
    ip_list: Vec<(String, String)>,
    /// `self.sock_list` — one sender per usable interface.
    senders: Vec<Arc<Sock>>,
    /// `self.sock_by_ip` — the same senders, keyed for the M-SEARCH reply, which
    /// must leave through the interface that shares a subnet with the requester.
    senders_by_ip: HashMap<String, Arc<Sock>>,
    /// `self.memberships` — the `(group, interface)` pairs to drop on the way out.
    memberships: Vec<(Ipv4Addr, Ipv4Addr)>,
}

/// Which thread the server believes is its recv thread, and whether that belief
/// is still current.
#[derive(Default)]
struct ThreadSlot {
    /// Bumped by every `start()`. A thread that is still unwinding from an
    /// *earlier* start must not blank the liveness flag of the one that replaced
    /// it — that is the only reason this counter exists.
    generation: u64,
    /// `self.ssdp_thread` — the join handle. Taken out by `stop()` and joined
    /// with **no lock held**.
    handle: Option<std::thread::JoinHandle<()>>,
    /// The id of the thread that claimed this generation, so `stop()` can skip
    /// the self-join Python guards against (`RuntimeError: cannot join current
    /// thread`). `None` until the spawned thread claims it.
    id: Option<ThreadId>,
}

/// How [`SsdpServer::serve`] finished.
enum Exit {
    /// The loop saw `running == false`: send byebye, drop memberships, close
    /// everything.
    Cleanup,
    /// Something the recv loop does not catch failed, or the bind was refused.
    /// Descriptors stay held so [`SsdpServer::is_healthy`] is the thing that
    /// notices — this is Python's "the thread dies and `running` stays true".
    Held,
}

/// Clears the liveness flag on the way out — but only for the generation that is
/// still current, and even on a panic (which Python's thread cannot do, and which
/// is the *better* side: a Rust panic in the recv thread leaves `running` true and
/// `alive` false, so the periodic restart sees it exactly as it sees a killed
/// socket).
struct AliveGuard {
    alive: Arc<AtomicBool>,
    slot: Arc<Mutex<ThreadSlot>>,
    generation: u64,
}

impl Drop for AliveGuard {
    fn drop(&mut self) {
        if current_generation(&self.slot) == self.generation {
            self.alive.store(false, Ordering::SeqCst);
        }
    }
}

fn current_generation(slot: &Arc<Mutex<ThreadSlot>>) -> u64 {
    match slot.lock() {
        Ok(g) => g.generation,
        // A poisoned slot means a thread died holding it; the generation it holds
        // is still the answer, so read it back rather than skip the check.
        Err(e) => e.into_inner().generation,
    }
}

/// A SSDP server: announces devices, answers M-SEARCH, and says goodbye on stop.
pub struct SsdpServer {
    /// `self.known` — **per-instance, not shared**, as Python's comment insists.
    known: Mutex<Vec<Entry>>,
    state: Mutex<SocketState>,
    /// `self.running`
    running: AtomicBool,
    /// `self.sending_byebye`
    sending_byebye: AtomicBool,
    /// The liveness answer behind [`SsdpServer::is_healthy`], set by `start()` and
    /// cleared by [`AliveGuard`].
    alive: Arc<AtomicBool>,
    slot: Arc<Mutex<ThreadSlot>>,
    hooks: SharedHooks,
    setting: Arc<Setting>,
    log: Arc<Logger>,
}

/// `logger = logging.getLogger("SSDPServer")`
impl SsdpServer {
    /// `SSDPServer()`.
    ///
    /// Python takes no arguments because it reads `Setting` globally; here the
    /// two things it needs from outside — the notification/service-stop hooks and
    /// the settings whose USN, port, and interfaces it announces — are handed in,
    /// so this crate never touches a user's real configuration by accident
    /// (AGENTS.md §10).
    pub fn new(hooks: SharedHooks, setting: Arc<Setting>) -> Arc<SsdpServer> {
        Arc::new(SsdpServer {
            known: Mutex::new(Vec::new()),
            state: Mutex::new(SocketState::default()),
            running: AtomicBool::new(false),
            // `__init__` sets it true; `start()` re-sets it, and `stop(byebye)` is
            // the only other writer.
            sending_byebye: AtomicBool::new(true),
            alive: Arc::new(AtomicBool::new(false)),
            slot: Arc::new(Mutex::new(ThreadSlot::default())),
            hooks,
            setting,
            log: Logger::named("SSDPServer"),
        })
    }

    /// `self.known` under a lock, with the poison read back rather than propagated:
    /// a panicked visitor left a perfectly usable registry behind.
    fn known(&self) -> MutexGuard<'_, Vec<Entry>> {
        match self.known.lock() {
            Ok(g) => g,
            Err(e) => e.into_inner(),
        }
    }

    fn state(&self) -> MutexGuard<'_, SocketState> {
        match self.state.lock() {
            Ok(g) => g,
            Err(e) => e.into_inner(),
        }
    }

    fn slot(&self) -> MutexGuard<'_, ThreadSlot> {
        match self.slot.lock() {
            Ok(g) => g,
            Err(e) => e.into_inner(),
        }
    }

    /// `start()` — spawn the recv thread, once.
    ///
    /// Python's `if not self.running:` is a check, not a claim, so two concurrent
    /// starts could both spawn. The `compare_exchange` here is strictly narrower
    /// in a safe direction: exactly one caller wins, and `running` is set before
    /// the thread exists, so a `stop()` that arrives mid-`start()` still finds a
    /// flag to clear and a handle to join.
    pub fn start(self: &Arc<Self>) {
        if self
            .running
            .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
            .is_err()
        {
            return;
        }
        self.sending_byebye.store(true, Ordering::SeqCst);
        self.alive.store(true, Ordering::SeqCst);

        let mut slot = self.slot();
        slot.generation += 1;
        let generation = slot.generation;
        // Python overwrites `self.ssdp_thread` here, abandoning any older handle.
        slot.handle = None;
        slot.id = None;
        drop(slot);

        let me = Arc::clone(&self);
        let spawned = std::thread::Builder::new()
            .name("SSDP_THREAD".to_string())
            .spawn(move || me.run_thread(generation));
        match spawned {
            Ok(handle) => self.slot().handle = Some(handle),
            Err(e) => {
                // `thread::spawn` can only fail by running out of OS resources.
                // Python would raise out of `start()` and leave `running` true; we
                // say so and put `running` back, because nothing can answer a
                // search and a restart driver should be allowed to try again.
                self.log.error(&format!("SSDP cannot start thread: {}", e));
                self.running.store(false, Ordering::SeqCst);
                self.alive.store(false, Ordering::SeqCst);
            }
        }
    }

    /// `stop(byebye=True)` — the order of the two writes is the whole point.
    ///
    /// 1. Publish the byebye policy **before** clearing `running`, because the recv
    ///    thread leaves its loop on `running == false` and then reads this flag
    ///    inside [`SsdpServer::shutdown`]. Getting this backwards made a
    ///    `stop(byebye=False)` — i.e. the periodic `update_ip()` restart — still
    ///    broadcast `ssdp:byebye` for a random prefix of the devices.
    /// 2. Wake the blocking recv so the thread leaves promptly.
    /// 3. Join *whenever* a handle exists, not only when `running` was set: a
    ///    `stop()` racing an already-exiting thread would otherwise return
    ///    immediately and the following `start()` could flip `sending_byebye` back
    ///    to true while the old thread is still inside `shutdown()`.
    ///
    /// The join happens with **no lock held** — joining while holding any of this
    /// struct's mutexes would deadlock against the thread's own cleanup.
    pub fn stop(&self, byebye: bool) {
        self.sending_byebye.store(byebye, Ordering::SeqCst);
        if self.running.load(Ordering::SeqCst) {
            self.running.store(false, Ordering::SeqCst);
            // `wake = socket.socket(AF_INET, SOCK_DGRAM)` + `sendto(b'', group:1900)`
            // + `close()` in a `finally`. Every error is swallowed (`except
            // Exception: pass`): this is a nudge, and a refused nudge must not turn
            // a deliberate stop into an exception for the caller.
            if let Ok(wake) = UdpSocket::bind("0.0.0.0:0") {
                let _ = wake.send_to(b"", group_destination());
            }
        }

        let handle = self.slot().handle.take();
        if let Some(handle) = handle {
            let is_self = {
                let slot = self.slot();
                slot.id == Some(std::thread::current().id())
            };
            if !is_self {
                // A `join()` error means the thread panicked; its [`AliveGuard`]
                // already cleared the flag, and `is_healthy()` is the reader that
                // cares. Nothing to report here that the flag does not say.
                let _ = handle.join();
            }
        }
    }

    /// `is_healthy()` — can this server still answer a discovery request right
    /// now?
    ///
    /// A deliberately stopped server answers **yes**: `running` is false because
    /// the user turned the service off (or because `update_ip()` is mid-restart),
    /// and that is not a fault — a caller that treated it as one would restart the
    /// server on every stop.
    ///
    /// The reason this question exists at all is [`SsdpServer::serve`]'s recv
    /// loop: it only handles the timeouts it expects, so any *other* failure kills
    /// the thread while `running` stays true and UDP/1900 stays bound. From
    /// outside that looks perfectly healthy — `lsof` still shows the port, a
    /// connect-style probe still succeeds — but nothing ever answers an M-SEARCH
    /// again, and a renderer nobody can discover is exactly the "searched for it,
    /// cast did nothing" symptom. So a liveness poll is the only thing that can
    /// tell "idle" from "dead", and it is what the periodic restart in the HTTP
    /// service asks before it tears the socket down.
    pub fn is_healthy(&self) -> bool {
        if !self.running.load(Ordering::SeqCst) {
            return true;
        }
        self.alive.load(Ordering::SeqCst)
    }

    /// Does this server currently have a recv thread marked as running? (The
    /// Python `self.running` read, exposed for the restart driver and the tests.)
    pub fn is_running(&self) -> bool {
        self.running.load(Ordering::SeqCst)
    }

    /// `self.ssdp_thread` claims and the guarded body. Split from [`SsdpServer::run`]
    /// only so the generation claim sits next to the guard that consumes it.
    fn run_thread(self: &Arc<Self>, generation: u64) {
        {
            let mut slot = self.slot();
            // Claim the id only while this generation is still the one that was
            // asked for; a thread that started late behind a restart must not
            // make itself the join target of the newer one.
            if slot.generation == generation {
                slot.id = Some(std::thread::current().id());
            }
        }
        let _guard = AliveGuard {
            alive: Arc::clone(&self.alive),
            slot: Arc::clone(&self.slot),
            generation,
        };
        self.run();
    }

    /// The socket, the options, the bind, the memberships, and the loop.
    ///
    /// A direct transcription of `run()`, including which calls are **not** inside
    /// a `try` — see [`Exit`].
    fn run(&self) {
        let sock = match Socket::new(Domain::IPV4, Type::DGRAM, Some(Protocol::UDP)) {
            Ok(s) => s,
            // Python: not guarded; the thread dies with a traceback and `running`
            // stays true. Same observable state via `Exit::Held`.
            Err(e) => {
                self.log.error(&e.to_string());
                return;
            }
        };
        // `self.sock` is assigned before anything else is done to it, so the
        // `if self.sock:` gate in `do_byebye` is true even on the bind-failure
        // path below.
        self.state().receiver = Some(Arc::new(sock));

        match self.serve() {
            Exit::Held => return,
            Exit::Cleanup => {}
        }
        self.shutdown();

        // IP_DROP_MEMBERSHIP for every group we joined. Python's
        // `except Exception: continue` — a membership the kernel will not drop is
        // not worth leaking the loop over.
        let (receiver, memberships) = {
            let mut state = self.state();
            let memberships = std::mem::take(&mut state.memberships);
            (state.receiver.clone(), memberships)
        };
        for (group, interface) in &memberships {
            if let Some(sock) = &receiver {
                let _ = sock.leave_multicast_v4(group, interface);
            }
        }
        // `self.sock_list = []; self.sock_by_ip = {}`. Python gets there *after*
        // calling `sender.close()` on each of them (`ssdp.py:273-274`); we achieve
        // the same descriptor release by dropping the last `Arc`, which is the
        // shape that survives a sender still borrowed by a concurrent
        // `send_it()` — that one finishes its `send_to` and then the socket closes
        // (see the note in [`crate::sock`]).
        {
            let mut state = self.state();
            state.senders.clear();
            state.senders_by_ip.clear();
        }
        // `self.sock.close(); self.sock = None`, same mechanism.
        let receiver = self.state().receiver.take();
        drop(receiver);
    }

    /// The option sequence, the bind, the memberships, and the receive loop.
    ///
    /// Which of these calls sits inside a `try` in Python is the whole content of
    /// this function: the four that are not each end the thread with a traceback
    /// while `running` stays true and UDP/1900 stays bound, so they return
    /// [`Exit::Held`] and leave every descriptor where it is. [`crate::ssdp::SsdpServer::is_healthy`]
    /// is what notices.
    fn serve(&self) -> Exit {
        // `self.sock` — held by `run()`; `None` here means the recv thread was
        // replaced under us, which Python can also do and which surfaces there as
        // `AttributeError: 'NoneType' object has no attribute 'setsockopt'`.
        let receiver = match self.state().receiver.clone() {
            Some(sock) => sock,
            None => {
                self.log.error(
                    "SSDP receiver socket is gone (a newer start() replaced this thread)",
                );
                return Exit::Held;
            }
        };

        // `IP_MULTICAST_LOOP, 0` — not guarded.
        if let Err(e) = receiver.set_multicast_loop_v4(false) {
            self.log.error(&e.to_string());
            return Exit::Held;
        }
        // The `SO_REUSE*` block — no outer `try` either, which is why a failing
        // `setsockopt` on macOS/Windows is fatal here and merely logged on Linux
        // (see [`crate::sock::set_reuse_for_ssdp`]).
        if let Err(e) = set_reuse_for_ssdp(&receiver, &self.log) {
            self.log.error(&e.to_string());
            return Exit::Held;
        }
        // `IP_MULTICAST_TTL, 4` — not guarded.
        if let Err(e) = receiver.set_multicast_ttl_v4(4) {
            self.log.error(&e.to_string());
            return Exit::Held;
        }

        // `try: self.sock.bind(('0.0.0.0', SSDP_PORT))`
        // `except Exception as e: logger.error(e); publish('app_notify', …); Thread(
        //  target=lambda: Setting.stop_service()).start(); return`
        if let Err(e) = receiver.bind(&socket2::SockAddr::from(SocketAddr::from((
            Ipv4Addr::UNSPECIFIED,
            SSDP_PORT,
        )))) {
            self.log.error(&e.to_string());
            self.hooks.notify("Macast", "SSDP Can't start");
            // A thread, not a call: `stop_service()` re-enters `stop()`, which
            // joins the recv thread — and this *is* the recv thread. That is the
            // same reason `stop()` has a self-join guard, and why the Python
            // version names its thread `SSDP_STOP_THREAD`.
            let hooks = Arc::clone(&self.hooks);
            if let Err(te) = std::thread::Builder::new()
                .name("SSDP_STOP_THREAD".to_string())
                .spawn(move || hooks.stop_service())
            {
                self.log.error(&format!("SSDP cannot start stop thread: {}", te));
            }
            return Exit::Held;
        }

        // `self.sock.settimeout(1)`: without it a deliberate `stop()` could only
        // leave the loop when a datagram happened to arrive.
        if let Err(e) = receiver.set_read_timeout(Some(Duration::from_secs(1))) {
            self.log.error(&e.to_string());
            return Exit::Held;
        }

        let ip_list = {
            let mut state = self.state();
            // `self.ip_list = list(Setting.get_ip())`
            state.ip_list = net::advertised_pairs(&self.setting);
            state.senders.clear();
            state.senders_by_ip.clear();
            state.memberships.clear();
            state.ip_list.clone()
        };
        let mut joined_any = false;
        for (ip, _mask) in &ip_list {
            // The netmask is not part of `IP_ADD_MEMBERSHIP`; it belongs to
            // `get_subnet_ip`, which decides which *sender* answers a requester.
            self.log.debug(&format!("add membership {}", ip));
            // `if sys.platform != 'darwin' or not joined_any:` — on macOS the first
            // successful join already covers the group on every interface, and a
            // second `IP_ADD_MEMBERSHIP` for the same group would fail.
            if !(cfg!(target_os = "macos") && joined_any) {
                // `interface_ip = '0.0.0.0' if darwin else ip`
                let interface = if cfg!(target_os = "macos") {
                    Ipv4Addr::UNSPECIFIED
                } else {
                    match ip.parse::<Ipv4Addr>() {
                        Ok(addr) => addr,
                        Err(_) => {
                            // `socket.inet_aton` on a bad string.
                            self.log.error("illegal IP address string passed to inet_aton");
                            continue;
                        }
                    }
                };
                match receiver.join_multicast_v4(&ssdp_group(), &interface) {
                    Ok(()) => {
                        let mut state = self.state();
                        state.memberships.push((ssdp_group(), interface));
                        drop(state);
                        joined_any = true;
                    }
                    // The outer `except Exception: logger.error(e)` — it skips the
                    // sender creation too, because `Sock(ip)` sits inside it.
                    Err(e) => {
                        self.log.error(&e.to_string());
                        continue;
                    }
                }
            }
            // `try: self.sock_list.append(sender = Sock(ip)); self.sock_by_ip[ip] = sender`
            // `except socket.error as se: logger.warning("skip SSDP interface %s: %s", ip, se)`
            match Sock::new(ip, &self.log) {
                Ok(sender) => {
                    let sender = Arc::new(sender);
                    let mut state = self.state();
                    state.senders.push(Arc::clone(&sender));
                    state.senders_by_ip.insert(ip.clone(), sender);
                }
                Err(se) => self.log.warning(&format!("skip SSDP interface {}: {}", ip, se)),
            }
        }

        // `while self.running:` / `try: data, addr = self.sock.recvfrom(1024)` /
        // `except socket.timeout: continue`.
        while self.running.load(Ordering::SeqCst) {
            let mut buf: [MaybeUninit<u8>; 1024] =
                unsafe { MaybeUninit::<[MaybeUninit<u8>; 1024]>::uninit().assume_init() };
            match receiver.recv_from(&mut buf) {
                Ok((len, addr)) => {
                    let data =
                        unsafe { slice::from_raw_parts(buf.as_ptr() as *const u8, len) }.to_vec();
                    let Some(peer) = addr.as_socket() else {
                        continue;
                    };
                    self.datagram_received(&data, peer);
                }
                Err(e) => {
                    match e.kind() {
                        // `socket.timeout`, and PEP 475's `EINTR`.
                        ErrorKind::WouldBlock | ErrorKind::TimedOut | ErrorKind::Interrupted => {
                            continue
                        }
                        _ => {}
                    }
                    // Anything else is Python's unguarded `recvfrom` failure: the
                    // thread dies, `running` stays true, the port stays bound.
                    self.log.error(&e.to_string());
                    return Exit::Held;
                }
            }
        }
        Exit::Cleanup
    }

    /// `shutdown()` — say goodbye to everything we announced, then forget it.
    ///
    /// Python walks `self.known` twice (once for the byebye, once for the
    /// unregister). One snapshot is enough here because `do_byebye` never mutates
    /// the registry; holding the registry lock across `send_it` is not acceptable,
    /// since that is a syscall.
    fn shutdown(&self) {
        let usns: Vec<String> = self.known().iter().map(|e| e.usn.clone()).collect();
        for usn in &usns {
            self.do_byebye(usn);
        }
        for usn in &usns {
            let _ = self.unregister(usn);
        }
    }

    /// `datagram_received(data, (host, port))`.
    ///
    /// The two log lines here are deliberately different shapes: the decode
    /// failure is an ERROR with **no** `host:port` prefix (Python does
    /// `logger.error(e)`), while anything the handler raises is the malformed
    /// warning that names the sender.
    pub fn datagram_received(&self, data: &[u8], peer: SocketAddr) {
        let host = peer.ip().to_string();
        let port = peer.port();
        let decoded = match utf8_decode(data) {
            Ok(text) => text,
            Err(e) => {
                self.log.error(&e);
                return;
            }
        };
        let header = decoded.split("\r\n\r\n").next().unwrap_or("");
        if header.is_empty() {
            return;
        }
        // `except Exception as e:` — one malformed/malicious datagram must never
        // kill the SSDP thread (which would make Macast undiscoverable until
        // restart).
        if let Err(msg) = self.handle_datagram(header, &host, port, data, peer) {
            self.log
                .warning(&format!("malformed SSDP datagram from {}:{}: {}", host, port, msg));
        }
    }

    /// The body of `datagram_received`, in Python's own order, returning what the
    /// outer `except` would have printed.
    fn handle_datagram(
        &self,
        header: &str,
        host: &str,
        port: u16,
        data: &[u8],
        peer: SocketAddr,
    ) -> Result<(), String> {
        // `header.split('\r\n')` — **not** `.lines()`: a `\n`-only datagram must not
        // be split, and the `{}` in a LOCATION placeholder has nothing to do with
        // this either (that gets expanded per sender, in `Sock::send_it`).
        let lines: Vec<&str> = header.split("\r\n").collect();
        let cmd: Vec<&str> = lines[0].split(' ').collect();
        if cmd.len() < 2 {
            return Ok(());
        }
        let mut headers: Vec<(String, String)> = Vec::new();
        for line in &lines[1..] {
            // `x.replace(': ', ':', 1)` — the first occurrence only, so
            // `LOCATION: http://192.168.1.5:8008/description.xml` loses the space
            // after the *name*, not the one after `http:/`.
            let line = line.replacen(": ", ":", 1);
            // `filter(lambda x: len(x) > 0, lines)`
            if line.is_empty() {
                continue;
            }
            let mut parts = line.splitn(2, ':');
            let name = parts.next().unwrap_or("").to_lowercase();
            // `x.split(':', 1)` then `[1]` — a header line with no colon raises
            // `IndexError` in Python, which the outer `except` reports as the
            // malformed warning.
            let value = parts.next().ok_or_else(|| "list index out of range".to_string())?;
            set_header(&mut headers, name, value.to_string());
        }

        if cmd[0] != "NOTIFY" {
            self.log.info(&format!(
                "SSDP command {} {} - from {}:{}",
                cmd[0], cmd[1], host, port
            ));
        }
        if cmd[0] == "M-SEARCH" && cmd[1] == "*" {
            self.log.debug("M-SEARCH *");
            // `logger.debug(data)` — the bytes repr, exactly as CPython prints it.
            self.log.debug(&bytes_repr(data));
            return self.discovery_request(&headers, peer);
        }
        if !(cmd[0] == "NOTIFY" && cmd[1] == "*") {
            self.log
                .warning(&format!("Unknown SSDP command {} {}", cmd[0], cmd[1]));
        }
        Ok(())
    }

    /// `register(usn, st, location, server=SERVER_ID, cache_control='max-age=1800')`.
    ///
    /// Six writes in Python's order, because that order is the response header
    /// order — see [`Entry`].
    pub fn register(&self, usn: &str, st: &str, location: &str, server: &str, cache_control: &str) {
        // `logging.info(...)` — the **root** logger, not the module's. That is not
        // a typo in the Python and it matters here: per-module log splitting
        // (AGENTS.md §4.10b) routes these two lines somewhere a reader will look
        // for them.
        logging::root().info(&format!("Registering {} ({})", st, location));

        let entry = Entry {
            usn: usn.to_string(),
            headers: vec![
                ("USN".to_string(), usn.to_string()),
                ("LOCATION".to_string(), location.to_string()),
                ("ST".to_string(), st.to_string()),
                ("EXT".to_string(), String::new()),
                ("SERVER".to_string(), server.to_string()),
                ("CACHE-CONTROL".to_string(), cache_control.to_string()),
            ],
        };
        let mut known = self.known();
        // `self.known[usn] = {}` then six key writes: re-registering an existing
        // USN replaces the value but keeps the key's position in the dict, so the
        // registry keeps its order here too.
        match known.iter_mut().find(|e| e.usn == usn) {
            Some(slot) => *slot = entry,
            None => known.push(entry),
        }
    }

    /// `register()` with Python's two defaults filled in.
    pub fn register_device(&self, usn: &str, st: &str, location: &str) {
        self.register(usn, st, location, SERVER_ID, DEFAULT_CACHE_CONTROL);
    }

    /// `unregister(usn)` — logs first, then deletes, so a `KeyError` still leaves
    /// the line behind.
    pub fn unregister(&self, usn: &str) -> Result<(), String> {
        self.log.info(&format!("Un-registering {}", usn));
        let mut known = self.known();
        match known.iter().position(|e| e.usn == usn) {
            Some(at) => {
                known.remove(at);
                Ok(())
            }
            None => Err(format!("KeyError({})", py_repr(usn))),
        }
    }

    /// `is_known(usn)`
    pub fn is_known(&self, usn: &str) -> bool {
        self.known().iter().any(|e| e.usn == usn)
    }

    /// `self.known[usn]` as a copy, so no lock is held while a message is built.
    ///
    /// Public because it is the **registry-as-announced** read seam: the device
    /// face needs to be inspected without sending a datagram, and the DLNA
    /// description will want the same answer. Building a message needs a copy
    /// anyway (`do_notify` rewrites `ST` to `NT`), so handing out a clone is not
    /// a leak — handing out a guard while a caller formats bytes would be.
    pub fn entry(&self, usn: &str) -> Option<Entry> {
        self.known().iter().find(|e| e.usn == usn).cloned()
    }

    /// Every announced device, in registration order.
    ///
    /// Order is the point: it is the order [`crate::device::SsdpPlugin`] writes
    /// the six USNs, and a test that only asks "is this one registered" cannot
    /// see a reordering.
    pub fn registry(&self) -> Vec<Entry> {
        self.known().clone()
    }

    /// `send_it(response, destination)` — out through every interface sender.
    pub fn send_it(&self, response: &str, destination: SocketAddr) {
        let senders = self.state().senders.clone();
        // Senders may keep the descriptor alive after `run()` clears the lists
        // (documented in `crate::sock`), so this is outside the lock on purpose.
        for sender in senders {
            sender.send_it(response, destination, &self.log);
        }
    }

    /// `discovery_request(headers, host_port)` — answer an M-SEARCH.
    ///
    /// `Err` is not a failure of the answer: it is the exception Python would have
    /// let escape the handler, which the caller turns into the malformed-datagram
    /// warning. A negative `MX` and a malformed netmask both take that path.
    fn discovery_request(
        &self,
        headers: &[(String, String)],
        destination: SocketAddr,
    ) -> Result<(), String> {
        let host = destination.ip().to_string();
        let port = destination.port();

        // `st = headers.get('st')` / `if not st:` — an empty `ST:` counts as
        // missing, not as a search target.
        let st = header(headers, "st").unwrap_or_default();
        if st.is_empty() {
            self.log
                .warning(&format!("Discovery request from ({},{}) missing ST header", host, port));
            return Ok(());
        }
        self.log.info(&format!(
            "Discovery request from ({},{}) for {}",
            host, port, st
        ));

        let entries = self.known().clone();
        let (ip_list, senders_by_ip) = {
            let state = self.state();
            (state.ip_list.clone(), state.senders_by_ip.clone())
        };
        // `formatdate(timeval=None, localtime=False, usegmt=True)`
        let date = timefmt::rfc1123_gmt(timefmt::now_secs());

        for entry in &entries {
            // `if i['ST'] == st or st == 'ssdp:all'` — the subscript is evaluated
            // first and is unguarded, so an entry without `ST` raises `KeyError`
            // even for a `ssdp:all` search.
            let have = entry
                .header("ST")
                .ok_or_else(|| format!("KeyError({})", py_repr("ST")))?;
            if !(have == st || st == "ssdp:all") {
                continue;
            }
            let Some(response) = search_reply(entry, &date) else {
                // `if usn:` — an absent or empty USN means no DATE, no trailing
                // CRLFs, and no send at all.
                continue;
            };
            // `try: mx = int(headers.get('mx', 0)) except (ValueError, TypeError): mx = 0`
            let mx = py_int(header(headers, "mx").as_deref()).unwrap_or(0);
            // `delay = random.randint(0, mx)` sits **outside** that `try`, so a
            // negative MX is the exception that becomes the malformed warning.
            let delay = py_randrange_inclusive(mx)?;
            self.log.debug(&format!(
                "send discovery response delayed by {}s for {} to {}",
                delay,
                entry.usn,
                tuple_repr(&host, port)
            ));
            // `# asyncio.sleep(delay)` — commented out in the Python: the answer
            // goes out immediately, and the jitter only ever reaches the log line.

            let mut sent = false;
            for (ip, mask) in &ip_list {
                if subnet_and(ip, mask)? == subnet_and(&host, mask)? {
                    if let Some(sender) = senders_by_ip.get(ip) {
                        sender.send_it(&response, destination, &self.log);
                        sent = true;
                    }
                    // The `break` is inside the subnet test and **outside** the
                    // sender test: the first same-subnet interface owns the answer,
                    // even when its sender failed to bind and is not in the map.
                    break;
                }
            }
            if !sent {
                self.log
                    .warning(&format!("No SSDP sender for discovery source {}", host));
            }
        }
        Ok(())
    }

    /// `do_notify(usn)` — the `ssdp:alive` broadcast, sent **twice** to the group.
    ///
    /// Python's `except (AttributeError, socket.error)` around the two sends is
    /// dead code in Rust: [`crate::sock::Sock::send_it`] swallows every send error
    /// and logs it itself. The `KeyError('ST')` from the NT rewrite is **not**
    /// caught here or by its caller (`SSDPPlugin.notify()` has no `try` either), so
    /// it is returned for the caller to deal with.
    pub fn do_notify(&self, usn: &str) -> Result<(), String> {
        self.log
            .debug(&format!("Sending alive notification for {}", usn));
        // The debug line comes first in Python, before the membership test.
        let Some(entry) = self.entry(usn) else {
            return Ok(());
        };
        let fields = entry
            .nt_form()
            .map_err(|missing| format!("KeyError({})", py_repr(&missing)))?;
        let message = notify_message("ssdp:alive", &fields);
        let group = group_destination();
        self.send_it(&message, group);
        self.send_it(&message, group);
        Ok(())
    }

    /// `do_byebye(usn)`
    ///
    /// Two different failure messages in Python, and both are kept: the build
    /// failure (`except KeyError`) and the send failure. The send happens **once**,
    /// unlike [`SsdpServer::do_notify`].
    pub fn do_byebye(&self, usn: &str) {
        if !self.sending_byebye.load(Ordering::SeqCst) {
            return;
        }
        self.log
            .info(&format!("Sending byebye notification for {}", usn));
        let message = match self.byebye_message(usn) {
            Ok(message) => message,
            Err(msg) => {
                self.log
                    .error(&format!("error building byebye notification: {}", msg));
                return;
            }
        };
        // `if self.sock:` — true even on the bind-failure path, because `run()`
        // stores the receiver before touching it.
        if self.state().receiver.is_some() {
            self.send_it(&message, group_destination());
        }
    }

    /// The bytes of a byebye, or Python's `KeyError` as `%r` would print it.
    ///
    /// A missing entry and a missing `ST` both land in the same `except KeyError`
    /// there, so they get the same shape here.
    fn byebye_message(&self, usn: &str) -> Result<String, String> {
        let entry = self
            .entry(usn)
            .ok_or_else(|| format!("KeyError({})", py_repr(usn)))?;
        let fields = entry
            .nt_form()
            .map_err(|missing| format!("KeyError({})", py_repr(&missing)))?;
        Ok(notify_message("ssdp:byebye", &fields))
    }
}

/// `cache_control='max-age=1800'` — Python's default argument, kept as a constant
/// so the two callers that use the default cannot drift apart.
pub const DEFAULT_CACHE_CONTROL: &str = "max-age=1800";

impl Entry {
    /// `known[usn][name]` for the header names Python subscripts directly
    /// (`ST`, `USN`), as an `Option` so the caller can choose between Python's
    /// `KeyError` and its falsy `if usn:`.
    fn header(&self, name: &str) -> Option<String> {
        self.headers
            .iter()
            .find(|(k, _)| k == name)
            .map(|(_, v)| v.clone())
    }
}

/// `response = ['HTTP/1.1 200 OK'] … .append each header … .append('DATE: …')
///  … .extend(('', ''))` then `'\r\n'.join(response)`.
///
/// `None` is Python's `if usn:` test: no USN (or an empty one, which is falsy) and
/// this reply is never built, never dated, and never sent.
fn search_reply(entry: &Entry, date: &str) -> Option<String> {
    let usn = entry.header("USN")?;
    if usn.is_empty() {
        return None;
    }
    let mut out = String::from("HTTP/1.1 200 OK");
    for (k, v) in &entry.headers {
        out.push_str("\r\n");
        out.push_str(k);
        out.push_str(": ");
        out.push_str(v);
    }
    out.push_str(&format!("\r\nDATE: {}\r\n\r\n", date));
    Some(out)
}

/// `['NOTIFY * HTTP/1.1', 'HOST: …', 'NTS: …'] + fields + ['', '']`.
///
/// Note the `': '.join` in Python: an `EXT` whose value is `''` is written as
/// `EXT: ` **with** its trailing space, and this does the same.
fn notify_message(nts: &str, fields: &[(String, String)]) -> String {
    let mut out = format!(
        "NOTIFY * HTTP/1.1\r\nHOST: {}:{}\r\nNTS: {}",
        SSDP_ADDR, SSDP_PORT, nts
    );
    for (k, v) in fields {
        out.push_str("\r\n");
        out.push_str(k);
        out.push_str(": ");
        out.push_str(v);
    }
    out.push_str("\r\n\r\n");
    out
}

/// `headers.get(name)` over a dict whose keys are already lower-cased.
fn header(headers: &[(String, String)], name: &str) -> Option<String> {
    headers
        .iter()
        .find(|(k, _)| k == name)
        .map(|(_, v)| v.clone())
}

/// `headers[name] = value` — a repeat of a key overwrites the value **in place**.
fn set_header(headers: &mut Vec<(String, String)>, name: String, value: String) {
    match headers.iter_mut().find(|(k, _)| *k == name) {
        Some(slot) => slot.1 = value,
        None => headers.push((name, value)),
    }
}

/// `repr(str)` as CPython prints it, for the `%r` in the two error lines and for
/// the `(host, port)` tuple in the debug line.
///
/// The quote choice flips to `"` only when the text contains `'` and no `"`; the
/// escapes are `\\`, `\n`, `\r`, `\t`, the active quote, `\xNN` below U+0100, and
/// `\uXXXX`/`\UXXXXXXXX` above.
fn py_repr(text: &str) -> String {
    let quote = if text.contains('\'') && !text.contains('"') {
        '"'
    } else {
        '\''
    };
    let mut out = String::new();
    out.push(quote);
    for c in text.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if c == quote => {
                out.push('\\');
                out.push(c);
            }
            c if is_printable(c) => out.push(c),
            c => {
                let code = c as u32;
                if code < 0x100 {
                    out.push_str(&format!("\\x{:02x}", code));
                } else if code < 0x10000 {
                    out.push_str(&format!("\\u{:04x}", code));
                } else {
                    out.push_str(&format!("\\U{:08x}", code));
                }
            }
        }
    }
    out.push(quote);
    out
}

/// Python's `str.isprintable()` for the repr above, with one documented
/// divergence: CPython consults `unicodedata` for the non-ASCII categories, and
/// this treats every other non-whitespace scalar value as printable. Nothing in
/// this crate can tell the two apart, because every string it reprs here is an
/// ASCII USN, header name, host, or address.
fn is_printable(c: char) -> bool {
    let code = c as u32;
    if code < 0x20 || code == 0x7f {
        return false;
    }
    if code < 0x7f {
        return true;
    }
    if (0x80..=0x9f).contains(&code) || code == 0xa0 {
        return false;
    }
    !c.is_whitespace()
}

/// `repr((host, port))` — the tuple the debug line ends with.
fn tuple_repr(host: &str, port: u16) -> String {
    format!("({}, {})", py_repr(host), port)
}

/// `repr(data)` for the `bytes` object Python logs after `M-SEARCH *`.
fn bytes_repr(data: &[u8]) -> String {
    let quote = if data.contains(&b'\'') && !data.contains(&b'"') {
        b'"'
    } else {
        b'\''
    };
    let mut out = String::from("b");
    out.push(quote as char);
    for &b in data {
        match b {
            b'\\' => out.push_str("\\\\"),
            b'\t' => out.push_str("\\t"),
            b'\n' => out.push_str("\\n"),
            b'\r' => out.push_str("\\r"),
            b if b == quote => {
                out.push('\\');
                out.push(b as char);
            }
            0x20..=0x7e => out.push(b as char),
            _ => out.push_str(&format!("\\x{:02x}", b)),
        }
    }
    out.push(quote as char);
    out
}

/// `data.decode()` — UTF-8 strict, and the CPython error **text**, because that
/// text is what `logger.error(e)` puts in the log.
fn utf8_decode(data: &[u8]) -> Result<String, String> {
    match std::str::from_utf8(data) {
        Ok(text) => Ok(text.to_string()),
        Err(e) => Err(unicode_error(data, e.valid_up_to())),
    }
}

/// `UnicodeDecodeError`'s `str()` for a bad UTF-8 sequence:
/// `'utf-8' codec can't decode byte 0x.. in position N: <reason>`, or the plural
/// `bytes in position N-M: <reason>` when more than one byte of the sequence was
/// consumed before it went wrong.
///
/// The three reasons are chosen the way CPython's decoder does it: an unusable
/// lead byte is `invalid start byte`, a sequence that reaches the end of the
/// buffer is `unexpected end of data`, and a well-formed lead followed by a bad
/// continuation is `invalid continuation byte`. Measured against this machine's
/// 3.12 rather than reasoned out; the cases are in the tests.
fn unicode_error(data: &[u8], p: usize) -> String {
    let lead = data[p];
    let valid_lead = (0xc2..=0xf4).contains(&lead);
    let need = if valid_lead { lead_len(lead) } else { 1 };
    let mut consumed = 1usize;
    while consumed < need {
        match data.get(p + consumed) {
            Some(&b) if continuation_ok(lead, consumed, b) => consumed += 1,
            Some(_) => break,
            None => break,
        }
    }
    let ran_out = p + consumed >= data.len();
    let reason = if !valid_lead {
        "invalid start byte"
    } else if ran_out {
        "unexpected end of data"
    } else {
        "invalid continuation byte"
    };
    let head = if consumed == 1 {
        format!("byte 0x{:02x} in position {}", lead, p)
    } else {
        format!("bytes in position {}-{}", p, p + consumed - 1)
    };
    format!("'utf-8' codec can't decode {}: {}", head, reason)
}

/// The number of bytes a UTF-8 lead byte promises.
fn lead_len(lead: u8) -> usize {
    match lead {
        0x00..=0x7f => 1,
        0xc0..=0xdf => 2,
        0xe0..=0xef => 3,
        _ => 4,
    }
}

/// Whether the byte at `index` inside a sequence is acceptable for that lead —
/// including the three ranges Python's decoder rejects to keep overlong encodings
/// and surrogates out (0xE0 must not take 0x80-0x9F, 0xED must not take 0xA0-0xBF,
/// 0xF0 must not take 0x80-0x8F, 0xF4 must not take 0x90-0xBF).
fn continuation_ok(lead: u8, index: usize, b: u8) -> bool {
    if index != 1 {
        return (0x80..=0xbf).contains(&b);
    }
    let lo = match lead {
        0xe0 => 0xa0,
        0xed => 0x80,
        0xf0 => 0x90,
        0xf4 => 0x80,
        _ => 0x80,
    };
    let hi = match lead {
        0xe0 => 0xbf,
        0xed => 0x9f,
        0xf0 => 0xbf,
        0xf4 => 0x8f,
        _ => 0xbf,
    };
    (lo..=hi).contains(&b)
}

/// `int(text)` for the `MX` header: surrounding whitespace and one leading sign
/// are allowed, and nothing else is. `None` is Python's `ValueError`, which the
/// caller catches and answers with `0`.
///
/// Overflow is the one divergence: Python grows an `int` forever, this saturates.
/// The value only ever feeds `random.randint(0, mx)`, and a `MX` big enough to
/// saturate has already broken the sender that sent it. Measured against this
/// machine's 3.12: `"-9223372036854775808"` is exact here (`i64::MIN`) because
/// `digits` is sign-stripped and all-ASCII, so the multiply below can only ever
/// reach `1 * i64::MAX` or `-1 * i64::MAX` and never overflows; saturation starts
/// one digit beyond either bound (`9223372036854775808` -> `i64::MAX`,
/// `-9223372036854775809` -> `i64::MIN`) where Python would keep growing.
///
/// The second divergence is underscore separators: CPython accepts `int("1_0")` as
/// `10`, this reads it as `None` and the caller answers `0`. A sender that puts a
/// digit separator in `MX` has already left the RFC, and the only thing the answer
/// reaches is one debug line, so this stays the simpler reader.
fn py_int(text: Option<&str>) -> Option<i64> {
    let t = text?.trim();
    let (sign, digits) = match t.as_bytes().first() {
        Some(b'+') => (1i64, &t[1..]),
        Some(b'-') => (-1i64, &t[1..]),
        _ => (1i64, t),
    };
    if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    digits.parse::<i64>().map(|v| sign * v).ok().or(Some(
        if sign < 0 { i64::MIN } else { i64::MAX },
    ))
}

/// `[int(n) for n in text.split('.')]` — every part is parsed before any of them
/// is indexed, so a non-numeric part raises Python's `ValueError` text and a short
/// address raises the `IndexError` from the loop instead.
fn octets(text: &str) -> Result<Vec<i64>, String> {
    let mut out = Vec::new();
    for part in text.split('.') {
        match part.trim().parse::<i64>() {
            Ok(v) => out.push(v),
            Err(_) => {
                return Err(format!(
                    "invalid literal for int() with base 10: {}",
                    py_repr(part)
                ))
            }
        }
    }
    Ok(out)
}

/// `get_subnet_ip(ip, mask)`: `mask` supplies `a` and `ip` supplies `b`, and the
/// four indexes are taken from the **mask's** length — so a mask with fewer than
/// four parts is the `IndexError`, while more parts than that simply go unread on
/// both sides.
fn subnet_and(ip: &str, mask: &str) -> Result<Vec<i64>, String> {
    let a = octets(mask)?;
    let b = octets(ip)?;
    let mut out = Vec::with_capacity(4);
    for i in 0..4 {
        let x = a.get(i).ok_or_else(|| "list index out of range".to_string())?;
        let y = b.get(i).ok_or_else(|| "list index out of range".to_string())?;
        out.push(x & y);
    }
    Ok(out)
}

/// `random.randint(0, mx)`, including 3.12's own wording for the empty range.
///
/// Measured on this machine's interpreter: `randint(0, -1)` raises
/// `ValueError: empty range in randrange(0, 0)` and `randint(0, -5)` raises
/// `empty range in randrange(0, -4)` — the stop is `mx + 1` on both sides of the
/// message.
fn py_randrange_inclusive(mx: i64) -> Result<i64, String> {
    let high = mx.saturating_add(1);
    if high <= 0 {
        return Err(format!("empty range in randrange(0, {})", high));
    }
    if high == 1 {
        return Ok(0);
    }
    Ok((rng_next() % high as u64) as i64)
}

/// `random`'s state is not reproducible here and does not need to be: the value
/// this produces goes to one debug line, because Python never actually sleeps for
/// it (`# asyncio.sleep(delay)`). A self-contained xorshift64\* is enough, and
/// nothing in this crate treats it as secret.
static RNG_STATE: AtomicU64 = AtomicU64::new(0);

fn rng_next() -> u64 {
    let mut current = RNG_STATE.load(Ordering::Relaxed);
    if current == 0 {
        let seeded = seed_state();
        let _ = RNG_STATE.compare_exchange(0, seeded, Ordering::Relaxed, Ordering::Relaxed);
        current = RNG_STATE.load(Ordering::Relaxed);
    }
    loop {
        let mut x = current;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        let out = x.wrapping_mul(0x2545_F491_4F6C_DD1D);
        match RNG_STATE.compare_exchange_weak(
            current,
            x,
            Ordering::Relaxed,
            Ordering::Relaxed,
        ) {
            Ok(_) => return out,
            Err(next) => current = next,
        }
    }
}

/// A nonzero seed from the clock and the address of the static — never `0`, which
/// the xorshift above would get stuck on.
fn seed_state() -> u64 {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0x9E37_79B9_7F4A_7C15);
    let addr = &RNG_STATE as *const AtomicU64 as u64;
    let mixed = nanos ^ addr.rotate_left(17);
    if mixed == 0 {
        0x2545_F491_4F6C_DD1D
    } else {
        mixed
    }
}

// ---------------------------------------------------------------------------
// Tests.
//
// Two rules govern everything below, and both come from how this module has to
// live:
//
//  * **No `start()`, no `Sock`.** `start()` binds UDP/1900 (the user's SSDP
//    port, and on this machine the running Python Macast already holds it), and
//    every path that could send installs a real `Sock` whose `send_it` puts a
//    multicast datagram on the user's LAN. So the receive face is driven by
//    calling `handle_datagram` / `datagram_received` / `discovery_request`
//    directly, with `state.ip_list` written by hand and **zero senders**
//    installed. That turns "did we answer?" into "did we log the refusal to
//    answer?", which is exactly the branch that is ours to test.
//  * **The witness is the log file, not a string we were handed.** Every
//    message asserted here is read back from `paths::log_file()` through
//    `macast_core::logging::read_tail`, because these functions return nothing
//    for the cases that matter most (silence is a behaviour).
//
// Absence claims therefore need a token nothing else can produce: hosts are
// TEST-NET-3 (`203.0.113.0/24`, RFC 5737 — not routable, not this LAN) and
// peers carry ports in the 419xx/420xx range that no real client of this
// machine uses. Each claim of "no line" is paired with a positive control.
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::Hooks;
    use macast_core::paths;

    /// A `Hooks` that answers nothing, so a bind failure or a stop that should
    /// not happen cannot reach the real service. Same shape as `device.rs`.
    struct NullHooks;

    impl Hooks for NullHooks {
        fn notify(&self, _title: &str, _text: &str) {}
        fn stop_service(&self) {}
    }

    /// A settings file that **does not exist**, named for this process and this
    /// test. Nothing here calls `save()` — registration is memory only — so
    /// AGENTS.md §10 ("不要为了验证去改用户的真实配置") cannot be violated from
    /// this module, and no file is left behind to clean up.
    fn server(name: &str) -> Arc<SsdpServer> {
        let path = std::env::temp_dir().join(format!(
            "macast-upnp-ssdp-{}-{}.json",
            std::process::id(),
            name
        ));
        let hooks: SharedHooks = Arc::new(NullHooks);
        SsdpServer::new(hooks, Arc::new(Setting::new(path)))
    }

    /// Every log line mentioning `needle`, most recent file read first.
    fn logged(needle: &str) -> Vec<String> {
        logging::read_tail(&paths::log_file(), 4000)
            .text
            .lines()
            .filter(|line| line.contains(needle))
            .map(|line| line.to_string())
            .collect()
    }

    fn hs(pairs: &[(&str, &str)]) -> Vec<(String, String)> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    fn entry(usn: &str, headers: Vec<(String, String)>) -> Entry {
        Entry {
            usn: usn.to_string(),
            headers,
        }
    }

    /// `RNG_STATE` is one global for the whole binary and the harness runs tests
    /// on threads, so the two tests that ask "did this touch the RNG?" hold a
    /// lock rather than race.
    static RNG_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    fn rng_guard() -> std::sync::MutexGuard<'static, ()> {
        match RNG_LOCK.lock() {
            Ok(g) => g,
            Err(e) => e.into_inner(),
        }
    }

    // -- the harness's own contract -----------------------------------------
    //
    // These two ask about the *test binary*, not the product. They exist because
    // the log is the witness for ~40 of the cases below, and a witness read from
    // a file that outlives the run is not a witness. See `crate::pin_test_dir`.

    #[test]
    fn the_suite_logs_into_its_own_directory_not_the_users() {
        crate::pin_test_dir();
        let dir = paths::setting_dir();
        let log = paths::log_file();
        assert_eq!(log, dir.join(paths::LOG_FILE_NAME));
        // The pinned directory carries this process's id, so a second `cargo test`
        // cannot read the first one's lines.
        let name = dir
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_default();
        assert!(
            name.ends_with(&format!("-{}", std::process::id())),
            "the test config dir is not per-process: {}",
            dir.display()
        );
        assert!(
            dir.starts_with(std::env::temp_dir()),
            "the test config dir is not under the temp dir: {}",
            dir.display()
        );
        // The half that only this machine can answer. On macOS the dependency's
        // `#[cfg(test)]` fallback is compiled out for us, so if the pin had not
        // been installed before the first log record, this is the path the whole
        // run would have written into — AGENTS.md §10.
        #[cfg(target_os = "macos")]
        if let Some(home) = std::env::var_os("HOME") {
            let real = std::path::PathBuf::from(home)
                .join("Library")
                .join("Application Support")
                .join("Macast")
                .join(paths::LOG_FILE_NAME);
            assert_ne!(log, real, "the suite is writing the user's real macast.log");
        }
    }

    #[test]
    fn every_test_pins_the_log_directory_first() {
        crate::pin_test_dir();
        // `pin_test_dir()` only protects the run if it runs before **any** log
        // record, and the only way to keep that true as tests are added is to ask
        // the files. A scan that found nothing would agree with anything, so the
        // count is asserted too.
        let src = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let mut files: Vec<_> = std::fs::read_dir(&src)
            .unwrap_or_else(|e| panic!("read {}: {e}", src.display()))
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.extension().map(|x| x == "rs").unwrap_or(false))
            .collect();
        files.sort();
        let mut scanned = 0usize;
        let mut unpinned: Vec<String> = Vec::new();
        for path in &files {
            let text = std::fs::read_to_string(path).unwrap();
            let lines: Vec<&str> = text.split('\n').collect();
            for (i, line) in lines.iter().enumerate() {
                if line.trim() != "#[test]" {
                    continue;
                }
                let mut j = i + 1;
                while j < lines.len()
                    && (lines[j].trim().is_empty()
                        || lines[j].trim_start().starts_with("//")
                        || lines[j].trim_start().starts_with('#'))
                {
                    j += 1;
                }
                let sig = lines.get(j).copied().unwrap_or("");
                assert!(
                    sig.trim().starts_with("fn ") && sig.trim().ends_with('{'),
                    "{}:{}: `#[test]` is not followed by a one-line signature ({sig:?}); \
                     this scan would silently skip the test",
                    path.display(),
                    j + 1
                );
                scanned += 1;
                let mut k = j + 1;
                while k < lines.len() && lines[k].trim().is_empty() {
                    k += 1;
                }
                if lines.get(k).map(|l| l.trim()) != Some("crate::pin_test_dir();") {
                    unpinned.push(format!("{}:{}", path.display(), k + 1));
                }
            }
        }
        assert!(
            scanned >= 60,
            "the scan only saw {scanned} tests across {:?} — the audit itself went quiet",
            files.iter().map(|p| p.file_name().map(|n| n.to_string_lossy().to_string())).collect::<Vec<_>>()
        );
        assert!(unpinned.is_empty(), "tests that do not pin the log dir first: {unpinned:?}");
    }

    // -- what goes on the wire ----------------------------------------------

    #[test]
    fn the_multicast_group_and_its_destination_agree() {
        crate::pin_test_dir();
        assert_eq!(ssdp_group(), Ipv4Addr::new(239, 255, 255, 250));
        assert_eq!(
            format!("{}", group_destination()),
            format!("{}:{}", SSDP_ADDR, SSDP_PORT)
        );
    }

    #[test]
    fn nt_form_copies_the_other_headers_then_appends_nt() {
        crate::pin_test_dir();
        // `NT` lands last because Python's `nt_form` deletes `ST` — which leaves
        // the dict slot alone — and then assigns a fresh key. Insertion order is
        // the wire format, so this is a formatting test, not tidiness.
        let e = entry(
            "u",
            vec![
                ("USN".into(), "u".into()),
                ("ST".into(), "urn:st".into()),
                ("LOCATION".into(), "http://h/l".into()),
                ("CACHE-CONTROL".into(), DEFAULT_CACHE_CONTROL.into()),
            ],
        );
        assert_eq!(
            e.nt_form().unwrap(),
            vec![
                ("USN".to_string(), "u".to_string()),
                ("LOCATION".to_string(), "http://h/l".to_string()),
                ("CACHE-CONTROL".to_string(), DEFAULT_CACHE_CONTROL.to_string()),
                ("NT".to_string(), "urn:st".to_string()),
            ]
        );
    }

    #[test]
    fn nt_form_names_the_missing_key_and_nothing_else() {
        crate::pin_test_dir();
        let e = entry("u", vec![("USN".into(), "u".into())]);
        assert_eq!(e.nt_form().unwrap_err(), "ST");
    }

    #[test]
    fn search_reply_repeats_every_header_then_dates_then_closes() {
        crate::pin_test_dir();
        let e = entry(
            "u",
            vec![
                ("USN".into(), "u".into()),
                ("CACHE-CONTROL".into(), "max-age=1800".into()),
                ("ST".into(), "urn:st".into()),
                ("EXT".into(), String::new()),
            ],
        );
        let reply = search_reply(&e, "Thu, 08 Oct 2026 00:00:00 GMT").unwrap();
        assert!(reply.starts_with("HTTP/1.1 200 OK\r\n"));
        // A valueless header still gets its colon and its space: "EXT: ".
        assert!(reply.contains("\r\nEXT: \r\n"));
        assert!(reply.ends_with("\r\n\r\n"));
        assert!(reply.contains("\r\nDATE: Thu, 08 Oct 2026 00:00:00 GMT\r\n\r\n"));
        // `response.extend(('', ''))` then `'\r\n'.join(response)`: the status line,
        // the four headers, the DATE, and **two** empty pieces — so a split yields
        // 8, not 7. The trailing `\r\n\r\n` that `ends_with` above checks is one of
        // those two, and reading it as one would make this count look wrong.
        assert_eq!(reply.split("\r\n").count(), 8);
    }

    #[test]
    fn search_reply_refuses_both_kinds_of_no_usn() {
        crate::pin_test_dir();
        // `if usn:` in Python is falsy for absent **and** for empty, and both
        // mean "no reply at all" — not a reply with a blank USN line.
        let absent = entry("u", vec![("ST".into(), "urn:st".into())]);
        assert!(search_reply(&absent, "d").is_none());
        let empty = entry("u", vec![("USN".into(), String::new())]);
        assert!(search_reply(&empty, "d").is_none());
    }

    #[test]
    fn notify_message_opens_with_the_group_host_and_the_nts() {
        crate::pin_test_dir();
        let message = notify_message("ssdp:alive", &[("NT".into(), "urn:st".into())]);
        assert!(message.starts_with(&format!(
            "NOTIFY * HTTP/1.1\r\nHOST: {}:{}\r\nNTS: ssdp:alive\r\n",
            SSDP_ADDR, SSDP_PORT
        )));
        assert!(message.ends_with("\r\nNT: urn:st\r\n\r\n"));
    }

    #[test]
    fn set_header_overwrites_in_place_and_appends_when_absent() {
        crate::pin_test_dir();
        let mut h = hs(&[("a", "1"), ("b", "2")]);
        set_header(&mut h, "a".to_string(), "9".to_string());
        assert_eq!(h, hs(&[("a", "9"), ("b", "2")]));
        set_header(&mut h, "c".to_string(), "3".to_string());
        assert_eq!(h.len(), 3);
        assert_eq!(header(&h, "c").as_deref(), Some("3"));
        // Keys are matched exactly; the handler lower-cases on the way in.
        assert!(header(&h, "A").is_none());
    }

    // -- CPython's shapes, spelled the way a witness would spell them --------

    #[test]
    fn py_repr_prefers_single_quotes() {
        crate::pin_test_dir();
        assert_eq!(py_repr("plain"), "'plain'");
        assert_eq!(py_repr("Macast"), "'Macast'");
    }

    #[test]
    fn py_repr_switches_quote_only_when_the_other_kind_is_alone() {
        crate::pin_test_dir();
        assert_eq!(py_repr("it's"), "\"it's\"");
        assert_eq!(py_repr("say \"hi\" it's me"), "'say \"hi\" it\\'s me'");
    }

    #[test]
    fn py_repr_escapes_the_backslash_and_only_the_active_quote() {
        crate::pin_test_dir();
        assert_eq!(py_repr("a\\b"), "'a\\\\b'");
    }

    #[test]
    fn py_repr_uses_the_narrowest_escape_cpython_uses() {
        crate::pin_test_dir();
        assert_eq!(py_repr("\u{1}"), "'\\x01'");
        assert_eq!(py_repr("\u{a0}"), "'\\xa0'");
        assert_eq!(py_repr("\u{1680}"), "'\\u1680'");
        // No `\U........` arm is reachable: every astral code point passes
        // `is_printable` here, so nothing can ask for the eight-digit spelling.
        // It exists to mirror CPython, and this comment is the only witness.
        assert!(py_repr("🙂").contains('🙂'));
    }

    #[test]
    fn is_printable_is_cpythons_not_rusts() {
        crate::pin_test_dir();
        assert!(!is_printable('\u{1}'));
        // Space is printable. Rust's `char::is_whitespace` would say otherwise,
        // which is why the ASCII cut here is `code < 0x20` and not "not a space".
        assert!(is_printable(' '));
        assert!(!is_printable('\u{1f}'));
        assert!(!is_printable('\u{7f}'));
        // The C1 controls and NBSP are the second family CPython excludes and
        // `is_whitespace` does not cover on its own.
        assert!(!is_printable('\u{80}'));
        assert!(!is_printable('\u{9f}'));
        assert!(!is_printable('\u{85}'));
        assert!(!is_printable('\u{a0}'));
        // Everything after that is whitespace-or-not, which is where Rust agrees
        // with CPython for the separators Python really does exclude.
        assert!(!is_printable('\u{2028}'));
        assert!(!is_printable('\u{3000}'));
        // And this is the documented divergence from the function's own doc
        // comment: U+E000 is category Cc, so CPython says not printable while we
        // say printable. Nothing in this crate ever reprs a private-use scalar,
        // so the divergence is stated rather than hidden.
        assert!(is_printable('\u{e000}'));
    }
    #[test]
    fn bytes_repr_is_the_python_literal_for_the_payload() {
        crate::pin_test_dir();
        // `logger.debug(data)` after `M-SEARCH *` prints a bytes literal, and the
        // escapes are the four CPython picks for bytes plus `\xNN` for everything
        // outside the printable ASCII band.
        assert_eq!(bytes_repr(b"a\tb\nc\x01"), "b'a\\tb\\nc\\x01'");
        assert_eq!(bytes_repr(b"a\\b"), "b'a\\\\b'");
        assert_eq!(bytes_repr(b"\x7f"), "b'\\x7f'");
        assert_eq!(bytes_repr(b"\xff"), "b'\\xff'");
        assert_eq!(bytes_repr(b""), "b''");
        // 0x20..=0x7e is literal, so a real datagram prints as itself.
        assert_eq!(
            bytes_repr(b"M-SEARCH * HTTP/1.1\r\n"),
            "b'M-SEARCH * HTTP/1.1\\r\\n'"
        );
    }

    #[test]
    fn bytes_repr_switches_quote_the_way_str_repr_does() {
        crate::pin_test_dir();
        // Same flip, same "only when the other kind is absent" condition, because
        // CPython builds both with one routine.
        assert_eq!(bytes_repr(b"'"), "b\"'\"");
        assert_eq!(bytes_repr(b"it's"), "b\"it's\"");
        assert_eq!(bytes_repr(b"say \"hi\" it's me"), "b'say \"hi\" it\\'s me'");
        // Both kinds present keeps the single quote and escapes only that one.
        assert!(bytes_repr(b"'\"").starts_with("b'\\'\""));
    }

    #[test]
    fn tuple_repr_has_the_space_the_warning_does_not() {
        crate::pin_test_dir();
        // `repr((host, port))` puts a space after the comma. The ST warning spells
        // the same pair `"({},{})"` with no space, and both shapes are in the log,
        // so a reader cannot tell them apart by pattern-matching on this line.
        assert_eq!(tuple_repr("203.0.113.7", 1900), "('203.0.113.7', 1900)");
        assert_eq!(tuple_repr("it's", 8008), "(\"it's\", 8008)");
        assert!(tuple_repr("203.0.113.7", 1900).contains(", 1900"));
    }

    #[test]
    fn utf8_decode_passes_ascii_through_and_keeps_the_text() {
        crate::pin_test_dir();
        assert_eq!(
            utf8_decode(b"NOTIFY * HTTP/1.1\r\n").unwrap(),
            "NOTIFY * HTTP/1.1\r\n"
        );
        // Multibyte is not a problem: this is strict UTF-8, not latin-1.
        assert_eq!(utf8_decode("ST: 影片\r\n".as_bytes()).unwrap(), "ST: 影片\r\n");
    }

    #[test]
    fn unicode_error_names_an_unusable_lead_as_a_start_byte() {
        crate::pin_test_dir();
        assert_eq!(
            utf8_decode(b"\xff").unwrap_err(),
            "'utf-8' codec can't decode byte 0xff in position 0: invalid start byte"
        );
        assert_eq!(
            utf8_decode(b"a\x80").unwrap_err(),
            "'utf-8' codec can't decode byte 0x80 in position 1: invalid start byte"
        );
        // An overlong two-byte lead is a start byte too: 0xC0 is not a valid lead
        // even though `lead_len` would happily promise two bytes.
        assert_eq!(
            utf8_decode(b"\xc0\x80").unwrap_err(),
            "'utf-8' codec can't decode byte 0xc0 in position 0: invalid start byte"
        );
    }

    #[test]
    fn unicode_error_says_end_of_data_only_when_the_buffer_ran_out() {
        crate::pin_test_dir();
        assert_eq!(
            utf8_decode(b"a\xe0\xa0").unwrap_err(),
            "'utf-8' codec can't decode bytes in position 1-2: unexpected end of data"
        );
        assert_eq!(
            utf8_decode(b"\xf0\x90").unwrap_err(),
            "'utf-8' codec can't decode bytes in position 0-1: unexpected end of data"
        );
    }

    #[test]
    fn unicode_error_names_the_surrogate_as_a_continuation_byte() {
        crate::pin_test_dir();
        // A well-formed lead followed by a byte the range table refuses. U+D800
        // (`ED A0 80`) is the case that makes the 0xED arm exist.
        assert_eq!(
            utf8_decode(b"\xed\xa0\x80").unwrap_err(),
            "'utf-8' codec can't decode byte 0xed in position 0: invalid continuation byte"
        );
        assert_eq!(
            utf8_decode(b"a\xe0\x80").unwrap_err(),
            "'utf-8' codec can't decode byte 0xe0 in position 1: invalid continuation byte"
        );
        assert_eq!(
            utf8_decode(b"\xf0\x80\x80\x80").unwrap_err(),
            "'utf-8' codec can't decode byte 0xf0 in position 0: invalid continuation byte"
        );
    }

    #[test]
    fn lead_len_promises_the_width_of_the_sequence() {
        crate::pin_test_dir();
        assert_eq!(lead_len(0x00), 1);
        assert_eq!(lead_len(0x7f), 1);
        assert_eq!(lead_len(0xc0), 2);
        assert_eq!(lead_len(0xdf), 2);
        assert_eq!(lead_len(0xe0), 3);
        assert_eq!(lead_len(0xef), 3);
        assert_eq!(lead_len(0xf0), 4);
        assert_eq!(lead_len(0xff), 4);
        // 0xf5..0xff answer 4 and are still refused upstream: `valid_lead` stops at
        // 0xf4, so `unicode_error` never asks this what a 0xff sequence looks like.
        assert_eq!(lead_len(0xf5), 4);
    }

    #[test]
    fn continuation_ok_widens_after_the_first_byte() {
        crate::pin_test_dir();
        // Only index 1 is special; indexes 2 and 3 take any continuation byte.
        assert!(continuation_ok(0xe0, 1, 0xa0));
        assert!(!continuation_ok(0xe0, 1, 0x9f));
        assert!(continuation_ok(0xed, 1, 0x9f));
        assert!(!continuation_ok(0xed, 1, 0xa0));
        assert!(continuation_ok(0xf0, 1, 0x90));
        assert!(!continuation_ok(0xf0, 1, 0x8f));
        assert!(continuation_ok(0xf4, 1, 0x8f));
        assert!(!continuation_ok(0xf4, 1, 0x90));
        // A plain three-byte lead takes the whole continuation band.
        assert!(continuation_ok(0xe2, 1, 0x80));
        assert!(continuation_ok(0xe0, 2, 0x80));
        assert!(continuation_ok(0xf0, 2, 0x90));
        assert!(!continuation_ok(0xf0, 3, 0xc0));
    }

    // -- numeric readers ----------------------------------------------------
    //
    // These four helpers are the whole distance between "an M-SEARCH arrived" and
    // "we slept for a random while". None of them can send anything, so they get
    // read directly rather than through a handler.

    #[test]
    fn py_int_reads_what_python_int_reads() {
        crate::pin_test_dir();
        assert_eq!(py_int(None), None);
        assert_eq!(py_int(Some("")), None);
        assert_eq!(py_int(Some("   ")), None);
        assert_eq!(py_int(Some("+")), None);
        assert_eq!(py_int(Some("-")), None);
        assert_eq!(py_int(Some("12x")), None);
        assert_eq!(py_int(Some("x12")), None);
        assert_eq!(py_int(Some("1 2")), None);
        assert_eq!(py_int(Some("0012")), Some(12));
        assert_eq!(py_int(Some("  42 ")), Some(42));
        assert_eq!(py_int(Some("+7")), Some(7));
        assert_eq!(py_int(Some("-7")), Some(-7));
        assert_eq!(py_int(Some("0")), Some(0));
        // CPython accepts `int("1_0")` as 10; we answer None and the caller
        // substitutes 0. Documented above, asserted here so it stays a choice.
        assert_eq!(py_int(Some("1_0")), None);
    }

    #[test]
    fn py_int_saturates_where_python_would_just_grow() {
        crate::pin_test_dir();
        // Python's int is unbounded; i64 is not, and the only reader of this value
        // is a randrange call, so the far end of the range is the safe side.
        assert_eq!(py_int(Some("9223372036854775807")), Some(i64::MAX));
        assert_eq!(py_int(Some("9223372036854775808")), Some(i64::MAX));
        assert_eq!(py_int(Some("-9223372036854775808")), Some(i64::MIN));
        assert_eq!(py_int(Some("-9223372036854775809")), Some(i64::MIN));
    }

    #[test]
    fn octets_names_the_part_it_could_not_read() {
        crate::pin_test_dir();
        assert_eq!(octets("1.2.3.4").unwrap(), vec![1, 2, 3, 4]);
        assert_eq!(octets(" 1 . 2 .3.4 ").unwrap(), vec![1, 2, 3, 4]);
        assert_eq!(octets("255.255.255.0.9").unwrap().len(), 5);
        assert_eq!(
            octets("1.2.x.4"),
            Err("invalid literal for int() with base 10: 'x'".to_string())
        );
        assert_eq!(
            octets(""),
            Err("invalid literal for int() with base 10: ''".to_string())
        );
        assert_eq!(
            octets("1.2.'3'"),
            Err("invalid literal for int() with base 10: \"'3'\"".to_string())
        );
    }

    #[test]
    fn subnet_and_asks_the_mask_before_it_asks_the_address() {
        crate::pin_test_dir();
        // Python's `get_subnet_ip` builds `a` from `mask` on its own line first
        // (`a = [int(n) for n in mask.split('.')]`, then `b = ... ip ...`), so with
        // two bad inputs the message that comes back names the **mask's** part.
        assert_eq!(
            subnet_and("nope.1.2.3", "255.255.255.x"),
            Err("invalid literal for int() with base 10: 'x'".to_string())
        );
        assert_eq!(
            subnet_and("255.255.255.x", "nope.1.2.3"),
            Err("invalid literal for int() with base 10: 'nope'".to_string())
        );
    }

    #[test]
    fn subnet_and_indexes_from_the_mask() {
        crate::pin_test_dir();
        // The loop runs four times over `a` first, so a short mask is the IndexError
        // even when the address is complete — and the reverse too.
        assert_eq!(
            subnet_and("1.2.3.4", "255.255"),
            Err("list index out of range".to_string())
        );
        assert_eq!(
            subnet_and("1.2", "255.255.255.0"),
            Err("list index out of range".to_string())
        );
        // Parts past the fourth go unread on both sides. And the fourth product is
        // `0 & 4`, not `4`: `a` is the **mask's** octet list, so the values that get
        // ANDed come from `"255.255.255.0.9"` at indexes 0..4 — the mask's fifth part
        // (`9`) and the address's fifth part (`5`) are both simply never read.
        assert_eq!(
            subnet_and("1.2.3.4.5", "255.255.255.0.9"),
            Ok(vec![1, 2, 3, 0])
        );
    }

    #[test]
    fn subnet_and_computes_the_network_of_the_pair() {
        crate::pin_test_dir();
        assert_eq!(
            subnet_and("192.168.1.188", "255.255.255.0"),
            Ok(vec![192, 168, 1, 0])
        );
        assert_eq!(
            subnet_and("192.168.1.188", "255.255.0.0"),
            Ok(vec![192, 168, 0, 0])
        );
        assert_eq!(
            subnet_and("10.1.2.3", "0.0.0.0"),
            Ok(vec![0, 0, 0, 0])
        );
        // Two addresses on one /24 agree, and the one off it does not: this is the
        // only comparison the discovery path makes.
        assert_eq!(
            subnet_and("192.168.1.7", "255.255.255.0"),
            subnet_and("192.168.1.200", "255.255.255.0")
        );
        assert_ne!(
            subnet_and("192.168.2.7", "255.255.255.0"),
            subnet_and("192.168.1.7", "255.255.255.0")
        );
    }

    #[test]
    fn randrange_names_the_empty_range_with_the_stop_python_prints() {
        crate::pin_test_dir();
        assert_eq!(
            py_randrange_inclusive(-1),
            Err("empty range in randrange(0, 0)".to_string())
        );
        assert_eq!(
            py_randrange_inclusive(-5),
            Err("empty range in randrange(0, -4)".to_string())
        );
        // The stop is `mx + 1` on both sides of the message, so the boundary itself
        // is not an error: 0 is a legal range with exactly one answer.
        assert_eq!(py_randrange_inclusive(0), Ok(0));
    }

    #[test]
    fn randrange_zero_never_touches_the_generator() {
        crate::pin_test_dir();
        let _guard = rng_guard();
        RNG_STATE.store(0x2545_F491_4F6C_DD1D, Ordering::Relaxed);
        assert_eq!(py_randrange_inclusive(0), Ok(0));
        assert_eq!(RNG_STATE.load(Ordering::Relaxed), 0x2545_F491_4F6C_DD1D);
        // Negative never reaches the generator either: the error returns first.
        assert_eq!(py_randrange_inclusive(-3).is_err(), true);
        assert_eq!(RNG_STATE.load(Ordering::Relaxed), 0x2545_F491_4F6C_DD1D);
    }

    #[test]
    fn randrange_stays_inside_the_bounds_it_was_given() {
        crate::pin_test_dir();
        let _guard = rng_guard();
        for _ in 0..64 {
            let v = py_randrange_inclusive(10).unwrap();
            assert!((0..=10).contains(&v), "randrange(0, 10) gave {}", v);
        }
        let v = py_randrange_inclusive(1).unwrap();
        assert!((0..=1).contains(&v), "randrange(0, 1) gave {}", v);
    }

    #[test]
    fn the_generator_moves_and_the_seed_is_never_zero() {
        crate::pin_test_dir();
        let _guard = rng_guard();
        assert_ne!(seed_state(), 0);
        assert_ne!(seed_state(), 0);
        RNG_STATE.store(0, Ordering::Relaxed);
        let first = rng_next();
        assert_ne!(RNG_STATE.load(Ordering::Relaxed), 0, "a zero state would wedge xorshift");
        let second = rng_next();
        assert_ne!(first, second);
        assert_ne!(second, 0);
    }

    // -- the receive face ---------------------------------------------------
    //
    // `start()` binds UDP/1900, so nothing here can be driven by a real client.
    // These call `datagram_received` the way the recv thread's `recvfrom` would,
    // with TEST-NET-3 peers, and read the answer out of the log file: silence is
    // a behaviour here, and a function that returns `()` cannot be asserted any
    // other way.

    /// A peer address that is not this LAN and not routable (RFC 5737).
    fn peer(last: u8, port: u16) -> SocketAddr {
        SocketAddr::from((Ipv4Addr::new(203, 0, 113, last), port))
    }

    /// The substring every line about that peer carries, so one needle can prove
    /// both "this line was written" and "nothing about this sender was".
    fn token(last: u8, port: u16) -> String {
        format!("203.0.113.{}:{}", last, port)
    }

    #[test]
    fn a_datagram_that_asks_for_nothing_is_silent() {
        crate::pin_test_dir();
        let s = server("silent");
        // `cmd.len() < 2` — one whitespace-free line is not a request, and Python
        // answers it without a word.
        s.datagram_received(b"\n", peer(201, 42001));
        // `\n` survives the `\r\n\r\n` split as a non-empty header, so it still
        // reaches the handler — and still says nothing.
        s.datagram_received(b"\r\n\r\nanything at all", peer(202, 42002));
        // A NOTIFY is a command we do not act on, and `cmd[0] != "NOTIFY"` is the
        // only thing standing between it and the info line.
        s.datagram_received(
            b"NOTIFY * HTTP/1.1\r\nHOST: 239.255.255.250:1900\r\nCACHE-CONTROL: max-age=1800\r\n\r\n",
            peer(203, 42003),
        );
        for p in [(201u8, 42001u16), (202, 42002), (203, 42003)] {
            assert!(
                logged(&token(p.0, p.1)).is_empty(),
                "quiet datagram spoke: {:?}",
                logged(&token(p.0, p.1))
            );
        }
        // Positive control: the same server, the same reader, one datagram that
        // *is* a command — so the three empties above are not just a broken reader.
        s.datagram_received(b"M-SEARCH * HTTP/1.1\r\n\r\n", peer(204, 42004));
        assert!(
            logged(&token(204, 42004))
                .iter()
                .any(|l| l.contains("SSDP command M-SEARCH * - from 203.0.113.204:42004")),
            "control datagram went unlogged"
        );
    }

    #[test]
    fn a_header_line_without_a_colon_is_the_malformed_warning() {
        crate::pin_test_dir();
        let s = server("colonless");
        // Python's `[1]` on a one-element split, reported by the outer `except`.
        s.datagram_received(
            b"M-SEARCH * HTTP/1.1\r\nMAN ssdp discover\r\n\r\n",
            peer(205, 42005),
        );
        let lines = logged(&token(205, 42005));
        // Exactly one line, and it is the warning. `lines.len() == 1` doubles as the
        // proof that the "SSDP command …" info line never happened, because that
        // line carries the same peer token: the exception is raised while the header
        // table is still being built, before the info line's turn.
        assert_eq!(lines.len(), 1, "expected one line, got {:?}", lines);
        assert!(
            lines[0].contains(
                "malformed SSDP datagram from 203.0.113.205:42005: list index out of range"
            ),
            "the line was: {}",
            lines[0]
        );
        assert!(
            logged("SSDP command M-SEARCH * - from 203.0.113.205").is_empty(),
            "the handler announced a command it never finished parsing"
        );
    }

    #[test]
    fn undecodable_bytes_are_an_error_without_the_peer_prefix() {
        crate::pin_test_dir();
        let s = server("decode");
        s.datagram_received(b"M\xff\n", peer(206, 42006));
        assert!(
            !logged("'utf-8' codec can't decode byte 0xff in position 1: invalid start byte")
                .is_empty(),
            "the decode failure said nothing"
        );
        // Python does `logger.error(e)`: the line *is* the exception text, so no
        // peer is named. This is the difference between this branch and the
        // malformed warning, and the reason both are asserted on the same token.
        assert!(
            logged(&token(206, 42006)).is_empty(),
            "the decode error grew a sender: {:?}",
            logged(&token(206, 42006))
        );
        assert!(
            logged("malformed SSDP datagram from 203.0.113.206").is_empty(),
            "a bad decode is not a bad parse"
        );
        // Positive control: something that decodes but does not parse *does* name
        // its sender, in the other shape.
        s.datagram_received(
            b"M-SEARCH * HTTP/1.1\r\nno colon here\r\n\r\n",
            peer(207, 42007),
        );
        assert!(
            !logged("malformed SSDP datagram from 203.0.113.207:42007: list index out of range")
                .is_empty(),
            "the prefix-bearing shape stopped being printed"
        );
    }

    #[test]
    fn a_newline_only_datagram_is_not_split_into_headers() {
        crate::pin_test_dir();
        // `header.split('\r\n')`, **not** `.lines()`. The `ST:` below is right there
        // in the bytes and still never reaches the header table, so this reads as a
        // request with no search target — which is what Python does.
        let s = server("lf");
        s.datagram_received(b"M-SEARCH * HTTP/1.1\nST: urn:example\n\n", peer(208, 42008));
        // The witness is filtered on the **bare address**, not `ip:port`, because this
        // module spells the same peer two ways: `handle_datagram` prints
        // `SSDP command {} {} - from {}:{}` (colon) while `discovery_request` prints
        // `Discovery request from ({},{})` (comma, no space). Keying the read on either
        // shape would hide the other from the assertions below, and the second one is
        // the point of this test. `203.0.113.208` is used by no other test, so the
        // wider filter still only ever sees this datagram's two lines.
        let lines = logged("203.0.113.208");
        assert!(
            lines
                .iter()
                .any(|l| l.contains("SSDP command M-SEARCH * - from 203.0.113.208:42008")),
            "{:?}",
            lines
        );
        assert!(
            lines.iter().any(|l| {
                l.contains("Discovery request from (203.0.113.208,42008) missing ST header")
            }),
            "{:?}",
            lines
        );
        // Two spellings of the same pair live in this module and are not
        // interchangeable: this warning is `({}, {})` with no space, while the delay
        // line uses Python's tuple repr (`({}, {})` **with** one, see
        // `tuple_repr_has_the_space_the_warning_does_not`). Splitting on `\n` here
        // would print the second shape and answer nothing.
        assert!(
            !lines
                .iter()
                .any(|l| l.contains("(203.0.113.208, 42008)")),
            "{:?}",
            lines
        );
    }

    #[test]
    fn an_unknown_verb_is_announced_and_refused() {
        crate::pin_test_dir();
        let s = server("verb");
        s.datagram_received(b"PING * HTTP/1.1\r\nST: x\r\n\r\n", peer(209, 42009));
        let lines = logged(&token(209, 42009));
        assert!(
            lines
                .iter()
                .any(|l| l.contains("SSDP command PING * - from 203.0.113.209:42009")),
            "{:?}",
            lines
        );
        // The warning names the verb only — never the sender — so it needs its own
        // needle.
        assert!(
            !logged("Unknown SSDP command PING *").is_empty(),
            "an unhandled verb passed through as a discovery request"
        );

        // `NOTIFY *` is the shape that escapes both lines: the info line is gated on
        // `cmd[0] != "NOTIFY"` and the warning on `cmd[1] != "*"`.
        s.datagram_received(b"NOTIFY * HTTP/1.1\r\nST: x\r\n\r\n", peer(210, 42010));
        assert!(logged(&token(210, 42010)).is_empty(), "{:?}", logged(&token(210, 42010)));
        assert!(logged("Unknown SSDP command NOTIFY").is_empty());

        // A NOTIFY whose second word is not `*` keeps the silence (no info line for
        // NOTIFY) but does *not* keep the warning. That asymmetry is the whole test:
        // a peer token here stays empty even though a line was written.
        s.datagram_received(b"NOTIFY ssdp:alive HTTP/1.1\r\n\r\n", peer(211, 42011));
        assert!(logged(&token(211, 42011)).is_empty());
        assert!(
            !logged("Unknown SSDP command NOTIFY ssdp:alive").is_empty(),
            "the second word stopped mattering"
        );
    }

    /// `(host, port)` as the two log lines inside `discovery_request` spell it.
    ///
    /// It is not [`token`]: Python formats those two with `%(host)s,%(port)s` — a
    /// comma with **no** space — while the malformed and command lines use
    /// `%s:%s`. One helper per spelling, so a test can ask for either without
    /// accidentally accepting the other.
    fn peer_tuple(last: u8, port: u16) -> String {
        format!("203.0.113.{},{}", last, port)
    }

    #[test]
    fn only_the_first_colon_space_is_eaten() {
        crate::pin_test_dir();
        let s = server("first-colon");
        // `x.replace(': ', ':', 1)` — count=1. A header *value* that contains its
        // own `: ` keeps it; eating every occurrence turns `foo: bar` into
        // `foo:bar`, which is a different answer on the wire for ST, MX, and
        // LOCATION alike.
        s.datagram_received(
            b"M-SEARCH * HTTP/1.1\r\nST: foo: bar\r\n\r\n",
            peer(212, 42012),
        );

        let lines = logged(&peer_tuple(212, 42012));
        assert_eq!(lines.len(), 1, "{:?}", lines);
        assert!(
            lines[0].contains("for foo: bar"),
            "the value lost its second `: `: {}",
            lines[0]
        );
        assert!(
            !lines[0].contains("for foo:bar"),
            "replace ran over every occurrence: {}",
            lines[0]
        );
        // And the third shape: not replacing at all leaves the space after the
        // *name* in the value, so the answer carries two spaces.
        assert!(
            !lines[0].contains("for  foo:"),
            "nothing was replaced: {}",
            lines[0]
        );

        // The command line for the same peer keeps the `host:port` spelling, which
        // is a separate fact about the same datagram.
        let cmd = logged(&token(212, 42012));
        assert_eq!(cmd.len(), 1, "{:?}", cmd);
        assert!(
            cmd[0].contains("SSDP command M-SEARCH * - from 203.0.113.212:42012"),
            "{}",
            cmd[0]
        );
    }

    #[test]
    fn header_values_are_never_stripped() {
        crate::pin_test_dir();
        let s = server("no-strip");
        // `ST:  x` — the count-1 replace takes the colon and *one* space, so the
        // value CPython stores is `" x"`. There is no `.strip()` in that chain, and
        // a reader who adds one changes the announced ST by a byte.
        s.datagram_received(
            b"M-SEARCH * HTTP/1.1\r\nST:  x\r\n\r\n",
            peer(213, 42013),
        );

        let lines = logged(&peer_tuple(213, 42013));
        assert_eq!(lines.len(), 1, "{:?}", lines);
        assert!(
            lines[0].contains("for  x"),
            "the value was stripped: {:?}",
            lines[0]
        );
    }

    #[test]
    fn names_are_lowercased_and_values_are_not() {
        crate::pin_test_dir();
        let s = server("case");
        // Two halves of one line: `.lower()` on the name is what makes `St:` find
        // the `st` lookup in `discovery_request`, and the *absence* of `.lower()`
        // on the value is what keeps `urn:Upper` uppercase in the answer.
        s.datagram_received(
            b"M-SEARCH * HTTP/1.1\r\nSt: urn:Upper\r\n\r\n",
            peer(214, 42014),
        );

        let lines = logged(&peer_tuple(214, 42014));
        assert_eq!(lines.len(), 1, "{:?}", lines);
        assert!(
            lines[0].contains("for urn:Upper"),
            "the value was lower-cased too: {}",
            lines[0]
        );
        assert!(
            !lines[0].contains("missing ST header"),
            "the name was not lower-cased: {}",
            lines[0]
        );
    }

    #[test]
    fn an_empty_st_header_counts_as_missing() {
        crate::pin_test_dir();
        let s = server("empty-st");
        // Python's `if not st:` reads an empty value the same as an absent one, and
        // that path prints `({},{})` — no space after the comma. Assert the exact
        // spelling *inside* the line rather than by searching for the other one, so
        // a peer token can never satisfy it by accident.
        s.datagram_received(
            b"M-SEARCH * HTTP/1.1\r\nST:\r\n\r\n",
            peer(215, 42015),
        );

        let lines = logged(&peer_tuple(215, 42015));
        assert_eq!(lines.len(), 1, "{:?}", lines);
        assert!(
            lines[0]
                .contains("Discovery request from (203.0.113.215,42015) missing ST header"),
            "{}",
            lines[0]
        );
        assert!(
            !lines[0].contains("(203.0.113.215, 42015)"),
            "the tuple grew a space: {}",
            lines[0]
        );
        assert!(
            !lines[0].contains(" for "),
            "an empty ST was announced as a search target: {}",
            lines[0]
        );
    }

    // ---- chunk D: `discovery_request`, called directly ----------------------
    //
    // Everything here drives the answer path with **zero senders installed**, so
    // "did we reply?" is read as "did we log the refusal to reply?", and a real
    // multicast never leaves this machine on the way to a green test.

    #[test]
    fn a_search_without_st_is_answered_with_the_comma_spelling() {
        crate::pin_test_dir();
        let s = server("d-missing-st");
        // The datagram path already covers the empty value; this is the *absent*
        // key, and it goes through `discovery_request` with no parsing in between.
        let outcome = s.discovery_request(&hs(&[("mx", "1")]), peer(220, 42020));
        assert_eq!(outcome, Ok(()), "a missing ST is not an exception");
        let lines = logged(&peer_tuple(220, 42020));
        assert_eq!(lines.len(), 1, "{:?}", lines);
        assert!(
            lines[0].contains("Discovery request from (203.0.113.220,42020) missing ST header"),
            "{}",
            lines[0]
        );
    }

    #[test]
    fn an_entry_without_st_raises_even_for_ssdp_all() {
        crate::pin_test_dir();
        let s = server("d-no-st-entry");
        // `if i['ST'] == st or st == 'ssdp:all'` — the subscript is evaluated
        // first and it is *unguarded*, so `ssdp:all` does not exempt an entry we
        // announced without a search target. The `KeyError` escapes the handler and
        // becomes the malformed warning, so the mismatch half of that line has to
        // stay unwritten.
        s.known().push(entry("uuid-without-st", vec![]));
        let outcome = s.discovery_request(&hs(&[("st", "ssdp:all")]), peer(221, 42021));
        assert_eq!(outcome, Err("KeyError('ST')".to_string()));

        let lines = logged(&peer_tuple(221, 42021));
        assert_eq!(lines.len(), 1, "{:?}", lines);
        assert!(lines[0].contains("for ssdp:all"), "{}", lines[0]);
        // The refusal-to-send line never happens: the `?` short-circuits before the
        // interface loop, and it names the peer by address only.
        assert!(logged("No SSDP sender for discovery source 203.0.113.221").is_empty());
    }

    #[test]
    fn a_negative_mx_is_the_exception_that_becomes_the_malformed_warning() {
        crate::pin_test_dir();
        let _guard = rng_guard();
        let s = server("d-mx");
        s.register_device("uuid-mx", "urn:mx-target", "http://203.0.113.9:8000/description.xml");
        // `random.randint(0, mx)` sits **outside** the `try` that guards the `int()`
        // conversion, so `-1` converts fine and then raises in the generator.
        let outcome = s.discovery_request(&hs(&[("st", "urn:mx-target"), ("mx", "-1")]), peer(222, 42022));
        assert_eq!(outcome, Err("empty range in randrange(0, 0)".to_string()));
        assert!(logged("No SSDP sender for discovery source 203.0.113.222").is_empty());

        // And a *non-numeric* MX is the other half of that pair: `except
        // (ValueError, TypeError)` turns it into `0`, which is a legal range, so
        // this search is answered (refused, but answered) instead of raising.
        let outcome = s.discovery_request(&hs(&[("st", "urn:mx-target"), ("mx", "soon")]), peer(223, 42023));
        assert_eq!(outcome, Ok(()), "a garbage MX must not escape as an error");
        assert_eq!(
            logged("No SSDP sender for discovery source 203.0.113.223").len(),
            1,
            "{:?}",
            logged("No SSDP sender for discovery source 203.0.113.223")
        );
    }

    #[test]
    fn only_the_entries_that_match_get_refused() {
        crate::pin_test_dir();
        let s = server("d-two-entries");
        s.register_device("uuid-a", "urn:first", "http://203.0.113.9:8000/description.xml");
        s.register_device("uuid-b", "urn:second", "http://203.0.113.9:8000/description.xml");

        // One search target, two announced devices: exactly one refusal.
        s.discovery_request(&hs(&[("st", "urn:first")]), peer(224, 42024))
            .unwrap();
        assert_eq!(
            logged("No SSDP sender for discovery source 203.0.113.224").len(),
            1,
            "the non-matching entry was answered too"
        );

        // `ssdp:all` walks the whole registry, so the same question now has two
        // answers. The count is the test: it is what the per-entry loop owes.
        s.discovery_request(&hs(&[("st", "ssdp:all")]), peer(225, 42025))
            .unwrap();
        assert_eq!(
            logged("No SSDP sender for discovery source 203.0.113.225").len(),
            2,
            "{:?}",
            logged("No SSDP sender for discovery source 203.0.113.225")
        );
    }

    #[test]
    fn an_entry_without_a_usn_is_silently_unanswered() {
        crate::pin_test_dir();
        let s = server("d-empty-usn");
        // `if usn:` — an empty USN means no DATE, no trailing CRLFs, and **no send
        // at all**, so the search that matches it neither answers nor logs a
        // refusal. It is `continue`, not an error.
        s.known().push(entry(
            "",
            vec![
                ("USN".to_string(), "".to_string()),
                ("ST".to_string(), "urn:blank".to_string()),
                (
                    "LOCATION".to_string(),
                    "http://203.0.113.9:8000/d.xml".to_string(),
                ),
            ],
        ));
        let outcome = s.discovery_request(
            &hs(&[("st", "urn:blank"), ("mx", "-1")]),
            peer(226, 42026),
        );
        assert_eq!(
            outcome,
            Ok(()),
            "the skipped entry never reached `random.randint`: `search_reply` returning \
             `None` continues before the delay is drawn, so a negative MX is not the fault"
        );
        assert!(logged("No SSDP sender for discovery source 203.0.113.226").is_empty());
        assert_eq!(logged(&peer_tuple(226, 42026)).len(), 1);
    }

    #[test]
    fn a_malformed_netmask_escapes_as_the_malformed_warning() {
        crate::pin_test_dir();
        let s = server("d-bad-mask");
        s.register_device("uuid-mask", "urn:mask", "http://203.0.113.9:8000/description.xml");
        // `subnet_and(ip, mask)?` — the `?` here is a real edge: the interface table
        // comes from `net::advertised_pairs()`, and one unparsable mask turns a
        // legal search into the malformed-datagram warning.
        s.state().ip_list.push(("203.0.113.9".to_string(), "255.x.0.0".to_string()));
        let outcome = s.discovery_request(&hs(&[("st", "urn:mask")]), peer(227, 42027));
        assert_eq!(
            outcome,
            Err("invalid literal for int() with base 10: 'x'".to_string())
        );
        assert!(logged("No SSDP sender for discovery source 203.0.113.227").is_empty());
    }

    // ---- chunk E: the registry, the notifications, the lifecycle -------------

    #[test]
    fn register_writes_six_headers_in_pythons_order() {
        crate::pin_test_dir();
        let s = server("e-register");
        s.register_device(
            "uuid-e-register",
            "urn:e-register-target",
            "http://203.0.113.9:8000/e-register.xml",
        );

        let rows = s.registry();
        assert_eq!(rows.len(), 1);
        let names: Vec<&str> = rows[0]
            .headers
            .iter()
            .map(|(k, _)| k.as_str())
            .collect();
        assert_eq!(
            names,
            vec!["USN", "LOCATION", "ST", "EXT", "SERVER", "CACHE-CONTROL"],
            "this order is the wire format of both an alive notification and a search reply"
        );
        assert_eq!(rows[0].header("EXT"), Some(String::new()));
        assert_eq!(rows[0].header("SERVER").as_deref(), Some(SERVER_ID));
        assert_eq!(
            rows[0].header("CACHE-CONTROL").as_deref(),
            Some(DEFAULT_CACHE_CONTROL)
        );

        let lines = logged("urn:e-register-target");
        assert_eq!(lines.len(), 1, "{:?}", lines);
        assert!(
            lines[0].contains(
                "Registering urn:e-register-target (http://203.0.113.9:8000/e-register.xml)"
            ),
            "{}",
            lines[0]
        );
    }

    #[test]
    fn unregister_writes_its_line_before_it_raises() {
        crate::pin_test_dir();
        let s = server("e-unregister");
        s.register_device(
            "uuid-e-here",
            "urn:e-here",
            "http://203.0.113.9:8000/e-unregister.xml",
        );

        // `logger.info(...)` sits before the dict lookup, so a refusal to remove
        // something still leaves a record that someone asked for it.
        assert_eq!(
            s.unregister("uuid-e-absent"),
            Err("KeyError('uuid-e-absent')".to_string())
        );
        assert_eq!(logged("Un-registering uuid-e-absent").len(), 1);

        // `%r` on a name containing an apostrophe flips to double quotes the way
        // Python's `repr` does, and this is the one place that spelling is printed
        // back at a reader.
        assert_eq!(
            s.unregister("uuid'e-quote"),
            Err("KeyError(\"uuid'e-quote\")".to_string())
        );

        // The success path really does remove it, and only it.
        assert_eq!(s.unregister("uuid-e-here"), Ok(()));
        assert!(!s.is_known("uuid-e-here"));
    }

    #[test]
    fn do_notify_of_an_unknown_usn_is_a_silent_success() {
        crate::pin_test_dir();
        let s = server("e-notify-unknown");
        // `if not self.is_known(usn): return` — an unknown device is a no-op, not
        // an error. The caller notifies every service it knows about, so raising
        // here would break the first service that was never registered.
        assert_eq!(s.do_notify("uuid-e-never-registered"), Ok(()));

        // The debug line sits **before** that membership test in Python, and it is
        // suppressed at the default threshold. That is what makes this a usable
        // witness: nothing about this usn reached the log at all, so the early
        // return happened before any message was built.
        assert!(logged("Sending alive notification for uuid-e-never-registered").is_empty());

        // **Documented gap**: Python sends the alive notification **twice**, and no
        // test in this file can count it. `send_it` walks `state().senders`, and
        // installing a real `crate::sock::Sock` would put a live multicast on the
        // user's network to prove a bookkeeping fact. The two calls sit side by side
        // in `do_notify` for a reader instead.
    }

    #[test]
    fn do_notify_names_the_key_it_could_not_find() {
        crate::pin_test_dir();
        let s = server("e-notify-no-st");
        s.known().push(entry(
            "uuid-e-no-st",
            vec![
                ("USN".into(), "uuid-e-no-st".into()),
                ("LOCATION".into(), "http://203.0.113.9:8000/x.xml".into()),
            ],
        ));
        // `NT` is what `ST` was renamed to, so an entry announced without one cannot
        // become an alive message. Python's `except KeyError` is not sitting in this
        // function or in its caller, so the failure has to escape as the answer.
        assert_eq!(
            s.do_notify("uuid-e-no-st"),
            Err("KeyError('ST')".to_string())
        );
    }

    #[test]
    fn byebye_names_the_usn_it_could_not_find() {
        crate::pin_test_dir();
        let s = server("e-byebye-missing");
        s.do_byebye("uuid-e-gone");

        // Two lines, in this order: the intent, then the build failure that
        // swallows it. A reader who gets only the second one cannot tell whether the
        // device was ever asked to leave.
        let lines = logged("uuid-e-gone");
        assert_eq!(lines.len(), 2, "{:?}", lines);
        assert!(
            lines[0].contains("Sending byebye notification for uuid-e-gone"),
            "{}",
            lines[0]
        );
        assert!(
            lines[1]
                .contains("error building byebye notification: KeyError('uuid-e-gone')"),
            "{}",
            lines[1]
        );
    }

    #[test]
    fn byebye_is_silent_when_the_policy_says_not_to() {
        crate::pin_test_dir();
        let s = server("e-byebye-muted");
        s.register_device(
            "uuid-e-muted",
            "urn:e-muted",
            "http://203.0.113.9:8000/e-muted.xml",
        );
        // `stop(False)` — which is what `update_ip()` does before it re-registers
        // everything — clears this flag so the dying thread does not announce a
        // goodbye for a device a new thread is about to re-announce. The whole body,
        // including the intent line, sits behind it.
        s.sending_byebye.store(false, Ordering::SeqCst);
        s.do_byebye("uuid-e-muted");
        // The registry line above carries only `ST` and `LOCATION`, so this needle
        // asks about nothing but the byebye path.
        assert!(logged("uuid-e-muted").is_empty());
    }

    // ---- chunk E2: the lifecycle flags and the liveness guard ------------------

    #[test]
    fn a_server_that_never_started_is_healthy_and_not_running() {
        crate::pin_test_dir();
        let s = server("e-never-started");
        // `is_healthy()` answers *yes* for a stopped server, and "never started" is
        // the same state as "stopped on purpose" as far as this question cares:
        // `running` is false, so nobody turned this off because it broke. A caller
        // that read it as a fault would restart the server on every stop — and
        // `update_ip()` stops before it restarts, so that caller would be restarting
        // a healthy server out from under its own re-register.
        assert!(s.is_healthy());
        assert!(!s.is_running());
    }

    #[test]
    fn a_dead_thread_behind_a_running_flag_is_unhealthy() {
        crate::pin_test_dir();
        let s = server("e-dead-thread");
        // The one shape `is_healthy()` exists for: `running` still says true because
        // the recv loop died on an error it does not catch, while the thread that
        // would answer an M-SEARCH is gone. From outside — port bound, connect
        // succeeds — this reads as healthy, which is exactly why a liveness flag,
        // not a socket probe, is what the periodic restart asks.
        s.running.store(true, Ordering::SeqCst);
        assert!(!s.is_healthy());
        // The flag is the answer, not `running`: once the thread is known to be
        // alive the same `running` reads as healthy.
        s.alive.store(true, Ordering::SeqCst);
        assert!(s.is_healthy());
        // Put the server back by hand instead of calling `stop(true)` here: with
        // `running` set to true, `stop()` binds a real wake UDP socket and
        // `send_to(b"", 239.255.255.250:1900)` — a multicast onto the user's LAN
        // from a unit test (AGENTS.md §10). The flag pair is all this test measures.
        s.running.store(false, Ordering::SeqCst);
        s.alive.store(false, Ordering::SeqCst);
        assert!(s.is_healthy());
    }

    #[test]
    fn stop_rewrites_the_byebye_policy_before_anything_else() {
        crate::pin_test_dir();
        let s = server("e-stop-policy");
        // `sending_byebye` starts true (`new()` mirrors Python: a server that was
        // started normally should announce goodbye when it stops), and `stop()`
        // writes it as its **first** action — before the wake send and before the
        // join — because the dying thread reads it from another stack.
        assert!(
            s.sending_byebye.load(Ordering::SeqCst),
            "a fresh server must intend to say byebye; the restart path in \
             `update_ip()` is the only thing that clears it"
        );
        // Nothing here has a thread or sets `running`, so `stop()` takes only its
        // first step: write the policy, skip the wake socket (no multicast leaves
        // this test), take a handle that is not there, and return.
        s.stop(false);
        assert!(!s.sending_byebye.load(Ordering::SeqCst));
        s.stop(true);
        assert!(s.sending_byebye.load(Ordering::SeqCst));
        assert!(!s.is_running());
    }

    #[test]
    fn alive_guard_clears_only_its_own_generation() {
        crate::pin_test_dir();
        // This is why `generation` exists at all: a thread unwinding from an
        // *earlier* `start()` must not blank the liveness flag of the server that
        // replaced it mid-restart. Get it wrong and the replacement reads as dead,
        // and the periodic restart tears down a socket that is answering fine.
        let alive = Arc::new(AtomicBool::new(true));
        let slot = Arc::new(Mutex::new(ThreadSlot::default()));
        slot.lock().unwrap().generation = 1;

        let stale = AliveGuard {
            alive: alive.clone(),
            slot: slot.clone(),
            generation: 0,
        };
        drop(stale);
        assert!(
            alive.load(Ordering::SeqCst),
            "a guard from generation 0 must leave the flag of generation 1 alone"
        );

        let current = AliveGuard {
            alive: alive.clone(),
            slot: slot.clone(),
            generation: 1,
        };
        drop(current);
        assert!(
            !alive.load(Ordering::SeqCst),
            "the guard that owns the current generation is the one thing that may \
             clear it"
        );
    }

    #[test]
    fn current_generation_reads_a_poisoned_slot() {
        crate::pin_test_dir();
        // A slot that is poisoned means a thread died holding it — and the
        // generation written before it died is still the current answer. Taking the
        // `Err` branch as a failure would make every guard on the way out skip its
        // check, so a panicked recv thread would leave `alive` true forever and
        // `is_healthy()` would never report the death the restart depends on.
        let slot = Arc::new(Mutex::new(ThreadSlot::default()));
        slot.lock().unwrap().generation = 7;
        // The panic line this prints to stderr is this test doing its job: it is the
        // "thread died holding the mutex" shape, manufactured on purpose.
        let died = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _held = slot.lock();
            panic!("the recv thread died holding the slot");
        }));
        assert!(died.is_err());
        assert!(
            slot.lock().is_err(),
            "the manufactured death must actually poison the mutex, otherwise the \
             `Err` branch below was never taken and this test proves nothing"
        );
        assert_eq!(current_generation(&slot), 7);
    }

    // ---- documented gaps: what this file could not ask --------------------------
    //
    // Written out rather than quietly omitted, because "the tests are green" reads
    // like "the behaviour is covered" and these four are not:
    //
    // 1. `start()` / `run()` / `shutdown()` / `run_thread()`. Every one of them
    //    binds UDP/1900 (or joins the thread that did), so none is callable from a
    //    unit test on a machine that has a user's SSDP running on it. The generation
    //    and thread-id claims inside `run_thread` — including the self-join guard
    //    that answers Python's `RuntimeError: cannot join current thread` — are
    //    therefore unexercised; [`AliveGuard`] and [`current_generation`] are tested
    //    around them, which is the half that has no socket.
    // 2. "Did we actually send it?" For the same reason no [`Sock`] is ever
    //    installed here: its `send_it` multicasts. Every receive-face test above
    //    converts that question into the log ("No SSDP sender for discovery source
    //    ..."), which proves the *decision* and not the bytes. So `do_notify`'s two
    //    `send_it` calls (Python's, deliberately: presence and the target-device
    //    announcement are separate packets), the `break` inside the subnet loop, and
    //    `Sock::send_it`'s own failure path are all unmeasured.
    // 3. Root-vs-module logging. `register()` writes through `logging::root()` while
    //    every other line in this file writes through the `SSDPServer` logger. That
    //    distinction is invisible from here: no logger name appears in a log line,
    //    and `Logger::claim()` has not run in these tests, so both go to the same
    //    file in the same `<stamp> [LEVEL] message` shape.
    // 4. `py_repr`'s `\U{...}` arm is unreachable from this file's inputs — every
    //    value it is asked to spell is an ASCII USN, ST, or header name.
    //
    // Tasks #13 and #14 (the DLNA HTTP face, then the on-machine probe) are where
    // 1 and 2 get their witnesses: #14 drives a real receiver against a real
    // UDP/1900 on this Mac.
}
