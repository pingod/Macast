//! The one moving part of mpv's IPC that is not pure: a connected socket.
//!
//! This module is deliberately small. Everything about it exists to answer one
//! question in the same words the reference does: *what does the log say when the
//! transport is not there?* Every sentence it can produce was read off
//! CPython 3.12.15 on this machine rather than composed — see the table in
//! [`Link`]'s docs and the tests at the bottom of this module, which assert the
//! whole string, brackets and capitalisation included.
//!
//! Both platforms the reference runs on are implemented here, and the split is
//! honest about which half has seen a machine: the Unix side was measured twice
//! (CPython and this module, on this Mac). The Windows side is a **port, not a
//! measurement** — it calls the same `CreateFile` / `ReadFile` / `WriteFile` the
//! reference's `_winapi` module does, and every sentence it can produce was read
//! off CPython's `multiprocessing.connection.PipeConnection` source line by line,
//! but no named pipe has been exercised here. Where the two sides genuinely
//! disagree, the disagreement is written into the table below rather than smoothed
//! over. Windows std ships **no** `named_pipe` module in this toolchain, so this
//! backend goes to the operating system directly through `windows-sys`, which is
//! also what CPython does.

#[cfg(unix)]
use std::io::{ErrorKind, Read, Write};
#[cfg(unix)]
use std::net::Shutdown;
#[cfg(unix)]
use std::os::fd::AsRawFd;
#[cfg(unix)]
use std::os::unix::io::FromRawFd;
#[cfg(unix)]
use std::os::unix::net::UnixStream;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

#[cfg(windows)]
use std::sync::Mutex;
// `PCWSTR` is not imported: it is a plain `*const u16`, so the names this module
// builds are handed over as `Vec<u16>::as_ptr()` and naming the alias would be a
// cast that says nothing the type does not already say.
#[cfg(windows)]
use windows_sys::Win32::Foundation::{
    GetLastError, CloseHandle, ERROR_BROKEN_PIPE, ERROR_MORE_DATA, ERROR_NO_DATA,
    GENERIC_READ, GENERIC_WRITE, INVALID_HANDLE_VALUE, HANDLE,
};
#[cfg(windows)]
use windows_sys::Win32::Storage::FileSystem::{
    CreateFileW, OPEN_EXISTING, ReadFile, WriteFile,
};
#[cfg(windows)]
use windows_sys::Win32::System::Diagnostics::Debug::{
    FORMAT_MESSAGE_FROM_SYSTEM, FORMAT_MESSAGE_IGNORE_INSERTS, FormatMessageW,
};
#[cfg(windows)]
use windows_sys::Win32::System::Pipes::PeekNamedPipe;

/// The text `send_command` logs when the socket was never made.
///
/// `self.ipc_sock` starts as `None` in `__init__`, so a command sent before the
/// IPC thread's first `socket.socket(...)` raises
/// `AttributeError: 'NoneType' object has no attribute 'sendall'`, which the
/// `except Exception` arm turns into a line. Measured verbatim on macOS.
///
/// The method name differs per platform because the two platforms hold different
/// objects there: `mpv.py` assigns `PipeConnection(handler)` on Windows and a raw
/// `socket.socket` everywhere else, and `send_command` calls `send_bytes` on the
/// first and `sendall` on the second (lines 367–386). So the Windows side of this
/// constant names the method that was actually being asked for — read off
/// `PipeConnection`'s own public surface, not measured here.
#[cfg(unix)]
pub const NO_SOCKET: &str = "'NoneType' object has no attribute 'sendall'";
#[cfg(windows)]
pub const NO_SOCKET: &str = "'NoneType' object has no attribute 'send_bytes'";

/// `str(e)` for an `OSError` that carries an error number.
///
/// The signature is the same on both platforms; the prefix and the words' source
/// are not. Unix prints `[Errno 2] No such file or directory`, reached through
/// `strerror(3)`. Windows prints `[WinError 2] The system cannot find the file
/// specified.`, reached through `FormatMessageW` — CPython's `OSError.__str__`
/// prefers `[WinError %d]` whenever the exception has a `winerror`, and every
/// `_winapi` failure arrives with one, because it is raised through
/// `PyErr_SetFromWindowsErr`. The number itself is the same fact either way
/// (`errno` on Unix, `GetLastError()` on Windows); only who words it differs.
///
/// **No filename is ever appended**, and that is not an oversight: every socket
/// failure measured below has `e.filename is None` — `connect()` does not put the
/// path into the exception, unlike `subprocess.Popen`, which does put the program
/// name into its own (see [`crate::player`]'s spawn text). CPython's
/// `OSError.__str__` only appends the filename when there is one, so appending it
/// here would be quoting a file the error never named.
///
/// The Unix form is **measured**; the Windows form is read off CPython's text and
/// has not been printed by a machine here, including how much trailing punctuation
/// `FormatMessage` leaves on the sentence. So the Windows tests assert the
/// `[WinError n] ` prefix and a non-empty remainder, never the words.
pub fn errno_text(code: i32) -> String {
    #[cfg(windows)]
    {
        format!("[WinError {code}] {}", message_of(code as u32))
    }
    #[cfg(not(windows))]
    {
        format!("[Errno {code}] {}", strerror(code))
    }
}

/// The operating system's own words for an errno — the same `strerror(3)` that
/// CPython calls to build an `OSError`'s message.
#[cfg(unix)]
fn strerror(code: i32) -> String {
    let raw = unsafe { libc::strerror(code) };
    if raw.is_null() {
        return format!("Unknown error: {code}");
    }
    unsafe { std::ffi::CStr::from_ptr(raw) }
        .to_string_lossy()
        .into_owned()
}

/// The operating system's own words for a `GetLastError()` code, which is what
/// CPython puts after its `[WinError n]` prefix.
///
/// `FORMAT_MESSAGE_IGNORE_INSERTS` is not decoration: without it `FormatMessage`
/// expands `%1`-style insert placeholders out of the `arguments` array, and this
/// passes no arguments — the two choices belong together, so a message string can
/// never steer what we print about an error. The buffer is ours (a stack array),
/// not allocated by the system, so nothing needs freeing.
///
/// Trailing CR/LF is trimmed. `FormatMessage` ends its sentences with `\r\n`, and a
/// log line that continues onto a second line is a different thing from the
/// sentence CPython prints, whether or not CPython keeps those two characters.
#[cfg(windows)]
fn message_of(code: u32) -> String {
    // Generous: no system message of the size we ask about comes near it, and a
    // truncated one still reads, whereas a heap allocation for a log line would be
    // the same cost paid twice.
    const CAPACITY: usize = 512;
    let mut wide = [0u16; CAPACITY];
    // SAFETY: `wide` is a valid, adequately-sized `[u16]` we own, and its length is
    // passed to the API in the same units the API documents (characters, not
    // bytes). `lpsource` and `arguments` are null, which is what the
    // FROM_SYSTEM + IGNORE_INSERTS combination requires. The API writes at most
    // `CAPACITY` characters and reports how many, so the slice below never reads
    // past what was written.
    let written = unsafe {
        FormatMessageW(
            FORMAT_MESSAGE_FROM_SYSTEM | FORMAT_MESSAGE_IGNORE_INSERTS,
            core::ptr::null(),
            code,
            0,
            wide.as_mut_ptr(),
            CAPACITY as u32,
            core::ptr::null(),
        )
    };
    if written == 0 {
        return format!("Unknown error: {code}");
    }
    String::from_utf16_lossy(&wide[..written as usize])
        .trim_end_matches(['\r', '\n'])
        .to_string()
}

/// `str(e)` for the transport this process has already closed.
///
/// Unix: `[Errno 9] Bad file descriptor`, measured — what CPython raises when a
/// closed descriptor is asked to send or receive.
///
/// Windows: the **bare** `handle is closed`, with no `[WinError n]` prefix at all.
/// `multiprocessing.connection.PipeConnection._check_closed` raises
/// `OSError("handle is closed")`, and `close()` nulls `_handle`, so the next
/// `send_bytes` / `recv_bytes` / `poll` hits that check before it touches the pipe.
/// An `OSError` built from a message alone has no errno and no `winerror`, so there
/// is no prefix to print — the sentence *is* the whole of `str(e)`.
///
/// What is deliberately **not** here is the other `handle is closed` in that class,
/// `[Errno 32] handle is closed`: `_send_bytes` raises it when an overlapped write
/// comes back `ERROR_OPERATION_ABORTED`, i.e. when `_close()` cancelled a pending
/// `WriteFile`. This port never issues an overlapped write, so that branch is
/// unreachable — reaching for it would be quoting a state the code cannot be in
/// (see the divergence notes in [`Link`]'s docs).
pub fn closed_text() -> String {
    #[cfg(windows)]
    {
        "handle is closed".to_string()
    }
    #[cfg(not(windows))]
    {
        errno_text(libc::EBADF)
    }
}

/// `str(e)` for the one socket failure that has no errno at all.
///
/// A path that will not fit in `sockaddr_un` raises `OSError('AF_UNIX path too
/// long')` before any syscall happens, so `errno` is `None` and there is no
/// `[Errno n]` prefix to print. Measured verbatim.
///
/// There is no Windows twin, because there is nothing to be too long *for*: a named
/// pipe has no fixed-size struct to overflow into — the name is a string handed to
/// `CreateFileW`, and an over-long or malformed one simply fails with a
/// `[WinError n]`, which [`errno_text`] already covers.
#[cfg(unix)]
pub const PATH_TOO_LONG: &str = "AF_UNIX path too long";

/// One endpoint of the JSON IPC line protocol.
///
/// The states this transport can be in are all different sentences, and the two
/// platforms do not have the same set of states — which is itself one of the
/// findings, not an omission.
///
/// Unix, where a socket is *made* and then *connected* as two separate calls:
///
/// | state | `send_all` | `recv_into` |
/// |---|---|---|
/// | never created | [`NO_SOCKET`] — the caller holds `None`, not a `Link` | (the reader loop is not running yet) |
/// | created, not connected | `[Errno 57] Socket is not connected` | macOS `[Errno 57]`; **Linux `[Errno 22] Invalid argument`** |
/// | connected | `[Errno 32] Broken pipe` once mpv is gone | `Ok(0)` — a clean EOF |
/// | closed by us | `[Errno 9] Bad file descriptor` | `[Errno 9] Bad file descriptor` |
///
/// Windows, where [`Link::connect`] is *both* calls at once — one `CreateFileW`
/// makes the handle and reaches mpv — so no
/// "created, not connected" middle state exists at all:
///
/// | state | `send_all` | `recv_into` |
/// |---|---|---|
/// | never opened | [`NO_SOCKET`] | (the reader loop is not running yet) |
/// | open, mpv gone | `[WinError 232] The pipe is being closed.` | `Ok(0)` — from `ERROR_BROKEN_PIPE` |
/// | closed by us | `handle is closed` — bare, no prefix | `handle is closed` |
///
/// The Unix column was measured twice on this Mac — once in CPython 3.12.15 and
/// once through this module — including the two shapes that look like they should
/// differ but do not: a peer that dies while bytes sit **unread in our own receive
/// queue** still answers `[Errno 32] Broken pipe` (not `ECONNRESET`, and the queued
/// bytes stay readable), which is why the broken-pipe test below asserts exactly one
/// sentence rather than accepting a set.
///
/// # The same errno table is not the same table on Linux
///
/// The Linux numbers were read off a real run of this module's own tests inside a
/// glibc container, not inferred, and they are the reason the Unix table above
/// carries a platform name on one row:
///
/// | fact | macOS | Linux |
/// |---|---|---|
/// | not connected, writing | `[Errno 57] Socket is not connected` | `[Errno 107] Transport endpoint is not connected` |
/// | not connected, reading | the same sentence | **`[Errno 22] Invalid argument`** |
/// | nothing listening | `[Errno 61] Connection refused` | `[Errno 111] Connection refused` |
/// | something else owns the path | `[Errno 38] Socket operation on non-socket` | `[Errno 88] Socket operation on non-socket` — **but see below** |
/// | no such path / broken pipe / closed fd | 2 / 32 / 9 | 2 / 32 / 9, identical |
///
/// Two of those are not just different numbers. First, `send` and `recv` **do not
/// answer alike on Linux** — reading an unconnected socket gives `EINVAL`, a
/// sentence this module never emits, because the reader thread only exists after a
/// successful connect. So the "they answer alike" shape in the second row of the
/// Unix table is a macOS fact, and the test that asserts it is gated accordingly.
/// Second, a **regular file** sitting at the path is not the same diagnosis on both
/// kernels: Linux answers `[Errno 111] Connection refused` — the *same sentence* as
/// "nothing is listening" — while macOS answers "not a socket". The kernel gets
/// there by a different road; the user hears one sentence either way, and on Linux
/// it is the less specific one.
///
/// And a trap worth naming: `[Errno 57]` is not a foreign spelling of "not
/// connected", it is **"Invalid slot"** on Linux. That is why
/// `the_not_connected_sentence_is_the_measured_one` asserts the macOS literal and is
/// gated to macOS rather than being loosened to "whatever this machine calls 57" —
/// the point of that test is that a specific string reaches the log, and a portable
/// assertion would stop making it.
///
/// # The Windows column is a port, not a measurement
///
/// Every Windows sentence above was read off CPython's
/// `multiprocessing.connection.PipeConnection` and the Win32 API's own documented
/// answers, line by line, and **no named pipe has been exercised on this machine** —
/// there is no Windows host in this development setup that runs this crate. The tests
/// for that half assert the shapes that need no operating system (the handle-less and
/// closed states, `[WinError n] ` prefixes, the UTF-16 packing) and deliberately do
/// **not** assert any English sentence the machine has not said here.
///
/// Four differences from the reference are written down rather than smoothed over:
///
/// 1. **A failed open lands on a different row.** The reference's `nt` branch calls
///    `CreateFile` and only then assigns `self.ipc_sock = PipeConnection(handler)`,
///    so the socket variable is never left half-made. A failed *first* connect
///    therefore leaves `None` and the next command logs [`NO_SOCKET`]; a failed
///    connect *after* an earlier session closed leaves that closed object in place
///    and logs `handle is closed`. This module reproduces the first of those and
///    notes the second as a narrow divergence: our slot is cleared, so a later
///    attempt says `NO_SOCKET` where CPython would say `handle is closed`.
/// 2. **Our `close()` cannot wake a parked reader.** See below.
/// 3. **Two sentences the reference prints are bugs, and we do not quote them.** The
///    reference's `_send_bytes` finishes with `assert err == 0`, so a write to a pipe
///    mpv has already closed raises an `AssertionError` whose `str()` is **empty** —
///    the log reads `mpv ipc socket reconnecting: `. We answer
///    `[WinError 232] The pipe is being closed.` instead. Its `_recv_bytes` turns
///    `ERROR_BROKEN_PIPE` into `raise EOFError`, also empty, and the reader loop has
///    no arm for it, so EOF on a pipe is an unhandled exception in the reference
///    where ours is the normal `Ok(0)`. Quoting a bug is not quoting a sentence.
/// 4. **`FILE_FLAG_OVERLAPPED` is deliberately not passed.** `PipeConnection`'s own
///    docstring says its handles *must* have been created with it, and the reference
///    does; this backend issues synchronous `ReadFile`/`WriteFile` and needs no
///    overlapped handle, which also removes the completion-port machinery — and the
///    `ERROR_OPERATION_ABORTED` state that comes with it — from the set of things this
///    module can report.
///
/// One argument this port copies without understanding its consequence: the reference
/// opens with **share mode 0**. Whether that — rather than the pipe server's own
/// instance bookkeeping — is what stops a second simultaneous open is something no
/// machine in this repository can ask, so it is written as an open question and not as
/// a reason. And no test here asks it either: the one server fixture in this file
/// (`Server`, used by `a_connected_pipe_moves_bytes_in_both_directions_and_ends`) is
/// created with `nMaxInstances = 1` and `FILE_FLAG_FIRST_PIPE_INSTANCE` and is handed
/// exactly one client, while the Windows cases elsewhere never reach a pipe a server
/// is standing on. So this backend's behaviour under a second simultaneous open is
/// **untested**, and reading share mode 0 as a deliberate exclusion would be reading a
/// consequence this module has not observed.
///
/// # The one place this is not the operating system
///
/// [`Link::close`] sets a flag rather than releasing the transport, on both
/// platforms, because an `Arc` cannot hand its resource out from behind a shared
/// reference, and closing something two owners both believe they hold would be a bug
/// in this port, not a fidelity win. The flag then produces the same sentence
/// CPython produces from a genuinely closed transport — `[Errno 9] Bad file
/// descriptor` on Unix, `handle is closed` on Windows — so the log line matches; the
/// difference is that the fd or handle stays live until the last owner drops it.
///
/// On macOS that difference is measurable and mostly harmless: `shutdown(2)` on an
/// `AF_UNIX` stream *does* wake a thread parked in `read(2)` and hands it EOF, and
/// that is true whether the waking side asks for `SHUT_RD` or `SHUT_RDWR`
/// (`a_link_closed_by_another_owner_wakes_a_blocked_reader`). Dropping a *non-final*
/// handle, by contrast, wakes nothing. On Windows the flag alone **cannot** wake a
/// blocked reader: we never issue overlapped I/O, and `CancelIo` cancels only the
/// calling thread's pending requests, so a reader parked in `ReadFile` stays parked.
/// That test therefore stays `#[cfg(unix)]` — not because the Unix behaviour is
/// universal, but because this is the one place the two sides genuinely differ.
///
/// **None of that is what ends the reader in `stop()`.** The reference joins the
/// player thread *before* it sets `ipc_running = False`, and its
/// `self.ipc_sock.close()` sits at the bottom of the outer loop — code that only
/// runs once the inner read loop has already broken. So the session ends because
/// **mpv exits, its half closes, and the read returns empty** — `b''` on Unix,
/// `ERROR_BROKEN_PIPE` on Windows. [`Link::close`] remains the right thing for a
/// shutdown we start ourselves, but a caller must not design `stop()` around it;
/// [`crate::player`] joins the player first for exactly this reason.
#[cfg(unix)]
#[derive(Debug)]
pub struct Link {
    stream: Arc<UnixStream>,
    closed: AtomicBool,
}

/// Windows keeps the same three-state idea in a different container, because the
/// thing it holds can appear and disappear *after* the object exists:
/// `CreateFileW` either hands back a handle or hands back `INVALID_HANDLE_VALUE`,
/// so the "created, not connected" row of the table above is an **empty slot**
/// rather than a socket with nothing behind it.
///
/// `Mutex` rather than nothing, because a handle is a value we can install later,
/// unlike a Unix descriptor that was there from the first line of the constructor.
/// The lock is never held across `ReadFile` or `WriteFile` — see
/// [`Link::current_handle`] — so a reader parked in a blocking read cannot hold up
/// a sender, which is the same reason `send_command` and the IPC thread can share
/// one `self.ipc_sock` in the reference.
#[cfg(windows)]
#[derive(Debug)]
pub struct Link {
    pipe: Arc<Mutex<Option<Pipe>>>,
    closed: AtomicBool,
}

/// A named-pipe handle that closes itself.
///
/// Stored as `isize` instead of `HANDLE` so the type is `Send + Sync` without an
/// `unsafe impl`: `windows_sys`'s `HANDLE` is a raw pointer, and raw pointers are
/// neither, even though a pipe handle is exactly as shareable between threads as
/// the integer Windows uses to name it. Every call site casts, and nowhere else
/// does the integer have a meaning.
#[cfg(windows)]
#[derive(Debug)]
struct Pipe(isize);

#[cfg(windows)]
impl Drop for Pipe {
    fn drop(&mut self) {
        // SAFETY: a `Pipe` is only ever built from a `CreateFileW` return value
        // that was checked against `INVALID_HANDLE_VALUE`, so this is a live handle
        // belonging to this process, and it is dropped exactly once because the
        // `Option` it lives in owns it. Closing the `-1`/`0` sentinels would return
        // an error we would ignore anyway; never reaching them is the invariant.
        let _ = unsafe { CloseHandle(self.0 as HANDLE) };
    }
}

#[cfg(unix)]
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
            return Err(closed_text());
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
            return Err(closed_text());
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

    /// Whether this descriptor has a peer, asked of the kernel with
    /// `getpeername(2)` — the same fact a failed write reports as `ENOTCONN`
    /// (`[Errno 57] Socket is not connected` on macOS, `[Errno 107] Transport
    /// endpoint is not connected` on Linux).
    ///
    /// A connection parked in a listener's **backlog** counts as having a peer: the
    /// three-way handshake is done from this side's point of view long before the
    /// other end calls `accept`. So `true` here means "mpv created its IPC socket",
    /// not "mpv is serving it" — see the note on [`Link::connect`].
    ///
    /// This reads the kernel and **not** [`Link::is_closed`], so it is a question about
    /// descriptors, not about our own decisions. Measured on this Mac (2026-10-14) for a
    /// stream unix socket that had been connected: after our own [`Link::close`] runs
    /// `shutdown(2)`, and again after the peer closes, `getpeername(2)` raises
    /// `[Errno 22] Invalid argument` rather than `ENOTCONN`, so `false` answers both.
    /// What it raises on Linux has not been measured here, which is why callers that need
    /// "did *we* close this" ask [`Link::is_closed`] instead of relying on this alone.
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

#[cfg(windows)]
impl Link {
    /// The Windows shape of "`socket()` ran, `connect()` did not" is an **empty
    /// slot**, because there is no call that makes a handle without also reaching
    /// the server: `CreateFileW` on a named pipe both opens the handle and
    /// completes the connection.
    ///
    /// Returns `Result` only so [`Link::to`] reads the same on both platforms. The
    /// `Err` half is unreachable here — allocating an `Arc` and a `Mutex` does not
    /// fail — whereas on Unix it answers for `socket(2)`. A caller that branches on
    /// it is not wrong, just never taking that branch on Windows.
    pub fn created() -> Result<Self, String> {
        Ok(Self {
            pipe: Arc::new(Mutex::new(None)),
            closed: AtomicBool::new(false),
        })
    }

    /// `CreateFileW` for the pipe, which is the reference's `_winapi.CreateFile`
    /// call at `mpv.py:390` with two deliberate differences, both recorded in the
    /// module doc:
    ///
    /// - **No `FILE_FLAG_OVERLAPPED`.** The reference passes it, and
    ///   `PipeConnection` is built for that handle: its `_send_bytes` finishes every
    ///   write with `ov.GetOverlappedResult(True)` and its `_recv_bytes` reads
    ///   `ERROR_MORE_DATA` as "there is a whole message waiting, give me the rest".
    ///   A synchronous handle cannot do either of those things, so the two calls are
    ///   ours rather than CPython's, and the sentences they return on failure are
    ///   ours too — see [`Link::send_all`] and [`Link::recv_into`].
    /// - **Share mode `0`.** The reference's third argument, copied without changing it.
    ///   What actually makes mpv's Windows IPC one client at a time is on mpv's side —
    ///   it creates the pipe with a single instance — so no test here can tell these
    ///   two explanations apart, and this port does not claim to.
    pub fn connect(&self, path: &str) -> Result<(), String> {
        let name = wide(path)?;
        // SAFETY: `name` is a NUL-terminated UTF-16 buffer that outlives this call,
        // and every pointer argument is either NULL or a null pointer of the type
        // the API asks for. A null `lpSecurityAttributes` with `bInheritHandle`
        // unset is what CPython passes too.
        let handle = unsafe {
            CreateFileW(
                name.as_ptr(),
                GENERIC_READ | GENERIC_WRITE,
                0,
                core::ptr::null(),
                OPEN_EXISTING,
                0,
                core::ptr::null_mut(),
            )
        };
        if handle == INVALID_HANDLE_VALUE {
            // The slot stays empty, which is how a failed reconnect keeps the
            // reference's `None`-valued `ipc_sock`: the next `send_command` hears
            // `NO_SOCKET` rather than something about a handle.
            return Err(errno_text(unsafe { GetLastError() } as i32));
        }
        *self.slot() = Some(Pipe(handle as isize));
        Ok(())
    }

    /// The reference's `sendall`, spelled out as the loop Windows does not give us
    /// for free: `WriteFile` reports the bytes it accepted, and a pipe server that
    /// is not reading can accept fewer than asked.
    ///
    /// A zero-byte "success" is answered rather than looped on forever. It should not
    /// happen — `WriteFile` on a byte pipe either writes or fails — but the only
    /// alternative to noticing is spinning here after mpv has gone, which is
    /// [`ERROR_NO_DATA`]'s own words ("the pipe has been ended") in a shape no test
    /// would catch.
    pub fn send_all(&self, bytes: &[u8]) -> Result<(), String> {
        if self.closed.load(Ordering::SeqCst) {
            return Err(closed_text());
        }
        let handle = match self.current_handle() {
            Some(handle) => handle,
            None => return Err(NO_SOCKET.to_string()),
        };
        let mut sent = 0usize;
        while sent < bytes.len() {
            let mut written = 0u32;
            // SAFETY: the slice lives for the call, `written` is a live `u32` we
            // read back, and a null `lpOverlapped` is required because this handle
            // was opened without `FILE_FLAG_OVERLAPPED`.
            let ok = unsafe {
                WriteFile(
                    handle,
                    bytes[sent..].as_ptr(),
                    (bytes.len() - sent) as u32,
                    &mut written,
                    core::ptr::null_mut(),
                )
            };
            if ok == 0 {
                return Err(errno_text(unsafe { GetLastError() } as i32));
            }
            if written == 0 {
                return Err(errno_text(ERROR_NO_DATA as i32));
            }
            sent += written as usize;
        }
        Ok(())
    }

    /// One `ReadFile`, which is the whole of the reference's read side for a byte
    /// pipe: mpv writes a line at a time and `start_ipc` reassembles by looking for
    /// the trailing `\n`, so no framing belongs here.
    ///
    /// `Ok(0)` is the `b''` that breaks that loop — on Windows mpv's side of the pipe
    /// going away is `ERROR_BROKEN_PIPE`, and translating it into `0` rather than an
    /// error is what keeps `stop()` ending the reader the same way on both platforms.
    pub fn recv_into(&self, buf: &mut [u8]) -> Result<usize, String> {
        if self.closed.load(Ordering::SeqCst) {
            return Err(closed_text());
        }
        if buf.is_empty() {
            return Ok(0);
        }
        let handle = match self.current_handle() {
            Some(handle) => handle,
            None => return Err(NO_SOCKET.to_string()),
        };
        let mut read = 0u32;
        // SAFETY: as in [`Link::send_all`] — the buffer outlives the call, the count
        // is a live `u32`, and no overlapped structure is passed.
        let ok = unsafe {
            ReadFile(
                handle,
                buf.as_mut_ptr(),
                buf.len() as u32,
                &mut read,
                core::ptr::null_mut(),
            )
        };
        if ok != 0 {
            return Ok(read as usize);
        }
        let code = unsafe { GetLastError() };
        if code == ERROR_BROKEN_PIPE {
            return Ok(0);
        }
        if code == ERROR_MORE_DATA {
            // Defensive arm, and it should never fire: `ERROR_MORE_DATA` is what a
            // *message-mode* pipe returns when a read stops at a message boundary,
            // and mpv's pipe is byte-mode. Kept because guessing wrong here would
            // turn a partial read into an error the reader logs as a reconnect.
            return Ok(read as usize);
        }
        Err(errno_text(code as i32))
    }

    /// Marks the session over. It does **not** `CloseHandle`, for the same reason the
    /// Unix [`Link::close`] does not `close(fd)`: another owner may still be holding
    /// this `Arc`, and the handle goes when the last of them drops.
    ///
    /// It also cannot wake a thread parked in `ReadFile`, because cancelling an
    /// overlapped read is not available on a synchronous handle. That is the
    /// documented cost in the module doc — the session ends when mpv exits, not when
    /// this runs.
    pub fn close(&self) {
        self.closed.store(true, Ordering::SeqCst);
    }

    /// Whether this handle **was opened successfully**, which is not the same
    /// question the Unix half asks. `PeekNamedPipe` is the cheapest way to ask the
    /// pipe anything at all, and CPython's `PipeConnection._poll` uses the same call
    /// — but it only fails once the handle is invalid, so a server that has closed
    /// its end can still answer here, whereas Unix's `getpeername(2)` reports the
    /// lost peer. Read it as "we have a connection to talk about".
    ///
    /// The byte count is asked for rather than passed as NULL because that is what
    /// makes the call a read probe at all; the answer is deliberately discarded.
    pub fn is_connected(&self) -> bool {
        if self.closed.load(Ordering::SeqCst) {
            return false;
        }
        let handle = match self.current_handle() {
            Some(handle) => handle,
            None => return false,
        };
        let mut available = 0u32;
        // SAFETY: three arguments are unused and given null pointers of their own
        // types; the one output we do read is a live `u32`.
        unsafe {
            PeekNamedPipe(
                handle,
                core::ptr::null_mut(),
                0,
                core::ptr::null_mut(),
                &mut available,
                core::ptr::null_mut(),
            ) != 0
        }
    }

    /// The lock, with poison forgiven: the only way to poison it is to panic while
    /// installing or dropping a handle, and a reader on the other side of that panic
    /// is better off seeing an empty slot than `unwrap`ing its way into the caller's
    /// error string.
    fn slot(&self) -> std::sync::MutexGuard<'_, Option<Pipe>> {
        self.pipe
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// Copy the handle out and **release the lock before any syscall**. A reader
    /// parked in `ReadFile` must not be able to hold up a sender, which is the whole
    /// reason the two threads in the reference can share one `self.ipc_sock`.
    fn current_handle(&self) -> Option<HANDLE> {
        self.slot().as_ref().map(|pipe| pipe.0 as HANDLE)
    }
}

/// The two methods that mean exactly the same thing on both platforms, so that
/// neither platform gets to drift from the other's wording.
impl Link {
    /// A connection already made to `path`, for a caller that does not want to keep
    /// the half-made object around.
    ///
    /// Note that the reference does *not* use this shape: `start_ipc` assigns
    /// `self.ipc_sock` before connecting, so a failed connect leaves the published
    /// object in place and the next `send_command` answers for it — `[Errno 57]` on
    /// macOS, `[Errno 107]` on Linux, `NO_SOCKET` on Windows. Use [`Link::created`]
    /// plus [`Link::connect`] to reproduce that; this helper discards the object on
    /// failure, which is a different observable.
    pub fn to(path: &str) -> Result<Self, String> {
        let link = Self::created()?;
        link.connect(path)?;
        Ok(link)
    }

    /// Whether [`Link::close`] has run. `stop()` reads this to know whether the
    /// reader thread has finished with the socket.
    pub fn is_closed(&self) -> bool {
        self.closed.load(Ordering::SeqCst)
    }
}

/// Another owner for the same socket: `self.ipc_sock` being read by the IPC thread
/// and written by `send_command` at the same time. One object, two handles, which is
/// what a duplicated descriptor would *not* give us — either copy closing
/// independently would leave the other side's `recv` waiting forever.
#[cfg(unix)]
impl Clone for Link {
    fn clone(&self) -> Self {
        Self {
            stream: Arc::clone(&self.stream),
            closed: AtomicBool::new(self.closed.load(Ordering::SeqCst)),
        }
    }
}

/// The same idea one level removed: cloning shares the *slot*, so a `connect()` on
/// one owner's [`Link`] becomes visible to the other, which is exactly how the
/// reference's single `self.ipc_sock` behaves when `start_ipc` reassigns it after a
/// failed read. The `closed` flag is deliberately **not** shared, matching Unix.
#[cfg(windows)]
impl Clone for Link {
    fn clone(&self) -> Self {
        Self {
            pipe: Arc::clone(&self.pipe),
            closed: AtomicBool::new(self.closed.load(Ordering::SeqCst)),
        }
    }
}

/// Turn an `io::Error` into the `str(e)` CPython would have.
#[cfg(unix)]
fn text_of(err: &std::io::Error) -> String {
    match err.raw_os_error() {
        Some(code) => errno_text(code),
        None => err.to_string(),
    }
}

/// The errno set by the libc call that just failed. Read immediately after the
/// call, because anything in between can overwrite it.
#[cfg(unix)]
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
#[cfg(unix)]
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
#[cfg(unix)]
fn fits(path: &str) -> bool {
    let (_, capacity) = sun_layout();
    path.len() + 1 <= capacity
}

/// `sockaddr_un` for a path, zeroed apart from the family and the name.
///
/// The reference is not careful here — `socket.socket().connect(str)` goes through
/// CPython, which does this same packing — so the layout is only reproduced to keep
/// `connect(2)` honest about where the name ends.
#[cfg(unix)]
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
#[cfg(unix)]
fn address_len(path: &str) -> libc::socklen_t {
    let (offset, _) = sun_layout();
    (offset + path.len() + 1) as libc::socklen_t
}

/// UTF-16, NUL-terminated, for the `lpFileName` of a Windows pipe open.
///
/// Windows names are UTF-16 at the API boundary, so the portability cost of a pipe
/// name is exactly this one conversion. An embedded NUL is refused with CPython's
/// words rather than truncated: `CreateFileW` stops at the first one, so a name
/// carrying it would silently open a *different* pipe than the caller asked for.
/// In practice unreachable from our callers — `command::ipc_socket_path` builds the
/// name from a fixed prefix and digits — but a silent redirect is the wrong failure.
#[cfg(windows)]
fn wide(path: &str) -> Result<Vec<u16>, String> {
    if path.contains('\0') {
        return Err("ValueError: embedded null byte".to_string());
    }
    let mut name: Vec<u16> = path.encode_utf16().collect();
    name.push(0);
    Ok(name)
}

/// Unix's behaviour, measured on the two platforms it can be measured on. The
/// sentences are platform-specific *numbers*, so the cases below are split by
/// number rather than by "which OS is this test running on": a Linux kernel that
/// answers the same *words* as macOS must still be asked for its own errno.
#[cfg(all(test, unix))]
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
        // `sendall` and the reader's `recv` have to answer. On macOS they answer
        // alike. On Linux they do not: `recv` there gives EINVAL, because the
        // socket has no address to read from at all, while `sendto` is the one the
        // kernel can say "not connected" about. Measured 2026-10-07 in the glibc
        // container (`/tmp/linux-baseline-3.txt`), not reasoned.
        let link = Link::created().expect("socket()");
        assert_eq!(
            link.send_all(b"{\"command\":[]}\n"),
            Err(errno_text(libc::ENOTCONN))
        );
        let mut buf = [0u8; 16];
        if cfg!(target_os = "linux") {
            assert_eq!(
                link.recv_into(&mut buf),
                Err("[Errno 22] Invalid argument".to_string()),
                "Linux answers the read half with EINVAL, not ENOTCONN"
            );
        } else {
            assert_eq!(
                link.recv_into(&mut buf),
                Err("[Errno 57] Socket is not connected".to_string())
            );
        }
    }

    #[test]
    fn the_not_connected_sentence_is_the_measured_one() {
        // 57 is macOS' ENOTCONN, 107 is Linux'. Both are written out rather than
        // reached through `libc::ENOTCONN` above so that a platform with a
        // different number cannot quietly redefine what the log says — this
        // assertion is the machine asking the reference's sentence, not the port
        // restating its own constant. The two sentences are *not* the same words:
        // macOS says "Socket is not connected", Linux says "Transport endpoint is
        // not connected", and the reference's log line therefore differs by kernel.
        if cfg!(target_os = "linux") {
            assert_eq!(
                errno_text(107),
                "[Errno 107] Transport endpoint is not connected"
            );
        } else {
            assert_eq!(errno_text(57), "[Errno 57] Socket is not connected");
        }
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
            Some(errno_text(libc::ECONNREFUSED)),
            "the refused sentence must come from this kernel's ECONNREFUSED"
        );
        // And the file is still there, still a socket: refusing is not the same as
        // absent, and the two sentences the user hears are different diagnoses.
        // That second half is the load-bearing one on Linux, where ECONNREFUSED is
        // 111 and a *regular* file at the path answers with the very same number —
        // see `a_regular_file_at_the_path_is_not_a_socket`.
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
        if cfg!(target_os = "linux") {
            // Linux gives ECONNREFUSED (111) here: `connect()` on an AF_UNIX socket
            // whose path names an inode that is not a socket never reaches ENOTSOCK,
            // because the kernel's first failure is "nothing is listening at that
            // name". Measured, 2026-10-07.
            //
            // The consequence is a real one for whoever reads the log: on Linux the
            // two diagnoses — "mpv left a dead socket behind" and "something that is
            // not mpv's socket is sitting at that path" — collapse into the *same*
            // sentence. macOS keeps them apart (61 vs 38). So the number alone is
            // not a diagnosis on Linux, and this comment is the only place that says
            // so in code; `a_socket_file_with_no_listener_refuses` above is the
            // other half of the pair, and on Linux the two are indistinguishable
            // from their replies alone.
            assert_eq!(
                Link::to(&target).err(),
                Some("[Errno 111] Connection refused".to_string())
            );
        } else {
            assert_eq!(
                Link::to(&target).err(),
                Some("[Errno 38] Socket operation on non-socket".to_string())
            );
        }
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
        // The measured set, for the four connect shapes and the three I/O shapes.
        // Split by *whether the number is the same everywhere*, because that is the
        // thing this function can get wrong on a second kernel: four of these codes
        // are the same number and the same words on macOS and Linux, and three are
        // not — so the three that differ are asked for their own platform's sentence
        // rather than folded into one "close enough" literal.
        //
        // macOS words measured from CPython 3.12.15 on this machine; Linux words
        // measured in the glibc container on 2026-10-07 (`/tmp/linux-baseline-3.txt`).
        let common = [
            (libc::ENOENT, "[Errno 2] No such file or directory"),
            (libc::EACCES, "[Errno 13] Permission denied"),
            (libc::EPIPE, "[Errno 32] Broken pipe"),
            (libc::EBADF, "[Errno 9] Bad file descriptor"),
        ];
        for (code, text) in common {
            assert_eq!(errno_text(code), text, "errno {code}");
        }
        if cfg!(target_os = "macos") {
            let macos = [
                (libc::ENOTSOCK, "[Errno 38] Socket operation on non-socket"),
                (libc::ENOTCONN, "[Errno 57] Socket is not connected"),
                (libc::ECONNREFUSED, "[Errno 61] Connection refused"),
            ];
            for (code, text) in macos {
                assert_eq!(errno_text(code), text, "errno {code}");
            }
        } else {
            // Linux: 88 with *macOS' words for 38* would be a coincidence worth
            // naming, and 57 there means "Invalid slot" — a different error
            // entirely, which is why this branch writes 107 and not 57.
            let linux = [
                (libc::ENOTSOCK, "[Errno 88] Socket operation on non-socket"),
                (libc::ENOTCONN, "[Errno 107] Transport endpoint is not connected"),
                (libc::ECONNREFUSED, "[Errno 111] Connection refused"),
            ];
            for (code, text) in linux {
                assert_eq!(errno_text(code), text, "errno {code}");
            }
            assert_eq!(errno_text(57), "[Errno 57] Invalid slot");
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
        // uses: `sun_path` never starts at byte 0 (macOS puts a `sun_len` byte there,
        // Linux puts the two-byte `sa_family_t` instead — the two kernels do not even
        // agree on *why* it is not at 0), and the length passed to `connect` is the
        // name, not the struct. Neither number is a literal in this file precisely
        // because both differ between the platforms this module now claims to run on.
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

/// The Windows half, and the honest heading matters more here than anywhere else in
/// this file: **these cases have never run.** There is no Windows machine in this
/// repository's test path — the crate is built and tested on macOS, and CI does not
/// invoke cargo at all — so `cargo check --target x86_64-pc-windows-msvc
/// --all-targets` is the whole of the local evidence, and it is evidence that this
/// code *typechecks*, not that mpv's pipe answers the way [`Link`] says it does.
///
/// Two consequences shaped every assertion below.
///
/// 1. **Sentences this file writes itself are asserted in full** (`NO_SOCKET`,
///    `closed_text`, the `[WinError n] ` prefix, `wide`'s rejection text), because a
///    typechecking build can be asked about them. **Sentences the operating system
///    writes are asserted as shape only** — prefix present, remainder non-empty —
///    because the words are `FormatMessage`'s and this machine has never heard them.
/// 2. **The pipe tests below are the part that would need a real Windows run.** They
///    stand up a `CreateNamedPipeW` server where mpv's IPC server would be and move
///    bytes through [`Link`], so on the first machine that executes them they answer
///    the question this module doc can only pose: does a synchronous handle read and
///    write mpv's byte pipe the way `PipeConnection` does with an overlapped one.
///    Until then they are compiled, not believed.
#[cfg(all(test, windows))]
mod windows_tests {
    use super::*;
    use windows_sys::Win32::Foundation::ERROR_PIPE_CONNECTED;
    use windows_sys::Win32::Storage::FileSystem::{
        FILE_FLAG_FIRST_PIPE_INSTANCE, PIPE_ACCESS_DUPLEX,
    };
    use windows_sys::Win32::System::Pipes::{
        ConnectNamedPipe, CreateNamedPipeW, DisconnectNamedPipe, PIPE_READMODE_BYTE,
        PIPE_REJECT_REMOTE_CLIENTS, PIPE_TYPE_BYTE, PIPE_WAIT,
    };

    /// The name mpv would have used, shaped like its own: a pipe on this machine,
    /// and a per-process marker so two runs of this test file cannot collide.
    fn pipe_name(marker: &str) -> String {
        format!(r"\\.\pipe\macast-link-{}-{}", std::process::id(), marker)
    }

    /// A named-pipe server, standing where mpv's IPC server stands.
    ///
    /// Byte type and byte read mode because that is what mpv's pipe is: `start_ipc`
    /// reassembles by looking for the trailing `\n`, which is a sentence about a byte
    /// stream — a message-mode pipe would hand back whole messages and the reader
    /// would never see a partial line. `PIPE_REJECT_REMOTE_CLIENTS` because nothing in
    /// this crate should be reachable from another machine, and `PIPE_WAIT` because
    /// this fixture has no buffer to poll: it wants to block until a client arrives.
    struct Server {
        name: String,
        handle: HANDLE,
    }

    impl Server {
        fn open(marker: &str) -> Self {
            let name = pipe_name(marker);
            // `wide` is the same helper [`Link::connect`] uses, so a name this
            // fixture builds and a name the client reads back cannot drift apart.
            let wide = wide(&name).expect("a pipe name has no embedded NUL");
            // SAFETY: `wide` is a NUL-terminated UTF-16 buffer that outlives the call,
            // and a null `lpSecurityAttributes` is what an unshared, unnamed-security
            // pipe takes. The two buffer sizes are ours, not mpv's, and only set how
            // much can be in flight before the slower side blocks.
            let handle = unsafe {
                CreateNamedPipeW(
                    wide.as_ptr(),
                    PIPE_ACCESS_DUPLEX | FILE_FLAG_FIRST_PIPE_INSTANCE,
                    PIPE_TYPE_BYTE
                        | PIPE_READMODE_BYTE
                        | PIPE_WAIT
                        | PIPE_REJECT_REMOTE_CLIENTS,
                    1,
                    4096,
                    4096,
                    0,
                    core::ptr::null(),
                )
            };
            assert_ne!(
                handle,
                INVALID_HANDLE_VALUE,
                "CreateNamedPipeW({name}) failed: {}",
                // SAFETY: `GetLastError` takes no arguments and reads this thread's
                // own last failure, which the call above just set.
                errno_text(unsafe { GetLastError() } as i32)
            );
            Self { name, handle }
        }

        /// Wait for the client to open the pipe.
        ///
        /// A client that arrived before this call is not a failure — `ConnectNamedPipe`
        /// returns `FALSE` with `ERROR_PIPE_CONNECTED` for exactly that case, which is
        /// why the two answers are treated as one rather than as a retry.
        fn accept(&self) {
            // SAFETY: a null `lpOverlapped` is required and correct because this
            // handle was created without `FILE_FLAG_OVERLAPPED` — the same choice
            // [`Link::connect`] makes on its side of the pipe.
            let ok = unsafe { ConnectNamedPipe(self.handle, core::ptr::null_mut()) };
            if ok == 0 {
                // SAFETY: as above.
                let code = unsafe { GetLastError() };
                assert_eq!(
                    code,
                    ERROR_PIPE_CONNECTED,
                    "ConnectNamedPipe({}) failed: {}",
                    self.name,
                    errno_text(code as i32)
                );
            }
        }

        /// The [`Link::send_all`] loop, run by the other end.
        fn write(&self, bytes: &[u8]) {
            let mut sent = 0usize;
            while sent < bytes.len() {
                let mut written = 0u32;
                // SAFETY: as in [`Link::send_all`] — live buffer, live count, no
                // overlapped structure on a synchronous handle.
                let ok = unsafe {
                    WriteFile(
                        self.handle,
                        bytes[sent..].as_ptr(),
                        (bytes.len() - sent) as u32,
                        &mut written,
                        core::ptr::null_mut(),
                    )
                };
                assert_ne!(
                    ok,
                    0,
                    "server write failed: {}",
                    // SAFETY: as above.
                    errno_text(unsafe { GetLastError() } as i32)
                );
                assert_ne!(written, 0, "the pipe accepted nothing and did not fail");
                sent += written as usize;
            }
        }

        /// One `ReadFile`, which is the whole of the server's read side.
        fn read(&self, buf: &mut [u8]) -> usize {
            let mut read = 0u32;
            // SAFETY: as in [`Link::recv_into`].
            let ok = unsafe {
                ReadFile(
                    self.handle,
                    buf.as_mut_ptr(),
                    buf.len() as u32,
                    &mut read,
                    core::ptr::null_mut(),
                )
            };
            if ok == 0 {
                // SAFETY: as above.
                let code = unsafe { GetLastError() };
                assert_eq!(
                    code,
                    ERROR_BROKEN_PIPE,
                    "server read failed: {}",
                    errno_text(code as i32)
                );
                return 0;
            }
            read as usize
        }
    }

    impl Drop for Server {
        fn drop(&mut self) {
            // SAFETY: the handle is this fixture's own, created above and closed
            // exactly once here; disconnecting first is what makes the client's next
            // read see `ERROR_BROKEN_PIPE` rather than a connection that is still
            // nominally open with nobody behind it.
            unsafe {
                DisconnectNamedPipe(self.handle);
                CloseHandle(self.handle);
            }
        }
    }

    /// The reassembly `start_ipc` does, because a byte pipe makes no promise that one
    /// read hands back one line. Asserting against a whole line without this would be
    /// testing the fixture's luck.
    fn read_line(link: &Link) -> Vec<u8> {
        let mut line = Vec::new();
        let mut buf = [0u8; 64];
        loop {
            let read = link.recv_into(&mut buf).expect("a live pipe answers");
            if read == 0 {
                break;
            }
            line.extend_from_slice(&buf[..read]);
            if line.ends_with(b"\n") {
                break;
            }
        }
        line
    }

    #[test]
    fn a_handle_that_was_never_made_says_no_socket() {
        let link = Link::created().expect("allocating a slot does not fail");
        // The Windows `None`-valued `ipc_sock` is a *handle-less* link rather than a
        // missing object, and the reference's `ipc_sock = None` path asks
        // `send_bytes` of it — which is why this constant names that method and not
        // `sendall`.
        assert_eq!(NO_SOCKET, "'NoneType' object has no attribute 'send_bytes'");
        assert_eq!(link.send_all(b"get_property time-pos\n"), Err(NO_SOCKET.to_string()));
        let mut buf = [0u8; 16];
        assert_eq!(link.recv_into(&mut buf), Err(NO_SOCKET.to_string()));
        assert!(!link.is_connected(), "there is no handle to ask about");
        assert!(!link.is_closed(), "having no handle is not the same as being done");
    }

    #[test]
    fn a_closed_handle_says_the_reference_sentence_and_it_has_no_win_error() {
        let link = Link::created().expect("created");
        link.close();
        assert!(link.is_closed());
        // `_check_closed` raises `OSError("handle is closed")` — built from a message,
        // so it carries no errno and no `winerror`, and there is no prefix to print.
        // This is the one sentence in this module that must **not** look like the
        // others: a `[WinError 32] handle is closed` would be quoting the overlapped
        // path this port never takes.
        assert_eq!(closed_text(), "handle is closed");
        assert!(
            !closed_text().starts_with("[WinError"),
            "the closed sentence carries no error number: {}",
            closed_text()
        );
        assert_eq!(link.send_all(b"x"), Err(closed_text()));
        let mut buf = [0u8; 16];
        assert_eq!(link.recv_into(&mut buf), Err(closed_text()));
    }

    #[test]
    fn the_closed_verdict_is_asked_before_the_missing_handle() {
        // Both conditions are true of this link, and the order decides which sentence
        // the log line gets. `stop()` closes the session *and* leaves the slot for the
        // reader, so a reader that wakes after `stop()` must say "handle is closed" —
        // the reason `stop()` ran — rather than the "never made" sentence that would
        // send someone to look for a missing socket.
        let link = Link::created().expect("created");
        link.close();
        assert_eq!(link.send_all(b"x"), Err(closed_text()));
        assert_eq!(link.send_all(b"x"), Err(closed_text()), "no handle was ever made");
    }

    #[test]
    fn every_owner_hears_the_same_close() {
        let link = Link::created().expect("created");
        let reader = link.clone();
        link.close();
        // One session, two owners: `close()` marks the shared flag rather than
        // reaching a handle, because the other owner may still be parked in `ReadFile`.
        assert!(reader.is_closed());
        assert_eq!(reader.send_all(b"x"), Err(closed_text()));
    }

    #[test]
    fn an_empty_buffer_reads_nothing_without_touching_the_pipe() {
        let link = Link::created().expect("created");
        // The reference's reader never asks for zero bytes, and answering it with
        // `NO_SOCKET` would be a lie about a read that did not need a socket at all.
        let mut buf: [u8; 0] = [];
        assert_eq!(link.recv_into(&mut buf), Ok(0));
    }

    #[test]
    fn errno_text_names_the_number_and_leaves_the_words_to_the_system() {
        // Four numbers this module can be asked about: the two that decide read
        // behaviour (`ERROR_BROKEN_PIPE` becomes `Ok(0)`, `ERROR_MORE_DATA` is the
        // defensive arm), the one that answers a zero-byte write
        // (`ERROR_NO_DATA`), and `2` — which is what an absent pipe server gives
        // `CreateFileW`, spelled as a number because no name in this file claims it.
        // The words after each prefix are `FormatMessage`'s, and this machine has
        // never heard them, so the assertions below stop at the shape.
        for code in [
            ERROR_BROKEN_PIPE as i32,
            ERROR_NO_DATA as i32,
            ERROR_MORE_DATA as i32,
            2,
        ] {
            let text = errno_text(code);
            assert!(
                text.starts_with(&format!("[WinError {code}] ")),
                "expected the prefix for {code}, got {text:?}"
            );
            assert!(
                text.len() > format!("[WinError {code}] ").len(),
                "code {code} was given no words at all: {text:?}"
            );
        }
    }

    #[test]
    fn wide_names_end_at_a_nul_and_reject_one() {
        let name = r"\\.\pipe\macast-link-x";
        // The local is not called `wide`: that name is the function under test, and a
        // binding over it turns the last assertion in this test into a type error.
        let units = wide(name).expect("no NUL here");
        assert_eq!(units.last(), Some(&0), "the buffer is NUL-terminated");
        assert_eq!(
            units.len(),
            name.encode_utf16().count() + 1,
            "one UTF-16 unit per character, then the terminator"
        );
        // The pipe name is ASCII in every shape this crate produces, so a one-to-one
        // character count is not a lucky case here — it is the claim being checked.
        assert_eq!(
            units[..units.len() - 1]
                .iter()
                .map(|u| *u as u8 as char)
                .collect::<String>(),
            name
        );
        assert_eq!(
            wide("a\0b").err(),
            Some("ValueError: embedded null byte".to_string()),
            "the rejection is CPython's `str()` for the same call"
        );
    }

    #[test]
    fn a_pipe_nobody_is_serving_is_an_error_with_a_win_error() {
        // Only the shape: whether an absent server answers `ERROR_FILE_NOT_FOUND` (2)
        // or `ERROR_PIPE_BUSY` (231) depends on the server's instance bookkeeping, and
        // guessing a number here would put a number in the test that no machine has
        // offered. What must hold is that a failed open is an `Err`, that it is
        // worded by the system, and that the slot stays empty so a later command says
        // `NO_SOCKET` rather than something about a handle.
        let absent = pipe_name("absent");
        let error = Link::to(&absent).err().expect("nobody is serving that name");
        assert!(
            error.starts_with("[WinError "),
            "a failed open must answer with the system's words, got {error:?}"
        );
    }

    #[test]
    fn a_connected_pipe_moves_bytes_in_both_directions_and_ends() {
        let server = Server::open("roundtrip");
        // [`Link::connect`] blocks until this thread is listening, and
        // [`Server::accept`] blocks until the client arrives, so the two cannot run in
        // some order on one thread. The client goes on a second one; if `connect`
        // fails, `accept` is still waiting and the panicking thread reports it, which
        // is the honest failure shape for a fixture nobody has run yet.
        let name = server.name.clone();
        let client = std::thread::spawn(move || Link::to(&name));
        server.accept();
        let link = client
            .join()
            .expect("the client thread did not panic")
            .expect("opening a pipe this fixture is serving");

        assert!(
            link.is_connected(),
            "`PeekNamedPipe` should answer for a handle that was opened"
        );

        // Server to client: one whole mpv-shaped line, read back through the same
        // reassembly the reader uses.
        let sent = b"{\"error\":\"success\",\"data\":2.5,\"request_id\":7}\n";
        server.write(sent);
        assert_eq!(
            read_line(&link),
            sent.to_vec(),
            "a line mpv writes at a time must arrive as that line"
        );

        // Client to server, and bytes in more than one call must not be glued together:
        // this is the `sendall` loop Windows does not give us for free.
        link.send_all(b"{\"command\":[\"")
            .expect("first half of a command");
        link.send_all(b"set_property\",\"time-pos\",0]}\n")
            .expect("second half of a command");
        let mut received = Vec::new();
        let mut buf = [0u8; 32];
        while !received.ends_with(b"\n") {
            let read = server.read(&mut buf);
            if read == 0 {
                break;
            }
            received.extend_from_slice(&buf[..read]);
        }
        assert_eq!(
            received,
            b"{\"command\":[\"set_property\",\"time-pos\",0]}\n".to_vec(),
            "two writes are one stream"
        );

        // And the end of the session: mpv's side of the pipe going away is
        // `ERROR_BROKEN_PIPE`, which this module translates into the `b''` that breaks
        // `start_ipc`'s loop — the same `Ok(0)` that a Unix socket returns when mpv
        // closes, which is why `stop()` ends the reader the same way on both
        // platforms.
        drop(server);
        let mut buf = [0u8; 16];
        assert_eq!(
            link.recv_into(&mut buf),
            Ok(0),
            "the closed far end is EOF, not an error the reader logs as a reconnect"
        );
    }
}
