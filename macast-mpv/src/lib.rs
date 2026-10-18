//! The player half of Macast: mpv's command line, its JSON IPC, and the one
//! process and one socket those need.
//!
//! mpv is driven over its JSON IPC, which is a line protocol on a Unix socket
//! (or a named pipe on Windows). Most of it is *pure* — the command we intend to
//! send and the meaning of the line that came back — and only the transport is
//! not. This crate holds both halves:
//!
//! - [`command`] — every `loadfile` / `set_property` / `seek` shape the renderer
//!   ever sends, the per-file option dictionary that makes a live stream play
//!   like one, and the socket path that gets passed to `--input-ipc-server`.
//! - [`argv`] — the mpv command line, rebuilt for every start attempt.
//! - [`ipc`] — byte-buffer framing: which chunk is worth parsing, and what the
//!   log says when a line cannot be decoded.
//! - [`state`] — an mpv reply translated into the state updates a DLNA/Cast
//!   control point is told about.
//! - [`pyjson`] — the decoder those replies are read with, replayed from
//!   CPython's own JSON scanner: which document is valid at all, and the exact
//!   sentence the log says when it is not.
//! - [`pyops`] — the places this port touches Python semantics rather than
//!   Rust's, kept in one module so the divergences are countable instead of
//!   scattered. (How many there are is that module's own business; restating the
//!   number here is how it goes stale.)
//! - [`link`] — the connected transport, and the words the reference uses when it
//!   is not there. Both halves are written: `AF_UNIX` on Unix, a named pipe on
//!   Windows. They are not equally *known*: the Unix sentences were measured on
//!   this Mac and read off a Linux run, while the Windows column is a port that no
//!   machine here has exercised. That boundary, and the four places this port
//!   diverges from the reference because of it, is stated in that module.
//! - [`player`] — the transport as the reference does it: spawn mpv, retry with
//!   a three-attempt budget, connect the IPC socket, read lines under one
//!   [`Feed`], write commands under one lock, and stop in an order that has
//!   reasons. The two things in it that are not transport are forked by platform
//!   rather than gated away: sending a signal (`SIGTERM`/`SIGKILL` on Unix,
//!   `TerminateProcess` on Windows — see the module's own note on why both names
//!   survive there), and the tests that fake a player, which need a shell script
//!   and a real socket and therefore live in a `#[cfg(unix)]` module, with the
//!   Windows cases that need no child written beside it. This is the only module
//!   here that talks to anything outside the process, and it does so through a
//!   [`player::Host`] trait — the event bus and the settings live in `src-tauri`,
//!   whose `impl Host for TauriHost` (`src-tauri/src/host.rs`) is the trait's only
//!   non-test implementation. The crate still *defines* the seam instead of
//!   depending on the shell: that is what keeps every test in this crate hermetic,
//!   and the three `impl Host` blocks inside `player.rs`'s test modules are what
//!   a player looks like when there is no window to notify.
//!
//! Why the split is worth a crate of its own: AGENTS.md records a family of
//! defects that all have the same shape — *a contract that only a real player or
//! a real mpv reply can show, discovered on a user's machine*. `loadfile` with
//! the position written into the fourth slot instead of the fifth returned
//! `invalid parameter` and the picture stayed frozen; a window titled
//! `media - mpv` was a filename fallback, not a bug in the title code; 92% of the
//! duration lines in a 3-hour log were restating the previous value. Each of
//! those is now a unit test with nothing on the other end of the pipe, because
//! the shapes are computed here.
//!
//! The ordering in [`player`] is the same kind of claim, and it was earned the
//! same way. Two examples that are *not* style: the command line is built
//! **outside** the retry loop's `try`, so a knob Python cannot compare escapes
//! the thread as a traceback instead of spending a retry and printing
//! 「mpv cannot start」; and the socket is assigned **before** `connect()`, so a
//! failed connect leaves an unconnected socket in the slot and the next command
//! hears the kernel's own 「not connected」 rather than an `AttributeError`. Which
//! kernel's words those are is not a detail: macOS says `[Errno 57] Socket is not
//! connected`, Linux says `[Errno 107] Transport endpoint is not connected`, and on
//! Linux 57 is 「Invalid slot」 — so the number is not the sentence's name, and the
//! test that quotes it is gated to the machine that measured it.
//! On Windows that state cannot be left behind at all: `CreateFileW` hands back a
//! handle or hands back nothing, so a failed connect leaves the slot empty and the
//! next command logs [`link::NO_SOCKET`]. That asymmetry is the first divergence
//! written out in [`link`].
//! Both examples are recorded in the modules they belong to, with what the
//! reference therefore never does.

pub mod link;
pub mod player;
pub mod argv;
pub mod command;
pub mod ipc;
pub mod pyjson;
pub mod pyops;
pub mod state;

// Re-exports below are the headline types of each module; the full surface of a
// module stays reachable through it (and `command` deliberately exports its
// individual shapes rather than a builder object — there is nothing to build up
// between one command and the next).
pub use argv::{build, Build, BuildError, Config};
pub use command::{live_load_options, loadfile, Liveness};
// `Link` is exported on every platform this crate builds on: the type is the same
// three-state question on all of them, only the container behind it differs.
pub use link::Link;
pub use player::{Host, Player, StateCall, Timings};
pub use state::{PlayerState, Signal};
pub use ipc::{Close, Failure, Feed, Received};
