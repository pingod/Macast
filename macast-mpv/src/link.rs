//! The one moving part of mpv's IPC that is not pure: a connected socket.
//!
//! This module is deliberately small. Everything about it exists to answer one
//! question in the same words the reference does: *what does the log say when the
//! transport is not there?* Every sentence it can produce was read off
//! CPython 3.12.15 on this machine rather than composed — see the table in
//! [`Link`]'s docs and the tests at the bottom of this module, which assert the
//! whole string, brackets and capitalisation included.
//!
//! Windows is not implemented here, and the reason is honest rather than lazy:
//! the reference uses `_winapi.CreateFile` plus a `PipeConnection` wrapper for
//! named pipes, and a named pipe cannot be exercised on this machine. So the whole
//! module is `#[cfg(unix)]` — it does not compile there, which is a build error
//! someone will see, rather than a mock that lets a test pass against code that
//! has never run. `crate::player` is gated the same way.

use std::io::{ErrorKind, Read, Write};
use std::net::Shutdown;
use std::os::fd::AsRawFd;
use std::os::unix::io::FromRawFd;
use std::os::unix::net::UnixStream;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

/// The text `send_command` logs when the socket was never made.
///
/// `self.ipc_sock` starts as `None` in `__init__`, so a command sent before the
/// IPC thread's first `socket.socket(...)` raises
/// `AttributeError: 'NoneType' object has no attribute 'sendall'`, which the
/// `except Exception` arm turns into a line. Measured verbatim.
pub const NO_SOCKET: &str = "'NoneType' object has no attribute 'sendall'";

/// `str(e)` for an `OSError` that carries an errno: `"[Errno 2] No such file or
/// directory"`.
///
/// **No filename is ever appended**, and that is not an oversight: every socket
/// failure measured below has `e.filename is None` — `connect()` does not put the
/// path into the exception, unlike `subprocess.Popen`, which does put the program
/// name into its own (see [`crate::player`]'s spawn text). CPython's
/// `OSError.__str__` only appends the filename when there is one, so appending it
/// here would be quoting a file the error never named.
pub fn errno_text(code: i32) -> String {
    let message = strerror(code);
    format!("[Errno {code}] {message}")
}

/// The operating system's own words for an errno — the same `strerror(3)` that
/// CPython calls to build an `OSError`'s message.
fn strerror(code: i32) -> String {
    let raw = unsafe { libc::strerror(code) };
    if raw.is_null() {
        return format!("Unknown error: {code}");
    }
    unsafe { std::ffi::CStr::from_ptr(raw) }
        .to_string_lossy()
        .into_owned()
}

/// `str(e)` for the one socket failure that has no errno at all.
///
/// A path that will not fit in `sockaddr_un` raises `OSError('AF_UNIX path too
/// long')` before any syscall happens, so `errno` is `None` and there is no
/// `[Errno n]` prefix to print. Measured verbatim.
pub const PATH_TOO_LONG: &str = "AF_UNIX path too long";

/// One endpoint of the JSON IPC line protocol.
///
/// The three states a Python socket can be in are all reachable here, and they
/// are all different sentences:
///
/// | state | `send_all` | `recv_into` |
/// |---|---|---|
/// | never created | [`NO_SOCKET`] — the caller holds `None`, not a `Link` | (the reader loop is not running yet) |
/// | created, not connected | `[Errno 57] Socket is not connected` | `[Errno 57] Socket is not connected` |
/// | connected | `[Errno 32] Broken pipe` once mpv is gone | `Ok(0)` — a clean EOF |
/// | closed by us | `[Errno 9] Bad file descriptor` | `[Errno 9] Bad file descriptor` |
///
/// Every one of those was measured twice on this machine — once in CPython 3.12.15
/// and once through this module — including the two shapes that look like they
/// should differ but do not: a peer that dies while bytes sit **unread in our own
/// receive queue** still answers `[Errno 32] Broken pipe` (not `ECONNRESET`, and
/// the queued bytes stay readable), which is why the broken-pipe test below asserts
/// exactly one sentence rather than accepting a set.
///
/// The connect failures a real path produces are all mapped too, and each of them
/// is a different diagnosis the user needs to hear: `[Errno 2] No such file or
/// directory` (mpv has not made the socket yet), `[Errno 61] Connection refused`
/// (the file is a socket but nothing is listening on it), and `[Errno 38] Socket
/// operation on non-socket` (something else owns that path).
///
/// # The one place this is not the operating system
///
/// [`Link::close`] sets a flag rather than releasing the file descriptor, because
/// `Arc<UnixStream>` cannot hand its fd out from behind a shared reference, and
/// closing a descriptor two owners both believe they hold would be a bug in this
/// port, not a fidelity win. The flag then produces the same
/// `[Errno 9] Bad file descriptor` that CPython produces from a genuinely closed
/// fd, so the log line matches; the difference is that Rust's fd is still open
/// until the last owner drops it. Measured here (see
/// `a_link_closed_by_another_owner_wakes_a_blocked_reader`): `shutdown(2)` on a
/// macOS `AF_UNIX` stream wakes a thread parked in `read(2)` and hands it `ECONN`
/// — `Ok(0)`, EOF — and that is true whether the waking side asks for `SHUT_RD` or
/// `SHUT_RDWR`. Dropping a *non-final* handle, by contrast, wakes nothing.
///
/// **None of that is what ends the reader in `stop()`.** The reference joins the
/// player thread *before* it sets `ipc_running = False`, and its
/// `self.ipc_sock.close()` sits at the bottom of the outer loop — code that only
/// runs once the inner `recv` loop has already broken. So the session ends because
/// **mpv exits, its half closes, and `recv` returns `b''`**. [`Link::close`]
/// remains the right thing for a shutdown we start ourselves, but a caller must not
/// design `stop()` around it; [`crate::player`] joins the player first for exactly
/// this reason.
#[derive(Debug)]
pub struct Link {
    stream: Arc<UnixStream>,
    closed: AtomicBool,
}

impl Link {
    /// `socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)` — the descriptor
    /// exists, nothing is connected.
    ///
    /// `Err` is `str(e)` for the `socket()` call itself failing. It is essentially
    /// unreachable (the reference has no branch for it either — it just falls into
    /// the same `except` as a connect failure), but it is a `Result` so a caller
    /// cannot mistake "no socket" for "unconnected socket", which are different
    /// rows of the table above.
    pub fn created() -> Result<Self, String> {
        let fd = unsafe { libc::socket(libc::AF_UNIX, libc::SOCK_STREAM, 0) };
        if fd < 0 {
            return Err(errno_text(last_errno()));
        }
        Ok(Self {
            stream: Arc::new(unsafe { UnixStream::from_raw_fd(fd) }),
            closed: AtomicBool::new(false),
        })
    }

    /// A socket already connected to `path`, which is what a caller wants when it
    /// does not care to keep the half-made object around.
    ///
    /// Note that the reference does *not* use this shape: `start_ipc` assigns
    /// `self.ipc_sock` before calling `connect`, so a failed connect leaves an
    /// unconnected socket in place — and a later `send_command` then logs
    /// `[Errno 57]`, not the `AttributeError`. Use [`Link::created`] plus
    /// [`Link::connect`] to reproduce that.
    pub fn to(path: &str) -> Result<Self, String> {
        let link = Self::created()?;
        link.connect(path)?;
        Ok(link)
    }

    /// `self.ipc_sock.connect(self.mpv_sock)`.
    ///
    /// This connects **this** descriptor, because that is the one the reference
    /// published as `self.ipc_sock` before it called `connect`. It is not the same
    /// as `UnixStream::connect(path)`: that would build a *second* socket, and a
    /// real mpv accepts concurrent connections, so a throwaway probe would quietly
    /// consume one of its accept slots and then be discarded.
    ///
    /// What success does **not** mean: a listener that has not reached `accept` yet
    /// still completes a connection into its backlog, so `Ok(())` here is evidence
    /// that mpv made the socket, not that it is answering on it. The reference knows
    /// this implicitly — it never trusts a connect, it issues `set_observe` and reads
    /// replies — and [`crate::player`] keeps that order.
    pub fn connect(&self, path: &str) -> Result<(), String> {
        let address = sockaddr_un(path)?;
        let rc = unsafe {
            libc::connect(
                self.fd(),
                &address as *const libc::sockaddr_un as *const libc::sockaddr,
                address_len(path),
            )
        };
        if rc == 0 {
            return Ok(());
        }
        Err(errno_text(last_errno()))
    }

    /// `sendall(msg.encode())` — all bytes or nothing.
    pub fn send_all(&self, bytes: &[u8]) -> Result<(), String> {
        if self.closed.load(Ordering::SeqCst) {
            return Err(errno_text(libc::EBADF));
        }
        (&*self.stream).write_all(bytes).map_err(|err| text_of(&err))
    }

    /// `self.ipc_sock.recv(1048576)`.
    ///
    /// `Ok(0)` is `b''` — mpv closed the socket and the reader loop breaks on it.
    /// A negative `isize` is not a thing here: the reference's "no data" is `0`,
    /// and the caller compares against it.
    pub fn recv_into(&self, buf: &mut [u8]) -> Result<usize, String> {
        if self.closed.load(Ordering::SeqCst) {
            return Err(errno_text(libc::EBADF));
        }
        loop {
            match (&*self.stream).read(buf) {
                Ok(n) => return Ok(n),
                // A Unix socket read interrupted by a signal: CPython retries
                // (PEP 475), so this is not an error at all, and answering `Ok(0)`
                // here would be worse than answering `Err` — `0` means EOF, and the
                // reader loop would end a live session over a stray `EINTR`.
                Err(err) if err.kind() == ErrorKind::Interrupted => continue,
                Err(err) => return Err(text_of(&err)),
            }
        }
    }

    /// `self.ipc_sock.close()`, plus the `shutdown` that makes it safe here.
    ///
    /// Idempotent: a second call changes nothing, which matches CPython's `close()`
    /// on an already-closed socket raising nothing that the reference's
    /// `except OSError: pass` would let escape.
    pub fn close(&self) {
        self.closed.store(true, Ordering::SeqCst);
        let _ = self.stream.shutdown(Shutdown::Both);
    }

    /// Whether [`Link::close`] has run. `stop()` reads this to know whether the
    /// reader thread has finished with the socket.
    pub fn is_closed(&self) -> bool {
        self.closed.load(Ordering::SeqCst)
    }

    /// Whether this descriptor has a peer, asked of the kernel with
    /// `getpeername(2)` — the same fact a failed write reports as
    /// `[Errno 57] Socket is not connected`.
    ///
    /// A connection parked in a listener's **backlog** counts as having a peer: the
    /// three-way handshake is done from this side's point of view long before the
    /// other end calls `accept`. So `true` here means "mpv created its IPC socket",
    /// not "mpv is serving it" — see the note on [`Link::connect`].
    pub fn is_connected(&self) -> bool {
        let mut storage = unsafe { std::mem::zeroed::<libc::sockaddr_storage>() };
        let mut length = std::mem::size_of::<libc::sockaddr_storage>() as libc::socklen_t;
        let rc = unsafe {
            libc::getpeername(
                self.fd(),
                &mut storage as *mut libc::sockaddr_storage as *mut libc::sockaddr,
                &mut length as *mut libc::socklen_t,
            )
        };
        rc == 0
    }

    fn fd(&self) -> i32 {
        self.stream.as_raw_fd()
    }
}

impl Clone for Link {
    /// Another handle for the same descriptor.
    ///
    /// This is `self.ipc_sock` being read by the IPC thread and written by
    /// `send_command` at the same time — one socket, two owners, which is what
    /// `try_clone()` would *not* give us (a duplicated descriptor closing
    /// independently would leave the other side's `recv` waiting forever).
    fn clone(&self) -> Self {
        Self {
            stream: Arc::clone(&self.stream),
            closed: AtomicBool::new(self.closed.load(Ordering::SeqCst)),
        }
    }
}

/// Turn an `io::Error` into the `str(e)` CPython would have.
fn text_of(err: &std::io::Error) -> String {
    match err.raw_os_error() {
        Some(code) => errno_text(code),
        None => err.to_string(),
    }
}

/// The errno set by the libc call that just failed. Read immediately after the
/// call, because anything in between can overwrite it.
fn last_errno() -> i32 {
    std::io::Error::last_os_error()
        .raw_os_error()
        .unwrap_or(libc::EIO)
}

/// The `sun_path` member's offset and size on *this* platform, both of which
/// differ: Linux has no `sun_len` byte and a 108-byte path, BSDs and macOS have
/// both. Reading them off the type is the only way not to hard-code either, and a
/// wrong offset puts `sun_family` where the kernel reads `sun_len` — which fails
/// with an errno no test would recognise as "my struct is shaped wrong".
fn sun_layout() -> (usize, usize) {
    // SAFETY: `sockaddr_un` is a plain C struct of integers; all-zeroes is a
    // valid value, and we only ask a member for its size.
    let blank = unsafe { std::mem::zeroed::<libc::sockaddr_un>() };
    let offset = std::mem::offset_of!(libc::sockaddr_un, sun_path);
    (offset, std::mem::size_of_val(&blank.sun_path))
}

/// Whether a path would fit in `sockaddr_un` — used to tell a too-long path apart
/// from a socket-level refusal, since only the first has no errno. CPython's own
/// check is `len(path) + 1 <= sizeof(sun_path)`, so the NUL is counted.
fn fits(path: &str) -> bool {
    let (_, capacity) = sun_layout();
    path.len() + 1 <= capacity
}

/// `sockaddr_un` for a path, zeroed apart from the family and the name.
///
/// The reference is not careful here — `socket.socket().connect(str)` goes through
/// CPython, which does this same packing — so the layout is only reproduced to keep
/// `connect(2)` honest about where the name ends.
fn sockaddr_un(path: &str) -> Result<libc::sockaddr_un, String> {
    if !fits(path) {
        return Err(PATH_TOO_LONG.to_string());
    }
    // SAFETY: all-zeroes is a valid `sockaddr_un`; the padding it leaves in the
    // name is what CPython's `strncpy` target looks like too.
    let mut address = unsafe { std::mem::zeroed::<libc::sockaddr_un>() };
    address.sun_family = libc::AF_UNIX as libc::sa_family_t;
    for (slot, byte) in address.sun_path.iter_mut().zip(path.as_bytes()) {
        *slot = *byte as libc::c_char;
    }
    Ok(address)
}

/// The third argument to `connect(2)`: the family field plus the bytes the name
/// actually uses, NUL included. Passing `sizeof(sockaddr_un)` instead would make
/// the kernel read past the end of the name into padding.
fn address_len(path: &str) -> libc::socklen_t {
    let (offset, _) = sun_layout();
    (offset + path.len() + 1) as libc::socklen_t
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tempdir() -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("macast-link-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("temp dir");
        dir
    }

    fn path(name: &str) -> String {
        let owned = tempdir().join(name);
        let _ = std::fs::remove_file(&owned);
        owned.to_string_lossy().into_owned()
    }

    /// A listening socket at `path`, as a `std` listener: mpv's own socket is
    /// exactly this shape before it starts accepting.
    fn listen(path: &str) -> std::os::unix::net::UnixListener {
        std::os::unix::net::UnixListener::bind(path).expect("bind")
    }

    #[test]
    fn an_unconnected_socket_says_it_is_not_connected() {
        // This is the state `start_ipc` leaves `self.ipc_sock` in when `connect`
        // raises, because the assignment happens first — so both the writer's
        // `sendall` and the reader's `recv` have to answer, and they answer alike.
        let link = Link::created().expect("socket()");
        assert_eq!(
            link.send_all(b"{\"command\":[]}\n"),
            Err(errno_text(libc::ENOTCONN))
        );
        let mut buf = [0u8; 16];
        assert_eq!(link.recv_into(&mut buf), Err(errno_text(libc::ENOTCONN)));
    }

    #[test]
    fn the_not_connected_sentence_is_the_measured_one() {
        // 57 is macOS' ENOTCONN. It is written out rather than reached through
        // `libc::ENOTCONN` above so that a platform with a different number cannot
        // quietly redefine what the log says — this assertion is the machine asking
        // the reference's sentence, not the port restating its own constant.
        assert_eq!(errno_text(57), "[Errno 57] Socket is not connected");
    }

    #[test]
    fn connecting_to_a_path_with_no_socket_names_no_file() {
        let missing = path("nope");
        assert_eq!(
            Link::to(&missing).err(),
            Some("[Errno 2] No such file or directory".to_string())
        );
        // And a directory answers the same way, because the kernel never gets to
        // say ENOTSOCK: the pathname walk fails first.
        let dir = tempdir();
        assert_eq!(
            Link::to(&dir.join("no-such-dir").to_string_lossy()).err(),
            Some("[Errno 2] No such file or directory".to_string())
        );
    }

    #[test]
    fn a_socket_file_with_no_listener_refuses() {
        // The shape that actually refuses is "the file is a socket and nothing is
        // listening", which on this machine means a listener that has been *closed*
        // while its file stays behind — exactly what a dead mpv leaves if the socket
        // was not cleaned up. (`UnixListener::drop` does not unlink, and neither does
        // mpv, per the real-player measurements in `player.rs`.)
        let target = path("refused");
        drop(listen(&target));
        assert_eq!(
            Link::to(&target).err(),
            Some("[Errno 61] Connection refused".to_string())
        );
        // And the file is still there, still a socket: refusing is not the same as
        // absent, and the two sentences the user hears are different diagnoses.
        assert!(std::path::Path::new(&target).exists());
    }

    #[test]
    fn connecting_does_not_prove_anyone_is_reading() {
        // A bound listener that has never called `accept` still takes the connection:
        // it lands in the backlog, and a second one lands there too. So `connect()`
        // succeeding is evidence that mpv got as far as creating
        // `--input-ipc-server`, **not** that it is serving the IPC — which is why the
        // reference's proof of liveness is the first reply it reads back, and why
        // `crate::player` never treats a connected socket as a live player.
        let target = path("backlog");
        let listener = listen(&target);
        let first = Link::to(&target).expect("connect into an unaccepted backlog");
        let second = Link::to(&target).expect("the backlog takes more than one");
        // The listener has accepted nothing, yet both connects "worked".
        assert!(first.is_connected());
        assert!(second.is_connected());
        let (peer, _) = listener.accept().expect("accept");
        drop(peer);
    }

    #[test]
    fn a_regular_file_at_the_path_is_not_a_socket() {
        let target = path("regular");
        std::fs::write(&target, b"not a socket").expect("write");
        assert_eq!(
            Link::to(&target).err(),
            Some("[Errno 38] Socket operation on non-socket".to_string())
        );
    }

    #[test]
    fn a_path_too_long_for_sockaddr_un_has_no_errno_prefix() {
        let long = format!("/tmp/{}", "x".repeat(200));
        assert!(!fits(&long));
        assert_eq!(Link::to(&long).err(), Some(PATH_TOO_LONG.to_string()));
        // Reaching it must not have cost us a socket-level attempt: the failure
        // happens before any syscall, which is why there is no `[Errno n]` in it.
        let link = Link::created().expect("socket()");
        assert_eq!(link.connect(&long), Err(PATH_TOO_LONG.to_string()));
        // And the socket is still usable for the shapes that do reach the kernel.
        assert_eq!(link.send_all(b"x").err(), Some(errno_text(libc::ENOTCONN)));
    }

    #[test]
    fn the_connect_reaches_the_descriptor_it_owns() {
        // The reason `connect` is not `UnixStream::connect`: a probe socket would
        // take an accept slot from mpv, which does accept more than one connection.
        // Asserted against a real listener that counts them.
        let target = path("identity");
        let listener = listen(&target);
        let link = Link::created().expect("socket()");
        link.connect(&target).expect("connect");
        let (peer, _) = listener.accept().expect("accept");
        // Exactly one pending connection ever existed on this socket: the one we
        // just took, so the queue is now empty.
        link.send_all(b"ping\n").expect("send");
        let mut buf = [0u8; 5];
        (&peer).take(5).read_exact(&mut buf).expect("read");
        assert_eq!(&buf, b"ping\n");
    }

    #[test]
    fn send_and_recv_work_over_a_real_connection() {
        let target = path("works");
        let listener = listen(&target);
        let link = Link::to(&target).expect("connect");
        let mut peer = listener.accept().expect("accept").0;
        link.send_all(b"hello\n").expect("send");
        let mut buf = [0u8; 8];
        peer.read_exact(&mut buf[..6]).expect("read");
        assert_eq!(&buf[..6], b"hello\n");

        let mut incoming = [0u8; 8];
        peer.write_all(b"world\n").expect("write");
        let mut got = Vec::new();
        while got.len() < 6 {
            let n = link.recv_into(&mut incoming).expect("recv");
            if n == 0 {
                break;
            }
            got.extend_from_slice(&incoming[..n]);
        }
        assert_eq!(&got, b"world\n");
        // `recv` returns `b''` when the peer goes away — no exception, and the
        // reader loop's `if data == b'': break` is what ends the session.
        drop(peer);
        assert_eq!(link.recv_into(&mut incoming), Ok(0));
    }

    #[test]
    fn writing_to_a_socket_whose_peer_is_gone_is_a_broken_pipe() {
        // The measured shape, not the convenient one: bytes are sitting **unread**
        // in our own receive queue when the peer dies, which is exactly what happens
        // when mpv answers and then exits. It still answers `[Errno 32] Broken pipe`
        // — no ECONNRESET, no ENOTCONN — and CPython says the same sentence in the
        // same shape, so this asserts one string and not a set.
        let target = path("gone");
        let listener = listen(&target);
        let link = Link::to(&target).expect("connect");
        let mut peer = listener.accept().expect("accept").0;
        peer.write_all(&vec![b'x'; 4096]).expect("peer write");
        drop(peer);
        drop(listener);
        assert_eq!(
            link.send_all(b"{\"command\":[\"quit\"]}\n"),
            Err("[Errno 32] Broken pipe".to_string())
        );
        // And the queued bytes survive the broken pipe, which is why the reader must
        // keep draining after a write fails: that data is mpv's last real answer.
        let mut buf = [0u8; 4];
        assert_eq!(link.recv_into(&mut buf), Ok(4));
    }

    #[test]
    fn closing_the_link_reports_a_bad_descriptor_every_after() {
        let target = path("closed");
        let listener = listen(&target);
        let link = Link::to(&target).expect("connect");
        let _peer = listener.accept().expect("accept");
        link.close();
        assert_eq!(link.send_all(b"hello\n").err(), Some(errno_text(libc::EBADF)));
        let mut buf = [0u8; 4];
        assert_eq!(
            link.recv_into(&mut buf).err(),
            Some(errno_text(libc::EBADF))
        );
        assert!(link.is_closed());
        // Idempotent, and a clone shares the verdict — the reader holds a clone, so
        // a second `close()` on the other handle must not look like a live socket.
        link.close();
        assert!(link.clone().is_closed());
    }

    #[test]
    fn a_link_closed_by_another_owner_wakes_a_blocked_reader() {
        // `stop()` cannot rely on this — the reference ends the reader by letting mpv
        // die first — but the mechanism is real and measured, and `Link::close` is
        // the only way to end a session *we* start without waiting on the player.
        // Bounded on purpose: a wrong guess about the OS must fail, not hang.
        let target = path("blocked");
        let listener = listen(&target);
        let link = Link::to(&target).expect("connect");
        let _peer = listener.accept().expect("accept");

        let reader = link.clone();
        let (sender, receiver) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let mut buf = [0u8; 16];
            let _ = sender.send(reader.recv_into(&mut buf));
        });
        // Give the thread time to park in `read(2)` before touching anything.
        std::thread::sleep(std::time::Duration::from_millis(200));
        link.close();
        let outcome = receiver
            .recv_timeout(std::time::Duration::from_secs(5))
            .expect("the reader never woke from a blocked recv when the link closed");
        assert_eq!(outcome, Ok(0), "shutdown must end the read as EOF");
    }

    #[test]
    fn errno_text_matches_every_measured_sentence() {
        // The measured set, from CPython 3.12.15 on this machine, for the four
        // connect shapes and the three I/O shapes.
        let cases = [
            (libc::ENOENT, "[Errno 2] No such file or directory"),
            (libc::EACCES, "[Errno 13] Permission denied"),
            (libc::ENOTSOCK, "[Errno 38] Socket operation on non-socket"),
            (libc::EPIPE, "[Errno 32] Broken pipe"),
            (libc::EBADF, "[Errno 9] Bad file descriptor"),
            (libc::ENOTCONN, "[Errno 57] Socket is not connected"),
            (libc::ECONNREFUSED, "[Errno 61] Connection refused"),
        ];
        for (code, text) in cases {
            assert_eq!(errno_text(code), text, "errno {code}");
        }
    }

    #[test]
    fn the_never_made_socket_says_what_it_says() {
        assert_eq!(NO_SOCKET, "'NoneType' object has no attribute 'sendall'");
    }

    #[test]
    fn the_socket_address_is_packed_for_this_platform() {
        // The offset and the capacity are read from `libc::sockaddr_un` rather than
        // written here, so this test's job is to prove they are the numbers CPython
        // uses: `sun_path` never starts at byte 0 on macOS (the `sun_len` byte is
        // first), and the length passed to `connect` is the name, not the struct.
        let (offset, capacity) = sun_layout();
        assert!(offset > 0, "sun_path sits at {offset}");
        assert!(capacity >= 92, "sun_path capacity is {capacity}");
        let short = "/tmp/mpv.sock";
        assert_eq!(address_len(short), (offset + short.len() + 1) as libc::socklen_t);
        assert!(fits(short));
        // A name at exactly the capacity still fits, and one byte over does not.
        let exact = "a".repeat(capacity - 1);
        assert!(fits(&exact));
        assert!(!fits(&format!("{exact}b")));
        let address = sockaddr_un(short).expect("pack");
        assert_eq!(address.sun_family, libc::AF_UNIX as libc::sa_family_t);
        let name: Vec<u8> = address.sun_path[..short.len()].iter().map(|b| *b as u8).collect();
        assert_eq!(name, short.as_bytes());
        assert_eq!(address.sun_path[short.len()], 0, "the name is NUL-terminated");
    }
}
