//! Shared, non-UI facts about this machine and this installation.
//!
//! This crate is the Rust counterpart of `macast/utils.py` plus the parts of
//! `macast/server.py` and `Macast.py` that are *not* about serving HTTP:
//! where files live, how the settings JSON is read and protected, which network
//! addresses are worth publishing, what environment a child process gets, and
//! how logs rotate.
//!
//! Every module here is deliberately free of sockets and processes, so the
//! rules that took real machines to learn (AGENTS.md §4.1 / §4.2 / §4.3) can be
//! asked with a unit test instead of a reproduction.

pub mod env;
pub mod logging;
pub mod module_settings;
pub mod net;
pub mod paths;
pub mod plugin_repo;
pub mod pyops;
pub mod setting;
pub mod timefmt;
pub mod token;
pub mod trash;
pub mod url;

pub use env::{player_env, PlayerEnv, PROXY_ENV_VARS};
pub use logging::Logger;
pub use net::{unadvertisable_reason, InterfaceRow, Snapshot, Steering};
pub use setting::{Key, Setting};
pub use trash::TrashError;
pub use url::{
    PySplit, PyUrlError, host_is_loopback, py_repr_str, py_urljoin, py_urlparse, py_urlsplit,
    py_urlunparse, py_urlunsplit,
};

/// Everything the receiver needs that is not a protocol.
///
/// Python keeps these in class attributes on `Setting`; here they are one
/// struct with a lock, because the Rust port has several threads reading the
/// same table (SSDP recv loop, event thread, mpv IPC thread, HTTP workers).
pub struct Core {
    pub setting: Setting,
}

impl Core {
    /// Build the core layer, loading the settings file from
    /// [`paths::setting_dir`].
    ///
    /// A file we cannot parse is moved aside intact before anything can write
    /// over it; that path returns the settings with defaults and logs where the
    /// original went (see [`setting::Setting::load`]).
    pub fn new() -> Self {
        Core {
            setting: Setting::load_default(),
        }
    }
}

impl Default for Core {
    fn default() -> Self {
        Self::new()
    }
}
