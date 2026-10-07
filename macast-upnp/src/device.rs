//! The six USNs a MediaRenderer announces, and the restart an IP change needs.
//!
//! Python counterpart: `macast/plugin.py::SSDPPlugin` (and, for the two numbers
//! below, `macast/ssdp.py::SSDPServer.register`).
//!
//! ## What this module owns
//!
//! [`SsdpServer`] owns the socket, the registry, and the bytes of a `NOTIFY`.
//! This module owns the *content of the announcement*: which six identities this
//! machine claims, what their `ST` is, where their description document lives,
//! and how long a control point may cache the answer. That split is Python's —
//! `SSDPPlugin` never touches a socket, it only calls `register`/`do_notify`/
//! `stop` — and it is the reason the announcement face is testable without
//! binding UDP/1900.
//!
//! ## The five details that are not decoration
//!
//! * **Six entries, in this order.** `upnp:rootdevice` first, then the bare
//!   `uuid:<id>`, then the device, then the three services. [`SsdpServer::do_notify`]
//!   walks the list in order, so this *is* the order the LAN sees, and a
//!   control point that keys off "the first answer I got" keys off it too.
//! * **`ST` is everything after the first `::`** — `st_of()`. Python used to
//!   carve this with `device[43:]`, which was correct only for a 36-character
//!   UUID; the comment at `plugin.py:148-150` says so. The bare
//!   `uuid:<id>` entry has **no** separator, and its `ST` is the whole string.
//! * **`LOCATION` keeps a literal `{}`.** `'http://{{}}:{port}/description.xml'`
//!   is Python's escaped-brace format string, and the brace survives into the
//!   registry on purpose: `crate::sock::Sock::send_it`'s `expand_ip` is the only
//!   place an interface address is substituted, because on a multi-homed machine
//!   each sender must stamp **its own** address. Filling the brace in here would
//!   hand every sender the same IP — the failure AGENTS.md §4.1 calls "searchable,
//!   then casting does nothing".
//! * **`max-age=66`, not the server's default.** [`SsdpServer::register_device`]
//!   supplies `max-age=1800`; a MediaRenderer passes 66 so a control point
//!   re-searches within about a minute. Two numbers, two callers, deliberately
//!   unequal — that is why the value is an argument here rather than a constant
//!   both sides read.
//! * **`stop(byebye=False)` inside `update_ip()`.** The restart driver
//!   (`macast/server.py::Service.notify`, ported as `crate::Service`'s question)
//!   calls this on every IP change. If the restart announced goodbye, the
//!   renderer would go offline in every control point's cache for the length of
//!   a re-register, which is exactly the "casting drops after a while" report
//!   AGENTS.md §4.2 records. Python also serialises the restart under
//!   `self.restart_lock`, so two IP-change publishes cannot interleave through
//!   one another's `stop`/`start`.
//!
//! ## What this module does *not* subscribe to
//!
//! Python's `start()` ends with two `bus.subscribe` calls (`ssdp_notify` →
//! `notify`, `ssdp_update_ip` → `update_ip`) and `stop()` reverses them. This
//! crate owns no bus and no timer: `src-tauri` publishes those two messages and
//! calls these two methods, the same way `macast/server.py` decides *when* to ask
//! and `SSDPPlugin` decides *what happens*. A timer inside the crate would be a
//! second owner of "is it healthy", and §4.8's rule is that a number with two
//! owners eventually gives two answers.
//!
//! ## Error shape
//!
//! [`SsdpPlugin::notify`] and [`SsdpPlugin::unregister`] return `Result<(), String>`
//! and stop at the **first** failure. That is faithful, not lazy: Python has no
//! `try` inside either loop, so the first `KeyError` escapes and the remaining
//! devices genuinely go unannounced (or stay registered). Silently logging and
//! continuing would make "the announcement happened" mean something else.

use std::sync::{Arc, Mutex, MutexGuard};

use macast_core::logging::Logger;
use macast_core::paths;
use macast_core::setting::{application_port, Setting};

use crate::ssdp::SsdpServer;
use crate::store::SharedHooks;

/// The root-device search target: `uuid:<id>::upnp:rootdevice`.
pub const ROOTDEVICE: &str = "upnp:rootdevice";

/// The device URN a DLNA MediaRenderer announces (`d:`).
pub const UPNP_DEVICE: &str = "urn:schemas-upnp-org:device:MediaRenderer:1";

/// The three service URNs (`urn:schemas-upnp-org:service:…`), in the order
/// Python appends them: RenderingControl, ConnectionManager, AVTransport.
pub const RENDERING_CONTROL_URN: &str = "urn:schemas-upnp-org:service:RenderingControl:1";
pub const CONNECTION_MANAGER_URN: &str = "urn:schemas-upnp-org:service:ConnectionManager:1";
pub const AVTRANSPORT_URN: &str = "urn:schemas-upnp-org:service:AVTransport:1";

/// `CACHE-CONTROL` for a renderer announcement.
///
/// Not [`crate::ssdp::DEFAULT_CACHE_CONTROL`]: that 1800 is the default for a
/// device registered through `register_device()`, and nothing in Macast uses it.
/// A control point caching these USNs for half an hour would keep a dead
/// renderer in its list long after the interface went away.
pub const ANNOUNCE_CACHE_CONTROL: &str = "max-age=66";

/// `st = device.split('::', 1)[1] if '::' in device else device`.
///
/// `str::find` returns the *first* occurrence, which is what `split` with
/// `maxsplit=1` takes the tail of, so the two agree on a USN that somehow
/// contains more than one separator as well as on the six we build.
pub fn st_of(usn: &str) -> &str {
    match usn.find("::") {
        Some(at) => &usn[at + 2..],
        None => usn,
    }
}

/// `'http://{{}}:{}/description.xml'.format(Setting.get_port())`.
///
/// The `{{}}` in Python is one literal pair of braces; the result still contains
/// `{}` when it reaches the registry. See the module header.
pub fn location(port: u16) -> String {
    format!("http://{{}}:{}/description.xml", port)
}

/// `SSDPPlugin(plugins.SimplePlugin)` — the announcement face of the renderer.
pub struct SsdpPlugin {
    /// `self.ssdp` — the socket-and-registry half, handed the same `Hooks` and
    /// `Setting` so a failure inside it reaches the application, not a copy.
    ssdp: Arc<SsdpServer>,
    /// `Setting` — Python reads it globally; here it is a field so a test can
    /// build a plugin over a settings file that does not exist (AGENTS.md §10).
    setting: Arc<Setting>,
    /// `self.devices` — the six USNs, rebuilt by `build_device_info()`.
    devices: Mutex<Vec<String>>,
    /// `self.restart_lock` — held across `stop` → rebuild → `register` → `start`.
    restart: Mutex<()>,
    /// `logger = logging.getLogger("PLUGIN")` — this module's Python lives in
    /// `plugin.py`, so its log name is the module's, not the class's. The three
    /// sentences below are what a reader greps for when SSDP "is not working".
    log: Arc<Logger>,
}

impl SsdpPlugin {
    /// `SSDPPlugin.__init__`.
    ///
    /// Python logs first, then builds the server, then calls
    /// `build_device_info()` — so an instance is registerable the moment it
    /// exists, and `start()` does not have to be preceded by anything.
    pub fn new(hooks: SharedHooks, setting: Arc<Setting>) -> Arc<SsdpPlugin> {
        let plugin = Arc::new(SsdpPlugin {
            ssdp: SsdpServer::new(hooks, Arc::clone(&setting)),
            setting,
            devices: Mutex::new(Vec::new()),
            restart: Mutex::new(()),
            log: Logger::named("PLUGIN"),
        });
        plugin.log.info("Initializing SSDPPlugin");
        plugin.build_device_info();
        plugin
    }

    /// The registry half, for the caller that wants to answer M-SEARCH
    /// diagnostics or ask [`SsdpServer::is_healthy`] directly.
    pub fn ssdp(&self) -> &Arc<SsdpServer> {
        &self.ssdp
    }

    /// `self.devices` as a copy, so no lock is held while six `register` calls
    /// (each of which takes the registry's own lock) run. Python's `for device
    /// in self.devices:` holds no lock at all and can therefore iterate a list
    /// another thread is replacing; a snapshot is the stricter side of that.
    pub fn devices(&self) -> Vec<String> {
        match self.devices.lock() {
            Ok(g) => g.clone(),
            Err(e) => e.into_inner().clone(),
        }
    }

    /// `build_device_info()` — reread the USN and rebuild the six.
    ///
    /// Python calls `Setting.get_usn()` once per line. That is six reads of an
    /// insert-on-read key (§4.2: reading a missing key writes the default into the
    /// table), so the first one generates the UUID and the other five return it.
    /// One read here is the same value with five fewer locks — and the value must
    /// be *one* read, because a `refresh_usn()` landing between two of Python's
    /// six would announce two different devices under one root.
    pub fn build_device_info(&self) {
        let usn = self.setting.usn();
        let list = vec![
            format!("uuid:{}::{}", usn, ROOTDEVICE),
            format!("uuid:{}", usn),
            format!("uuid:{}::{}", usn, UPNP_DEVICE),
            format!("uuid:{}::{}", usn, RENDERING_CONTROL_URN),
            format!("uuid:{}::{}", usn, CONNECTION_MANAGER_URN),
            format!("uuid:{}::{}", usn, AVTRANSPORT_URN),
        ];
        let mut devices = match self.devices.lock() {
            Ok(g) => g,
            Err(e) => e.into_inner(),
        };
        *devices = list;
    }

    /// `register()` — file all six, each with the `ST` derived from its own USN.
    ///
    /// The port comes from [`application_port`], which is Python's
    /// `Setting.get_port()` (`Setting.get(ApplicationPort, DEFAULT_PORT)`), and the
    /// `SERVER` from [`paths::server_info`], Python's `Setting.get_server_info()`.
    /// Both read the settings on **every** call rather than caching: a fallback
    /// port (58880 taken → `Macast(0123)` on a random one, §4.2) must not leave a
    /// `LOCATION` that points at a port nothing listens on.
    pub fn register(&self) {
        let port = application_port(&self.setting);
        let server = paths::server_info();
        let address = location(port);
        for device in self.devices() {
            let st = st_of(&device);
            self.ssdp
                .register(&device, st, &address, &server, ANNOUNCE_CACHE_CONTROL);
        }
    }

    /// `notify()` — `ssdp:alive` for each of the six.
    ///
    /// Each announcement goes out **twice** (that is `SsdpServer::do_notify`'s
    /// business, not this loop's): a freshly-woken receiver has not joined the
    /// multicast group when the first datagram arrives, and one M-SEARCH per
    /// search was the mistake this compensates for.
    pub fn notify(&self) -> Result<(), String> {
        for device in self.devices() {
            self.ssdp.do_notify(&device)?;
        }
        Ok(())
    }

    /// `unregister()` — delete all six from the registry.
    ///
    /// Reaching the caller's `Err` means the loop stopped, so some of the six may
    /// still be registered and may still answer an M-SEARCH. Python has exactly
    /// this behaviour; the difference is that here the caller is *told*.
    pub fn unregister(&self) -> Result<(), String> {
        for device in self.devices() {
            self.ssdp.unregister(&device)?;
        }
        Ok(())
    }

    /// `update_ip()` — the restart, under the lock.
    ///
    /// Order matters and is Python's: stop **without** goodbye, reread the USN
    /// (it can have been refreshed while the service was down), re-register with
    /// the current port, then start. The `stop(byebye=False)` argument is the one
    /// line here that a reader should not "tidy": see the module header.
    pub fn update_ip(&self) {
        let _restart = self.restart_lock();
        self.ssdp.stop(false);
        self.build_device_info();
        self.register();
        self.ssdp.start();
    }

    /// `start()` — register, then open the socket.
    ///
    /// Registering **before** `start()` is what lets the very first
    /// `ssdp:alive` burst inside `SsdpServer::run` find six entries to announce.
    /// Python's two `bus.subscribe` lines are `src-tauri`'s wiring; see the
    /// module header.
    pub fn start(&self) {
        self.log.info("starting SSDPPlugin");
        self.register();
        self.ssdp.start();
    }

    /// `stop()` — the deliberate goodbye.
    ///
    /// The message reads `Stoping SSDPPlugin`, misspelled, because that is what
    /// `plugin.py:185` has always printed and a reader grepping a log for
    /// "Stoping SSDP" must keep finding it. Fixing the spelling in a port is how
    /// a log-searching habit silently dies.
    pub fn stop(&self) {
        self.log.info("Stoping SSDPPlugin");
        let _restart = self.restart_lock();
        self.ssdp.stop(true);
    }

    /// `with self.restart_lock:` — poison read back rather than propagated, as
    /// every other lock in this crate does. A panicked restart left the mutex in
    /// an otherwise usable state, and refusing the next restart would turn one
    /// old failure into permanent silence.
    fn restart_lock(&self) -> MutexGuard<'_, ()> {
        match self.restart.lock() {
            Ok(g) => g,
            Err(e) => e.into_inner(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ssdp::DEFAULT_CACHE_CONTROL;
    use crate::store::Hooks;
    use macast_core::setting::Key;
    use serde_json::Value;

    /// A `Hooks` that answers nothing. The announcement face never calls it in
    /// these tests — it exists so a test can build a plugin without dragging in
    /// `src-tauri`, and so an accidental `stop_service()` from a bind failure
    /// would be visible as silence rather than as a real service being torn down.
    struct NullHooks;

    impl Hooks for NullHooks {
        fn notify(&self, _title: &str, _text: &str) {}
        fn stop_service(&self) {}
    }

    /// A settings file that **does not exist**, named for this process and this
    /// test. `Setting::new` reads nothing until `load()`, the accessors retry the
    /// load and find nothing, and nothing here calls `save()` — so the generated
    /// USN, the port, and the six USNs all live in memory, and AGENTS.md §10
    /// ("不要为了验证去改用户的真实配置") cannot be violated from this module.
    /// `the_announcement_face_never_writes_the_settings_file` checks that promise
    /// instead of assuming it, and says which write would break it.
    fn setting(name: &str) -> Arc<Setting> {
        let path = std::env::temp_dir().join(format!(
            "macast-upnp-device-{}-{}.json",
            std::process::id(),
            name
        ));
        Arc::new(Setting::new(path))
    }

    fn plugin(name: &str) -> (Arc<SsdpPlugin>, Arc<Setting>) {
        let setting = setting(name);
        let hooks: SharedHooks = Arc::new(NullHooks);
        let plugin = SsdpPlugin::new(hooks, Arc::clone(&setting));
        (plugin, setting)
    }

    /// Every header of one registered device, as `(key, value)` pairs.
    fn headers(plugin: &SsdpPlugin, usn: &str) -> Vec<(String, String)> {
        plugin
            .ssdp()
            .entry(usn)
            .unwrap_or_else(|| panic!("{} is not registered", usn))
            .headers
    }

    fn value_of(fields: &[(String, String)], key: &str) -> String {
        for (k, v) in fields {
            if k == key {
                return v.clone();
            }
        }
        panic!("no {} header", key)
    }

    /// Deletes this test's settings file on the way out.
    ///
    /// One test needs it: `Setting::refresh_usn()` goes through `set()`, which
    /// writes the file. Every other write here is a `put()` — memory only — so
    /// the announcement face is never the reason a file exists.
    struct TempFile(Arc<Setting>);

    impl Drop for TempFile {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(self.0.path());
        }
    }

    #[test]
    fn the_six_usns_are_in_the_order_the_lan_sees_them() {
        crate::pin_test_dir();
        let (plugin, setting) = plugin("order");
        let id = setting.usn();
        assert_eq!(
            plugin.devices(),
            vec![
                format!("uuid:{}::{}", id, ROOTDEVICE),
                format!("uuid:{}", id),
                format!("uuid:{}::{}", id, UPNP_DEVICE),
                format!("uuid:{}::{}", id, RENDERING_CONTROL_URN),
                format!("uuid:{}::{}", id, CONNECTION_MANAGER_URN),
                format!("uuid:{}::{}", id, AVTRANSPORT_URN),
            ],
            "the order of this list is the order of the six announcements"
        );
    }

    #[test]
    fn st_is_the_tail_after_the_first_separator_and_the_bare_uuid_has_none() {
        crate::pin_test_dir();
        let (plugin, setting) = plugin("st");
        let id = setting.usn();
        let devices = plugin.devices();

        // The three services and the root device: everything after the first `::`.
        assert_eq!(st_of(&devices[0]), ROOTDEVICE);
        assert_eq!(st_of(&devices[2]), UPNP_DEVICE);
        assert_eq!(st_of(&devices[3]), RENDERING_CONTROL_URN);
        assert_eq!(st_of(&devices[4]), CONNECTION_MANAGER_URN);
        assert_eq!(st_of(&devices[5]), AVTRANSPORT_URN);

        // The bare `uuid:<id>` entry carries no separator, and its ST is the
        // whole string — that is what `if '::' in device else device` says.
        assert_eq!(devices[1], format!("uuid:{}", id));
        assert!(!devices[1].contains("::"));
        assert_eq!(st_of(&devices[1]), devices[1]);

        // `device[43:]` was the old carve, and it is right only for a 36-character
        // UUID. A short id shows the two answers disagreeing, which is the whole
        // reason `st_of` exists (`plugin.py:148-150`) — and in Python the wrong
        // carve there is not even an error, it is an empty ST, because a slice past
        // the end of a string silently returns `''`.
        let short = format!("uuid:abc::{}", ROOTDEVICE);
        assert!(
            short.len() < 43,
            "this is the case the fixed slice could not express: the whole USN is \
             shorter than the offset it started at"
        );
        assert_eq!(st_of(&short), ROOTDEVICE);
        assert_eq!(
            short.get(43..),
            None,
            "Rust's equivalent of `device[43:]` has nothing to give back here"
        );

        // More than one separator: `split('::', 1)` keeps everything after the
        // *first*, and so does `str::find`.
        assert_eq!(st_of("uuid:x::a::b"), "a::b");
        assert_eq!(st_of(""), "");
        assert_eq!(st_of("::"), "");
        assert_eq!(st_of("x:y"), "x:y", "a single colon is not a separator");
    }

    #[test]
    fn registered_headers_are_in_pythons_write_order() {
        crate::pin_test_dir();
        let (plugin, _) = plugin("header-order");
        plugin.register();
        let root = plugin.devices()[0].clone();
        let fields = headers(&plugin, &root);
        let keys: Vec<&str> = fields.iter().map(|(k, _)| k.as_str()).collect();
        assert_eq!(
            keys,
            vec!["USN", "LOCATION", "ST", "EXT", "SERVER", "CACHE-CONTROL"],
            "register() writes these six keys in this order, and the order is the \
             response header order the control points were tested against"
        );
        assert_eq!(value_of(&fields, "USN"), root);
        assert_eq!(value_of(&fields, "ST"), ROOTDEVICE);
        // `EXT` is registered with an empty value; `notify_message` joins with
        // `': '`, so the wire line is `EXT: ` *with* its trailing space. Reading
        // the empty value here is the only way to keep that fact honest.
        assert_eq!(value_of(&fields, "EXT"), "");
    }

    #[test]
    fn location_keeps_a_literal_brace_and_the_current_port() {
        crate::pin_test_dir();
        // The brace survives on purpose: `crate::sock::Sock::send_it`'s `expand_ip`
        // is the only place an interface address is substituted, because each
        // sender must stamp *its own* address.
        assert_eq!(
            location(58880),
            "http://{}:58880/description.xml",
            "Python's '{{}}' is one literal pair of braces"
        );

        let (plugin, setting) = plugin("location");
        plugin.register();
        let first = plugin.devices()[0].clone();
        assert_eq!(
            value_of(&headers(&plugin, &first), "LOCATION"),
            location(application_port(&setting))
        );

        // The port is read on **every** register, not cached at construction: a
        // fallback port (§4.2, 58880 taken → a random one) must not leave a
        // `LOCATION` pointing at a port nothing listens on. `put`, not `set`: the
        // latter writes the settings file, and a test has no business doing that.
        setting.put(Key::ApplicationPort, Value::from(59001i64));
        plugin.register();
        assert_eq!(
            value_of(&headers(&plugin, &first), "LOCATION"),
            "http://{}:59001/description.xml"
        );
    }

    #[test]
    fn the_renderer_caches_for_66_not_the_servers_1800() {
        crate::pin_test_dir();
        assert_eq!(ANNOUNCE_CACHE_CONTROL, "max-age=66");
        assert_ne!(
            ANNOUNCE_CACHE_CONTROL, DEFAULT_CACHE_CONTROL,
            "two numbers, two callers: register_device() supplies 1800 and nothing \
             in Macast uses it"
        );

        let (plugin, _) = plugin("cache-control");
        plugin.register();
        for device in plugin.devices() {
            assert_eq!(
                value_of(&headers(&plugin, &device), "CACHE-CONTROL"),
                ANNOUNCE_CACHE_CONTROL,
                "{} was announced with the wrong cache control",
                device
            );
        }
    }

    #[test]
    fn server_header_is_the_settings_answer() {
        crate::pin_test_dir();
        let (plugin, _) = plugin("server");
        plugin.register();
        let expected = paths::server_info();
        for device in plugin.devices() {
            assert_eq!(value_of(&headers(&plugin, &device), "SERVER"), expected);
        }
        assert!(
            expected.contains("Macast"),
            "a control point reads this as the device's identity"
        );
    }

    #[test]
    fn re_registering_replaces_in_place_and_keeps_the_order() {
        crate::pin_test_dir();
        let (plugin, setting) = plugin("replace");
        plugin.register();
        let before: Vec<String> = plugin
            .ssdp()
            .registry()
            .iter()
            .map(|e| e.usn.clone())
            .collect();

        setting.put(Key::ApplicationPort, Value::from(59002i64));
        plugin.register();

        let after = plugin.ssdp().registry();
        assert_eq!(after.len(), 6, "re-registering must not append a second copy");
        let usns: Vec<String> = after.iter().map(|e| e.usn.clone()).collect();
        assert_eq!(usns, before, "the dict keeps each key's position");
        for entry in &after {
            assert_eq!(value_of(&entry.headers, "LOCATION"), location(59002));
        }
    }

    #[test]
    fn a_refreshed_usn_registers_six_more_and_leaves_the_first_six_in_place() {
        crate::pin_test_dir();
        // `refresh_usn()` writes the settings file (it goes through `set`, not
        // `put`), so this is the one test that needs the file removed on the way
        // out. Everything else in this module keeps its writes in memory.
        let (plugin, setting) = plugin("refresh");
        let _temp = TempFile(Arc::clone(&setting));
        let old = setting.usn();
        plugin.register();

        // `update_ip()`'s middle, without the two socket steps: `SsdpServer::start`
        // binds UDP/1900, so it is never callable from a test.
        let fresh = setting.refresh_usn();
        assert_ne!(fresh, old);
        plugin.build_device_info();
        plugin.register();

        let devices = plugin.devices();
        assert_eq!(devices[1], format!("uuid:{}", fresh));
        for device in &devices {
            assert!(device.contains(&fresh), "{} still names the old device", device);
        }

        // The half that a "the old device goes away" reading would have lost:
        // `update_ip()` stops, rebuilds, registers and starts — it never
        // unregisters (`plugin.py:171-181`), so the registry now holds twelve
        // USNs, the stale six first because a Python dict keeps a key's position
        // and these are *new* keys. Consequence on the wire: `ssdp.py` answers an
        // M-SEARCH by iterating `known.values()`, so a control point that names
        // the old USN still gets a reply. That is Python's behaviour; it is
        // recorded here rather than "improved" away, because the two announcements
        // a refresh leaves in the cache are not what a phone ever searches for.
        let usns: Vec<String> = plugin
            .ssdp()
            .registry()
            .iter()
            .map(|e| e.usn.clone())
            .collect();
        assert_eq!(usns.len(), 12, "update_ip() does not unregister");
        let stale = format!("uuid:{}::{}", old, ROOTDEVICE);
        assert_eq!(usns[0], stale, "the six stale USNs keep their positions");
        assert!(
            plugin.ssdp().is_known(&stale),
            "the device that was replaced is still in the registry as announced"
        );
        assert_eq!(usns[6], format!("uuid:{}::{}", fresh, ROOTDEVICE));

        // Announcements, by contrast, walk `devices()` and so touch only the
        // current six. This is the fact that makes the twelve-entry registry
        // survivable: the LAN is re-told about one device, not two. And the
        // announcement path only ever *reads* the registry — it adds nothing and
        // prunes nothing, which is what the count below is for.
        plugin.notify().unwrap();
        assert_eq!(
            plugin.ssdp().registry().len(),
            12,
            "notify() must leave the registry exactly as register() left it"
        );
    }

    #[test]
    fn unregistering_stops_at_the_first_device_that_is_not_there() {
        crate::pin_test_dir();
        let (plugin, _) = plugin("unregister");
        plugin.register();
        let devices = plugin.devices();

        // Python has no `try` inside `unregister()`, so the first `KeyError`
        // escapes the loop and the devices after it genuinely stay registered.
        // Failing loudly is the faithful port; silently continuing would make
        // "the device was unregistered" mean something else.
        plugin.ssdp().unregister(&devices[1]).unwrap();
        let err = plugin.unregister().unwrap_err();
        assert_eq!(err, format!("KeyError('{}')", devices[1]));

        let left: Vec<String> = plugin
            .ssdp()
            .registry()
            .iter()
            .map(|e| e.usn.clone())
            .collect();
        assert_eq!(
            left,
            vec![devices[2].clone(), devices[3].clone(), devices[4].clone(), devices[5].clone()],
            "the loop stopped at index 1, so indexes 2..5 are still announced"
        );
    }

    #[test]
    fn unregistering_everything_leaves_an_empty_registry() {
        crate::pin_test_dir();
        let (plugin, _) = plugin("unregister-all");
        plugin.register();
        assert_eq!(plugin.ssdp().registry().len(), 6);
        plugin.unregister().unwrap();
        assert!(plugin.ssdp().registry().is_empty());
    }

    #[test]
    fn notify_before_register_is_silent_not_fatal() {
        crate::pin_test_dir();
        // `do_notify` answers `Ok(())` for a USN it never registered — a device
        // that is not announced cannot be re-announced, and that is not an error
        // the caller can act on. The loop therefore must not turn it into one.
        let (plugin, _) = plugin("notify-empty");
        plugin.notify().unwrap();
        assert!(plugin.ssdp().registry().is_empty());

        plugin.register();
        plugin.notify().unwrap();
    }

    #[test]
    fn stop_is_idempotent_on_a_service_that_never_started() {
        crate::pin_test_dir();
        let (plugin, _) = plugin("stop");
        plugin.stop();
        plugin.stop();
        assert!(!plugin.ssdp().is_running());
        assert!(plugin.ssdp().is_healthy());
    }

    #[test]
    fn the_announcement_face_never_writes_the_settings_file() {
        crate::pin_test_dir();
        let (plugin, setting) = plugin("no-disk");
        plugin.build_device_info();
        plugin.register();
        plugin.notify().unwrap();
        plugin.stop();
        assert!(
            !setting.path().exists(),
            "the announcement face only ever reads, so a device test cannot leave \
             a file behind in the user's configuration directory (§10)"
        );

        // The counter-example, asked rather than assumed: the promise above is
        // about *this* face, not about `Setting`. `set()` stores **and** saves
        // (`setting.rs`: `self.put(key, value); self.save();`), which is exactly
        // why every other write in this module is a `put()` and why the one test
        // that calls `refresh_usn()` carries a `TempFile`.
        setting.set(Key::ApplicationPort, Value::from(59003i64));
        assert!(
            setting.path().exists(),
            "if this ever reads as absent, `set()` stopped writing, and then so did \
             `refresh_usn()` — which is what the `TempFile` above is paying for"
        );
        let _ = std::fs::remove_file(setting.path());
    }
}
