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
//! - [`link`] — the connected socket, and the words the reference uses when it
//!   is not there. `#[cfg(unix)]`: Windows named pipes are not implemented, and
//!   the reason is in that module.
//! - [`player`] — the transport as the reference does it: spawn mpv, retry with
//!   a three-attempt budget, connect the IPC socket, read lines under one
//!   [`Feed`], write commands under one lock, and stop in an order that has
//!   reasons. `#[cfg(unix)]` with it. This is the only module here that talks to
//!   anything outside the process, and it does so through a [`player::Host`]
//!   trait — the event bus and the settings live in `src-tauri`, which has not
//!   been written yet, so the crate defines the seam and its tests are the only
//!   implementations of it for now.
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
//! hears 「[Errno 57] Socket is not connected」 rather than an `AttributeError`.
//! Both are recorded in that module, with what the reference therefore never
//! does.

#[cfg(unix)]
pub mod link;
#[cfg(unix)]
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
#[cfg(unix)]
pub use link::Link;
#[cfg(unix)]
pub use player::{Host, Player, StateCall, Timings};
pub use state::{PlayerState, Signal};
pub use ipc::{Close, Failure, Feed, Received};
