//! One UDP *sender* per interface, and the two shapes of `IP_MULTICAST_IF`.
//!
//! Python counterpart: `macast/ssdp.py::Sock`. The receiver on UDP/1900 lives in
//! [`crate::ssdp`]; this module is only about getting a datagram *out* of the
//! interface we chose, which is the part that is not the same call on every
//! operating system.

use std::io;
use std::net::{Ipv4Addr, SocketAddr};

use macast_core::logging::Logger;
use macast_core::net;
use socket2::{Domain, Protocol, Socket, Type};

/// A send-only UDP socket pinned to one interface.
///
/// `Sock` never joins a multicast group and never binds to receive: it exists so
/// that a `NOTIFY` or an M-SEARCH answer leaves through *this* interface's source
/// address. That is what the comment in Python's `__init__` means when it says a
/// virtual adapter can make `IP_MULTICAST_IF` fail with `WSAEADDRNOTAVAIL` — the
/// failure is deliberately **not** swallowed here, because the caller has to skip
/// the adapter rather than keep a socket that silently drops every datagram.
pub struct Sock {
    ip: String,
    sock: Socket,
}

/// What the kernel wants for `IP_MULTICAST_IF`.
///
/// Darwin and the BSDs want the interface **index** as a `u_int32` in network
/// byte order; Linux accepts the index *or* the address; Windows accepts only
/// the address. `socket2` exposes only the address form (`set_multicast_if_v4`),
/// so the index route goes to the operating system directly.
enum MulticastIf {
    #[cfg(target_os = "macos")]
    Index(u32),
    Addr(Ipv4Addr),
}

impl Sock {
    /// `Sock(ip)` — create, pin to the interface, and bind the source address.
    ///
    /// The option order is Python's, and the first three calls propagate while
    /// `bind` does not:
    ///
    /// 1. `IP_MULTICAST_IF` — propagates, so `crate::ssdp` can skip an adapter it
    ///    cannot send through (a hotspot/ICS adapter on Windows).
    /// 2. `IP_MULTICAST_LOOP = 0` — we must not hear our own `NOTIFY`, or every
    ///    announcement would look like a second device on the LAN.
    /// 3. `IP_MULTICAST_TTL = 4` — the default of `1` is link-local only, and a
    ///    DLNA client behind an AP or on another VLAN never receives the
    ///    announcement. Bumped to 4 to clear a few router hops.
    /// 4. `bind((ip, 0))` — wrapped, and failure only warns: this keeps unicast
    ///    SSDP replies on this interface when Wi-Fi, Ethernet, and a virtual
    ///    adapter are up together, which is a nicety, not a requirement.
    ///
    /// Python's `__init__` also computes `self.ssdp_addr = inet_aton(SSDP_ADDR)`
    /// and never reads it again; that dead field is not carried over.
    pub fn new(ip: &str, log: &Logger) -> io::Result<Sock> {
        let sock = Socket::new(Domain::IPV4, Type::DGRAM, Some(Protocol::UDP))?;
        let addr: Ipv4Addr = match ip.parse() {
            Ok(a) => a,
            // Python would raise `OSError` from `inet_aton` here and the caller
            // would log `skip SSDP interface …`. Same outcome, same message shape.
            Err(e) => return Err(io::Error::new(io::ErrorKind::InvalidInput, e.to_string())),
        };

        let mif = multicast_if(ip, addr, log);
        pin_multicast_if(&sock, mif, ip)?;
        sock.set_multicast_loop_v4(false)?;
        sock.set_multicast_ttl_v4(4)?;

        let bind_addr = SocketAddr::from((addr, 0));
        if let Err(e) = sock.bind(&bind_addr.into()) {
            log.warning(&format!("SSDP cannot bind sender to {}: {}", ip, e));
        }
        Ok(Sock {
            ip: ip.to_string(),
            sock,
        })
    }

    /// The address this sender speaks as.
    pub fn ip(&self) -> &str {
        &self.ip
    }

    /// `Sock.send_it(response, destination)` — fill the `{}` in the message with
    /// this interface's address and hand it to the kernel.
    ///
    /// `macast/plugin.py::SSDPPlugin.register()` builds the `LOCATION` as
    /// `http://{}:{port}/description.xml` **on purpose**: one registered device
    /// serves the right address on whichever interface the datagram leaves from.
    /// So the placeholder travels through the registry and is substituted here,
    /// at send time, per sender.
    ///
    /// All send errors are swallowed with one warning, exactly as Python does.
    /// That is what makes the two `except (AttributeError, socket.error)` blocks
    /// around `do_notify`/`do_byebye` unreachable in practice — they are ported as
    /// behaviour anyway, because a receiver that stops announcing on a transient
    /// `ENETUNREACH` is worse than one that logs.
    ///
    /// There is no `close()`: [`crate::ssdp`] holds every sender in an `Arc`, and
    /// dropping the last handle is what closes the descriptor. A `Sock` that
    /// anybody could still send through must not be closed.
    pub fn send_it(&self, response: &str, destination: SocketAddr, log: &Logger) {
        let text = expand_ip(response, &self.ip);
        match self.sock.send_to(text.as_bytes(), &destination.into()) {
            Ok(_) => {}
            Err(e) => {
                let _ = e; // Python's handler does not print the error; see `failure_sending`.
                log.warning(&failure_sending(&self.ip, &destination))
            }
        }
    }
}

/// Fill the `{}` placeholder in a registered message with this sender's address.
///
/// Python calls `str.format(self.ip)`, which substitutes **positionally** and
/// raises `IndexError` on a second `{}` or `KeyError` on a named one. Every
/// registered message comes from [`crate::device`], which writes exactly one bare
/// `{}` per `LOCATION`, so replacing every `{}` is
/// the faithful subset of that behaviour: same answer for every message we can
/// actually build, and it never raises on a stray brace a future device template
/// might carry. A `{}` that was meant to be literal would have to be written
/// `{{}}` in Python, and no template here does that.
pub fn expand_ip(response: &str, ip: &str) -> String {
    response.replace("{}", ip)
}

/// The Python-shaped warning: `failure sending out data: from <ip> to (<addr>, <port>)`.
///
/// The destination is spelled as a Python **tuple repr** (`('239.255.255.250',
/// 1900)`, single quotes) rather than `SocketAddr`'s `ip:port`, and the
/// `socket.error` itself is **not** printed — neither of those is decoration: the
/// log file is what §5's greps read, and a line that drifts from the shape a user
/// has already searched for is a lost debugging session. Kept as a function so the
/// exact spelling is written once instead of pasted at each call.
fn failure_sending(ip: &str, destination: &SocketAddr) -> String {
    format!(
        "failure sending out data: from {} to ('{}', {})",
        ip,
        destination.ip(),
        destination.port()
    )
}

/// Resolve the `IP_MULTICAST_IF` value for one address.
#[cfg(target_os = "macos")]
fn multicast_if(ip: &str, addr: Ipv4Addr, log: &Logger) -> MulticastIf {
    match interface_index(ip, log) {
        Ok(index) => MulticastIf::Index(index),
        Err(reason) => {
            // Python's `except Exception` around the whole netifaces + ctypes
            // block, including its own `RuntimeError('no if_index found for %s')`.
            log.error(&format!(
                "Sock: failed to resolve if_index for {}: {}, falling back to inet_aton",
                ip, reason
            ));
            MulticastIf::Addr(addr)
        }
    }
}

#[cfg(not(target_os = "macos"))]
fn multicast_if(_ip: &str, addr: Ipv4Addr, _log: &Logger) -> MulticastIf {
    // Linux and Windows: the address form. Linux would also take an index, and
    // the comment in Python's `__init__` says so; the address is what both accept.
    MulticastIf::Addr(addr)
}

/// `fcntl.if_nametoindex(name)` for the interface that carries `ip`.
///
/// Python asks `netifaces` for every interface and matches the AF_INET address;
/// we ask [`net::Snapshot`], which reads the same table through `getifaddrs` and
/// is what `advertised_pairs()` already uses to choose these addresses. Two
/// divergences, both recorded rather than papered over:
///
/// * The search order is the snapshot's (sorted by name, then address) instead of
///   the OS's enumeration order. Only reachable when two interfaces share one
///   address, and then either name yields a working index.
/// * There is no `netifaces` to fail: a snapshot read cannot raise, so the only
///   failure that reaches the fallback log line is "no interface has that address"
///   or `if_nametoindex` answering 0.
#[cfg(target_os = "macos")]
fn interface_index(ip: &str, _log: &Logger) -> Result<u32, String> {
    let snapshot = net::Snapshot::collect();
    let name = snapshot
        .addrs
        .iter()
        .find(|r| r.ip == ip)
        .map(|r| r.name.clone())
        .ok_or_else(|| format!("no if_index found for {}", ip))?;
    let c = std::ffi::CString::new(name).map_err(|e| e.to_string())?;
    let index = unsafe { libc::if_nametoindex(c.as_ptr()) };
    if index == 0 {
        return Err(format!("no if_index found for {}", ip));
    }
    Ok(index)
}

/// Set `IP_MULTICAST_IF` in whichever shape the platform wants.
#[cfg(target_os = "macos")]
fn pin_multicast_if(sock: &Socket, mif: MulticastIf, _ip: &str) -> io::Result<()> {
    use std::os::unix::io::AsRawFd;
    match mif {
        MulticastIf::Index(index) => {
            // `struct.pack('!I', if_index)` — network byte order, four bytes.
            // Handing the kernel `&index` on a little-endian Mac is the same bug
            // the Python comment warns about, just with the bytes swapped here
            // instead of by the syscall.
            let packed = index.to_be();
            let rc = unsafe {
                libc::setsockopt(
                    sock.as_raw_fd(),
                    libc::IPPROTO_IP,
                    libc::IP_MULTICAST_IF,
                    &packed as *const u32 as *const libc::c_void,
                    std::mem::size_of::<u32>() as libc::socklen_t,
                )
            };
            if rc == 0 {
                Ok(())
            } else {
                Err(io::Error::last_os_error())
            }
        }
        MulticastIf::Addr(addr) => sock.set_multicast_if_v4(&addr),
    }
}

#[cfg(not(target_os = "macos"))]
fn pin_multicast_if(sock: &Socket, mif: MulticastIf, _ip: &str) -> io::Result<()> {
    match mif {
        MulticastIf::Addr(addr) => sock.set_multicast_if_v4(&addr),
    }
}

/// The receiver's `SO_REUSE*` choice, kept next to the sender's options because
/// this is the second place in this crate where the platform is not the same.
///
/// * Windows: `SO_REUSEADDR`, set **without** a guard, as Python does. On Windows
///   that option means "allow another socket to bind this exact address and
///   port", which is what a second receiver needs to be able to send byebye.
/// * Darwin: `SO_REUSEPORT`, also unguarded. macOS gives each binder its own
///   socket and load-balances inbound datagrams, so two stacks can genuinely
///   share UDP/1900. The call is `libc`-level here — see [`set_reuse_port`].
/// * Linux: `SO_REUSEPORT`, guarded, and a failure logs the two Python strings
///   and keeps going — on Linux the option is a convenience for development and
///   refusing to start over it would be worse.
///
/// ⚠ The Darwin and Windows calls deliberately propagate: Python's `run()` has no
/// `try` there either, so a failing `setsockopt` kills the SSDP thread while
/// `running` stays true. [`crate::ssdp::SsdpServer::is_healthy`] is what notices,
/// and the periodic restart that acts on it arrives with the HTTP server port.
pub fn set_reuse_for_ssdp(sock: &Socket, log: &Logger) -> io::Result<()> {
    apply_reuse(sock, log)
}

/// Windows: `SO_REUSEADDR`, unguarded, as Python does. On Windows that option
/// means "let another socket bind this exact address and port", which is what a
/// second receiver needs in order to be able to send its own byebye.
#[cfg(windows)]
fn apply_reuse(sock: &Socket, _log: &Logger) -> io::Result<()> {
    sock.set_reuse_address(true)
}

/// `setsockopt(SOL_SOCKET, SO_REUSEPORT, 1)` — the raw call, deliberately.
///
/// `socket2` does ship this setter, but behind `features = ["all"]`, and this
/// workspace pulls that crate in for exactly two multicast options plus this one.
/// Turning the feature on would recompile the copy already in the graph (via
/// `tokio`/`hyper-util`) with a far larger surface to buy one `setsockopt`, so the
/// option goes to the operating system the same way [`pin_multicast_if`] does on
/// Darwin. `libc` is a `cfg(unix)` dependency of this crate for that reason.
#[cfg(unix)]
fn set_reuse_port(sock: &Socket) -> io::Result<()> {
    use std::os::unix::io::AsRawFd;

    let on: libc::c_int = 1;
    let rc = unsafe {
        libc::setsockopt(
            sock.as_raw_fd(),
            libc::SOL_SOCKET,
            libc::SO_REUSEPORT,
            &on as *const libc::c_int as *const libc::c_void,
            std::mem::size_of::<libc::c_int>() as libc::socklen_t,
        )
    };
    if rc == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

/// Darwin: `SO_REUSEPORT`, unguarded — see the warning on
/// [`set_reuse_for_ssdp`] about what a failure here costs.
#[cfg(target_os = "macos")]
fn apply_reuse(sock: &Socket, _log: &Logger) -> io::Result<()> {
    set_reuse_port(sock)
}

/// Everything else: try `SO_REUSEPORT`, fall back to `SO_REUSEADDR`, and treat
/// neither failure as fatal. The four message strings are Python's verbatim, in
/// Python's order, at Python's levels (`debug` on success, two `error` lines on
/// failure with the exception text as its **own** line — that split is why §5's
/// `grep -aE "ERROR|Traceback"` finds the reason next to the label).
#[cfg(all(unix, not(target_os = "macos")))]
fn apply_reuse(sock: &Socket, log: &Logger) -> io::Result<()> {
    match set_reuse_port(sock) {
        Ok(()) => {
            log.debug("SSDP set SO_REUSEPORT");
            return Ok(());
        }
        Err(e) => {
            log.error("SSDP cannot set SO_REUSEPORT");
            log.error(&e.to_string());
        }
    }
    match sock.set_reuse_address(true) {
        Ok(()) => log.debug("SSDP set SO_REUSEADDR"),
        Err(e) => {
            log.error("SSDP cannot set SO_REUSEADDR");
            log.error(&e.to_string());
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn destination(text: &str) -> SocketAddr {
        text.parse()
            .unwrap_or_else(|_| panic!("{text} is not a socket address"))
    }

    /// The whole contract of the placeholder: it is a **substitution**, not a
    /// template language. One `{}` becomes the sender's address and nothing else
    /// in the message moves.
    #[test]
    fn expand_ip_fills_the_placeholder_and_nothing_else() {
        crate::pin_test_dir();
        assert_eq!(
            expand_ip("http://{}:8000/description.xml", "203.0.113.9"),
            "http://203.0.113.9:8000/description.xml"
        );
        // A message with no placeholder passes through untouched: `NOTIFY` and the
        // search reply are built that way and are sent through this same function,
        // so "no `{}`" must never become "something was rewritten".
        assert_eq!(
            expand_ip("NTS: ssdp:alive\r\n", "203.0.113.9"),
            "NTS: ssdp:alive\r\n"
        );
    }

    /// The two documented divergences from `str.format`, written down because they
    /// are the places where this port answers a question Python would have raised
    /// on, or answered differently.
    #[test]
    fn expand_ip_replaces_every_placeholder_rather_than_raising() {
        crate::pin_test_dir();
        // Python's `.format(ip)` fills only the *first* `{}` and raises `IndexError`
        // on the second (`Replacement index 1 out of range for positional args
        // tuple`). Filling every occurrence is the divergence this port chose: no
        // message `device.rs` builds has more than one bare `{}`, so the answers
        // agree on every real input, and a stray extra brace can never take the
        // SSDP thread down the way it would in Python.
        assert_eq!(
            expand_ip("a{}b{}c", "203.0.113.9"),
            "a203.0.113.9b203.0.113.9c"
        );
        //
        // **Where it stops being faithful, measured rather than assumed:** `{{}}` is
        // Python's escape for a literal brace, so `.format(ip)` there yields `{}`.
        // This function sees a `{}` starting at index 1 and consumes it, so
        // `"{{}}"` becomes `"{<ip>}"`. It does not honour brace escaping at all.
        // That is safe only because no template in this crate writes an escaped
        // brace — a future one must carry its own literal text rather than come
        // through here, and this assertion is what says so out loud instead of
        // leaving it as a comment next to a claim of parity.
        assert_eq!(expand_ip("{{}}", "203.0.113.9"), "{203.0.113.9}");
    }

    #[test]
    fn failure_sending_spells_the_destination_as_a_python_tuple() {
        crate::pin_test_dir();
        // Two shapes in one line, both load-bearing: the destination is a Python
        // **tuple repr** with single quotes (not `SocketAddr`'s `ip:port`), and the
        // `socket.error` itself is not printed at all — the `let _ = e;` in
        // [`Sock::send_it`] is deliberate, because that is what Python's handler
        // does. The line is what §5's greps read, so it is spelled for the shape a
        // user has already learned to search for.
        assert_eq!(
            failure_sending(
                "203.0.113.9",
                &destination("239.255.255.250:1900")
            ),
            "failure sending out data: from 203.0.113.9 to ('239.255.255.250', 1900)"
        );
        // The port is a bare number, not part of one quoted string: a reader who
        // copied the quoted half would search for `'1900'` and find nothing.
        assert_eq!(
            failure_sending("192.168.1.5", &destination("192.168.1.13:8008")),
            "failure sending out data: from 192.168.1.5 to ('192.168.1.13', 8008)"
        );
    }

    #[test]
    fn a_sender_for_an_address_that_is_not_an_address_refuses_as_invalid_input() {
        crate::pin_test_dir();
        // The one [`Sock::new`] path this file can be asked about without a socket:
        // a name that is not an IPv4 address is refused with `InvalidInput`, and the
        // text is the parser's own words rather than a message invented here —
        // because the caller in `crate::ssdp` logs "skip SSDP interface …" and the
        // reason has to survive into that line. (Python raises `OSError` out of
        // `inet_aton` for the same case; the shape of the answer is what matters,
        // not the exception class.)
        let log = Logger::named("SockRefusalTest");
        let refused = Sock::new("not-an-address", &log);
        let error = match refused {
            Ok(_) => panic!("a name that is not an address must never build a sender"),
            Err(e) => e,
        };
        assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
        let parse_error = "not-an-address"
            .parse::<Ipv4Addr>()
            .expect_err("the input must fail the same way twice, or this test is empty");
        assert_eq!(
            error.to_string(),
            parse_error.to_string(),
            "the refusal must carry the parser's own sentence, not a paraphrase"
        );
    }

    // ---- documented gaps --------------------------------------------------------
    //
    // Not faked with a socket, and not silently missing:
    //
    // 1. [`Sock::new`]'s success path, [`Sock::ip`], and [`Sock::send_it`] need a
    //    real interface address. Building a sender for `203.0.113.9` would make the
    //    test's outcome depend on whether `IP_MULTICAST_IF` accepts an address this
    //    machine does not have (and on macOS that goes through the interface-index
    //    lookup, which is a different answer per platform) — so it would be a
    //    machine report, not a contract.
    // 2. The `SSDP cannot bind sender to …` warning is `sock.bind()` failing, and
    //    the choice that it is a **warning** rather than an error is only visible
    //    with a bound socket.
    // 3. [`set_reuse_for_ssdp`] is three different platform behaviours. The macOS
    //    and Windows arms deliberately propagate (`is_healthy()` is what notices
    //    afterwards), the Linux arm deliberately does not, and `SO_REUSEPORT` on a
    //    loopback socket in a unit test would answer "does this kernel have the
    //    option", which is not a property of this crate.
    // 4. The failure warning path in [`Sock::send_it`] is the reason
    //    `do_notify`/`do_byebye`'s error handling is unreachable in practice —
    //    asserting it would mean sending a real datagram, so [`failure_sending`] is
    //    tested as the pure string it produces and the sending itself is left to
    //    the on-machine probe (task #14).
}
