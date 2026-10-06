//! The player half of Macast: everything this crate does, it does without
//! touching a socket or a process.
//!
//! mpv is driven over its JSON IPC, which is a line protocol on a Unix socket
//! (or a named pipe on Windows). Two of its three moving parts are therefore
//! *pure* — the command we intend to send and the meaning of the line that came
//! back — and only the transport is not. This crate holds the pure halves:
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
//! The transport half (spawning mpv, connecting, the retry loop with its
//! three-attempt budget) is deliberately *not* here: it belongs with the process
//! supervision of `src-tauri`, where it can be written once against real
//! platform APIs instead of twice against a mock.

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
pub use ipc::{Close, Failure, Feed, Received};
pub use state::{PlayerState, Signal};
