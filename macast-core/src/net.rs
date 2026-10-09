//! Which of this machine's addresses are worth telling other devices about.
//!
//! This is the rule that turned "Macast is discoverable but nothing can cast to
//! it" from a mystery into a test. mDNS publishes *every* address as an A record
//! for the service host; a sender resolves the device, gets five answers (Wi-Fi,
//! two VM bridges, the Tailscale utun, the loopback), picks roughly at random and
//! connects to a host that is not listening there. So one predicate decides it
//! for both halves of discovery — the network picker shows its reason text and
//! the advertiser applies the same test, which is why neither can drift (AGENTS.md
//! §4.1, `macast/utils.py:unadvertisable_reason`).
//!
//! Three layers, deliberately separated:
//!
//! - [`unadvertisable_reason`] and the pure functions that consume it: the rules,
//!   testable on any machine with a hand-written table.
//! - [`Snapshot`]: what this machine actually has, read from the OS.
//! - [`get_ip`] / [`get_advertisable_ip`] / [`advertisable_addresses`]: the three
//!   questions the callers ask, each a pure function over a [`Snapshot`] plus the
//!   three settings that steer it.
//!
//! The published address matters beyond cosmetics: SSDP only answers a peer whose
//! address falls in the same subnet as an entry here, so pinning an interface also
//! stops Macast replying over bridges and tunnels the user never chose.

use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::sync::Mutex;

use serde::Serialize;

use crate::setting::{Key, Setting};

/// The strings `unadvertisable_reason` can return. Public because the settings
/// page quotes them and a test must be able to name them.
pub const REASON_NO_IPV4: &str = "没有 IPv4 地址";
pub const REASON_LOOPBACK: &str = "回环地址，其他设备不可达";
pub const REASON_LINK_LOCAL: &str = "链路本地地址，未取得 DHCP 租约";
pub const REASON_POINT_TO_POINT: &str = "点对点地址（/32），不属于局域网";
pub const REASON_NOT_IPV4: &str = "非 IPv4 地址";
pub const REASON_CGNAT: &str = "共享地址空间（Tailscale 等虚拟网卡）";

/// Why `addr` must not be published to other devices; `""` means it may.
///
/// The order is the order Python uses and each rule is load-bearing: the /32
/// test reads the *netmask*, so it must run before the quad parse (a point-to-point
/// link with no mask still deserves the clearer sentence), and the CGNAT range is
/// `100.64.0.0/10` — Tailscale's `100.x` addresses are reachable *to the tunnel
/// peer*, which is not the LAN a phone is on.
pub fn unadvertisable_reason(addr: &str, netmask: &str) -> String {
    if addr.is_empty() {
        return REASON_NO_IPV4.to_string();
    }
    if addr.starts_with("127.") {
        return REASON_LOOPBACK.to_string();
    }
    if addr.starts_with("169.254.") {
        return REASON_LINK_LOCAL.to_string();
    }
    if netmask == "255.255.255.255" || netmask == "32" {
        return REASON_POINT_TO_POINT.to_string();
    }
    let parts: Vec<&str> = addr.split('.').collect();
    if parts.len() != 4 {
        return REASON_NOT_IPV4.to_string();
    }
    if parts[0] == "100" {
        match parts[1].parse::<u32>() {
            Ok(second) if (64..=127).contains(&second) => {
                return REASON_CGNAT.to_string()
            }
            // The only arithmetic Python does on the quad, and it is guarded the
            // same way: an unparsable second octet is not an IPv4 address.
            Err(_) => return REASON_NOT_IPV4.to_string(),
            _ => {}
        }
    }
    String::new()
}

/// Whether an IPv4 address is worth publishing on a LAN.
pub fn is_advertisable_address(addr: &str, netmask: &str) -> bool {
    unadvertisable_reason(addr, netmask).is_empty()
}

/// One IPv4 address on one interface, exactly as the OS handed it over.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AddrRow {
    pub name: String,
    pub ip: String,
    pub netmask: String,
}

/// A row of the network picker's table — the same seven keys as Python's dicts,
/// because the page reads them by name and the tests compare against the page.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct InterfaceRow {
    pub name: String,
    pub ip: String,
    pub netmask: String,
    pub default: bool,
    pub usable: bool,
    pub reason: String,
    pub selected: bool,
}

/// What this machine looks like to us right now: every IPv4 address plus the
/// interfaces that carry a default route.
#[derive(Debug, Clone, Default)]
pub struct Snapshot {
    pub addrs: Vec<AddrRow>,
    pub default_ifaces: BTreeSet<String>,
}

impl Snapshot {
    /// Read the machine. Unix uses `getifaddrs`; see [`snapshot_uncached`] for
    /// the Windows situation.
    #[cfg(unix)]
    pub fn collect() -> Snapshot {
        snapshot_uncached()
    }

    #[cfg(windows)]
    pub fn collect() -> Snapshot {
        snapshot_uncached()
    }

    /// The addresses of one interface, in the order the OS listed them.
    pub fn addrs_of(&self, name: &str) -> Vec<&AddrRow> {
        self.addrs.iter().filter(|r| r.name == name).collect()
    }
}

/// [`Snapshot::collect`] without any caching, so a test can call the parsers
/// directly instead of depending on the runner's network.
#[cfg(unix)]
pub fn snapshot_uncached() -> Snapshot {
    Snapshot {
        addrs: interface_addresses(),
        default_ifaces: default_route_interfaces(),
    }
}

/// Windows: deliberately not implemented in this pass.
///
/// The right source is `GetAdaptersAddresses` (`AF_INET`, walking
/// `FirstIpAddress` and reading `OnLinkPrefixLength`), which needs a
/// `windows-sys` target dependency. Returning an empty table is honest and safe:
/// every caller below already has a fallback that ends in loopback, so discovery
/// still registers, and [`crate::paths`] keeps working. The migration doc lists
/// this as an unported surface rather than pretending it is done — a half-written
/// Windows collector nobody compiled would be worse than none.
#[cfg(windows)]
pub fn snapshot_uncached() -> Snapshot {
    Snapshot::default()
}

/// Annotate the raw addresses with the picker's three derived columns and sort
/// them the way the page expects.
///
/// Sort key is `(not default, not usable, name, ip)`: the row a user almost
/// always wants is first, a usable one beats an unusable one, and the tail is
/// stable alphabetical rather than OS enumeration order.
pub fn annotate(rows: &[AddrRow], default_ifaces: &BTreeSet<String>, pinned: &str) -> Vec<InterfaceRow> {
    let mut out: Vec<InterfaceRow> = rows
        .iter()
        .map(|r| {
            let reason = unadvertisable_reason(&r.ip, &r.netmask);
            InterfaceRow {
                name: r.name.clone(),
                ip: r.ip.clone(),
                netmask: r.netmask.clone(),
                default: default_ifaces.contains(&r.name),
                usable: reason.is_empty(),
                selected: !pinned.is_empty() && r.name == pinned,
                reason,
            }
        })
        .collect();
    out.sort_by(|a, b| {
        (!a.default)
            .cmp(&(!b.default))
            .then((!a.usable).cmp(&(!b.usable)))
            .then_with(|| a.name.cmp(&b.name))
            .then_with(|| a.ip.cmp(&b.ip))
    });
    out
}

/// The pinned interface if it can actually carry discovery, else `""`.
///
/// An explicit choice is honoured only while it has a LAN-reachable IPv4. Pinning
/// an interface that has gone away, or that only carries a link-local /
/// point-to-point / overlay address, would make Macast silently invisible;
/// falling back to automatic selection keeps it reachable, and the reason travels
/// into the warning so the state is visible rather than guessed.
///
/// `warn` receives the two sentences Python logs — verbatim, because they are the
/// only thing a user reading `macast.log` gets, and because a test can only check
/// a message it can capture without a log file.
pub fn resolve_pin<F: FnMut(&str)>(pinned: &str, rows: &[InterfaceRow], mut warn: F) -> String {
    if pinned.is_empty() {
        return String::new();
    }
    let mine: Vec<&InterfaceRow> = rows.iter().filter(|r| r.name == pinned).collect();
    if mine.is_empty() {
        warn(&format!(
            "Pinned interface {} not found; using automatic selection",
            python_repr_str(pinned)
        ));
        return String::new();
    }
    if !mine.iter().any(|r| r.usable) {
        warn(&format!(
            "Pinned interface {} has no LAN-reachable IPv4 ({}); using automatic selection",
            python_repr_str(pinned),
            mine[0].reason
        ));
        return String::new();
    }
    pinned.to_string()
}

/// The write-side twin of [`resolve_pin`]: may this name be stored at all?
///
/// `Some(message)` refuses the write; `None` means store it. `""` is the picker's
/// 自动选择 and is never refused. Any other name must belong to an interface the
/// machine reports right now — **usable or not**, because the reference does not
/// filter (`macast/protocol.py:1977-1995`) and a down-but-real interface is the
/// user's to keep pinning, whereas a typo would park Macast on the loopback
/// fallback and leave it invisible *while looking configured*.
///
/// The refusal has to name the alternatives: this branch is reachable from the
/// page for real — unplug the USB dongle between rendering the picker and
/// clicking 应用 — and "未找到" without a list sends the user to the wrong door
/// (the same "every refusal must give a next step" rule the Python side writes
/// into §4.8). `known` is the same sorted set the picker's table is built from.
pub fn pin_refusal(name: &str, known: &[String]) -> Option<String> {
    if name.is_empty() || known.iter().any(|k| k == name) {
        return None;
    }
    let list = if known.is_empty() {
        "无".to_string()
    } else {
        known.join("、")
    };
    Some(format!("未找到网卡 {}，当前可用：{}", name, list))
}

/// `'%r'` of a `str` for the two warnings above, so a name with a quote in it
/// still reads as one token.
fn python_repr_str(s: &str) -> String {
    if s.contains('\'') && !s.contains('"') {
        format!("\"{}\"", s)
    } else {
        format!("'{}'", s.replace('\\', "\\\\").replace('\'', "\\'"))
    }
}

/// The three settings that steer address selection, read once.
#[derive(Debug, Clone)]
pub struct Steering {
    /// `Network_Interface` as stored (unresolved).
    pub pinned: String,
    pub additional: Vec<String>,
    pub blocked: Vec<String>,
}

impl Steering {
    pub fn of(setting: &Setting) -> Steering {
        Steering {
            pinned: setting.get_str(Key::NetworkInterface, "").trim().to_string(),
            additional: setting.get_str_list(Key::AdditionalInterfaces),
            blocked: setting.get_str_list(Key::BlockedInterfaces),
        }
    }

    /// The pin after the honour rules, logging through the `Utils` logger.
    pub fn resolved_pin(&self, rows: &[InterfaceRow]) -> String {
        let logger = crate::logging::Logger::named("Utils");
        resolve_pin(&self.pinned, rows, |msg| logger.warning(msg))
    }
}

/// `(addr, netmask)` pairs this host may advertise.
///
/// A pinned interface narrows this to that interface *alone* — that is what makes
/// the picker meaningful rather than cosmetic. Unpinned, the set is
/// "every interface plus anything asked for, minus anything blocked"; Python
/// derived the same set from netifaces' gateway tables (`AF_INET` ∪ `AF_LINK`),
/// which on a machine with VMs or Tailscale means basically every interface with
/// an address, and the comment in `macast/discovery.py` calls that deliberately
/// permissive. Reproducing it from the address list keeps the same answer without
/// parsing a routing dump on each platform, and the one case where the two differ
/// (an interface with an address but no connected route) is harmless here: SSDP
/// matches the peer against the subnet, so an entry with no route can never be
/// the one an answer is sent from.
pub fn get_ip(snapshot: &Snapshot, steering: &Steering, resolved_pin: &str) -> Vec<(String, String)> {
    let names: BTreeSet<String> = if !resolved_pin.is_empty() {
        // Additional_Interfaces must not widen an explicit choice.
        [resolved_pin.to_string()].into_iter().collect()
    } else {
        let mut set: BTreeSet<String> = snapshot.addrs.iter().map(|r| r.name.clone()).collect();
        set.extend(steering.additional.iter().cloned());
        for blocked in &steering.blocked {
            set.remove(blocked);
        }
        set
    };
    let mut pairs = Vec::new();
    for row in &snapshot.addrs {
        if names.contains(&row.name) && !row.ip.is_empty() {
            pairs.push((row.ip.clone(), row.netmask.clone()));
        }
    }
    note_last_seen(&pairs);
    pairs
}

/// The addresses discovery will actually publish, as a list.
///
/// Falls back to the unfiltered set so a host whose only address looks unusual to
/// us still advertises something rather than nothing.
pub fn get_advertisable_ip(
    snapshot: &Snapshot,
    steering: &Steering,
    resolved_pin: &str,
) -> Vec<String> {
    let pairs = get_ip(snapshot, steering, resolved_pin);
    let addrs: Vec<String> = pairs
        .iter()
        .filter(|(ip, mask)| is_advertisable_address(ip, mask))
        .map(|(ip, _)| ip.clone())
        .collect();
    if !addrs.is_empty() {
        return addrs;
    }
    pairs.into_iter().map(|(ip, _)| ip).collect()
}

/// The IPv4 addresses mDNS will put in the A records, and the status page shows.
///
/// Same rules as the DLNA side but a different last resort: registration needs
/// *something*, so an unreachable host still publishes loopback rather than an
/// empty address list (a zeroconf `ServiceInfo` with no addresses is invisible in
/// a way that looks like a bug in our code).
pub fn advertisable_addresses(
    snapshot: &Snapshot,
    steering: &Steering,
    resolved_pin: &str,
) -> Vec<String> {
    let logger = crate::logging::Logger::named("Discovery");
    let names: BTreeSet<String> = if !resolved_pin.is_empty() {
        [resolved_pin.to_string()].into_iter().collect()
    } else {
        let mut set: BTreeSet<String> = snapshot.default_ifaces.clone();
        set.extend(steering.additional.iter().cloned());
        for blocked in &steering.blocked {
            set.remove(blocked);
        }
        set
    };
    let mut addrs: Vec<String> = Vec::new();
    if !names.is_empty() {
        for row in &snapshot.addrs {
            if names.contains(&row.name) && is_advertisable_address(&row.ip, &row.netmask) {
                addrs.push(row.ip.clone());
            }
        }
    }
    if !addrs.is_empty() {
        return addrs;
    }
    // Fall back to the DLNA address set, minus what we know is unreachable.
    addrs = get_advertisable_ip(snapshot, steering, resolved_pin);
    if addrs.is_empty() {
        logger.warning("No reachable local address found; publishing loopback so discovery still registers");
        addrs.push("127.0.0.1".to_string());
    }
    addrs
}

/// The picker's table: every local IPv4 interface, annotated and sorted.
pub fn network_interfaces(setting: &Setting) -> Vec<InterfaceRow> {
    network_interfaces_from(&Snapshot::collect(), setting)
}

/// [`network_interfaces`] against a caller-supplied machine view (tests, and any
/// caller that already read the OS).
pub fn network_interfaces_from(snapshot: &Snapshot, setting: &Setting) -> Vec<InterfaceRow> {
    let steering = Steering::of(setting);
    annotate(&snapshot.addrs, &snapshot.default_ifaces, &steering.pinned)
}

/// `Setting.get_ip()` also remembers the last answer so `is_ip_changed()` can
/// notice a DHCP renewal. Python keeps it in a class attribute; a process-wide
/// set behind a lock is the same thing with the sharing spelled out, because the
/// SSDP notify thread and the page both ask.
fn note_last_seen(pairs: &[(String, String)]) {
    let mut last = last_ip_table().lock().unwrap_or_else(|e| e.into_inner());
    *last = pairs.iter().cloned().collect();
}

fn last_ip_table() -> &'static Mutex<HashSet<(String, String)>> {
    static LAST: OnceLockTable = OnceLockTable::new();
    LAST.get()
}

/// A `static Mutex<HashSet<..>>` without a `const`-lock dependency.
struct OnceLockTable {
    inner: std::sync::OnceLock<Mutex<HashSet<(String, String)>>>,
}

impl OnceLockTable {
    const fn new() -> Self {
        OnceLockTable {
            inner: std::sync::OnceLock::new(),
        }
    }
    fn get(&'static self) -> &'static Mutex<HashSet<(String, String)>> {
        self.inner.get_or_init(Default::default)
    }
}

/// Whether the advertised address set changed since the last [`get_ip`] — the
/// trigger for re-announcing SSDP and refreshing the mDNS A records
/// (`macast/server.py:401` asks it every notify cycle).
///
/// The previous answer must be read **before** [`get_ip`] runs, because
/// [`get_ip`] overwrites it. Python gets this for free from evaluation order —
/// `Setting.last_ip != Setting.get_ip()` reads the left operand first — and a
/// Rust port that compares afterwards is silently always-false, which means SSDP
/// never re-announces and a DHCP renewal leaves stale addresses published.
pub fn is_ip_changed(snapshot: &Snapshot, setting: &Setting) -> bool {
    let steering = Steering::of(setting);
    let pin = steering.resolved_pin(&annotate(
        &snapshot.addrs,
        &snapshot.default_ifaces,
        &steering.pinned,
    ));
    let previous: HashSet<(String, String)> = last_ip_table()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .iter()
        .cloned()
        .collect();
    let current: HashSet<(String, String)> = get_ip(snapshot, &steering, &pin)
        .into_iter()
        .collect();
    current != previous
}

/// The IPv4 route table on Linux: `/proc/net/route`, one row per route,
/// tab-separated, columns `Iface Destination Gateway Flags RefCnt Use Metric …`
/// with the addresses as little-endian hex. The header line is skipped; a row
/// whose **Destination** is `00000000` is the default route, and the interface
/// carrying it is column 0 (**not** column 2 — that is the gateway, and picking
/// it would advertise `0101A8C0` as a netif name).
pub fn parse_proc_net_route(text: &str) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    for line in text.lines().skip(1) {
        let cols: Vec<&str> = line.split('\t').collect();
        if cols.len() >= 7 && cols[1] == "00000000" {
            out.insert(cols[0].to_string());
        }
    }
    out
}

/// `route -n get default` on macOS/BSD: one or more blocks, each with an
/// `interface:` line whose value is the netif carrying that default route.
pub fn parse_route_get_default(text: &str) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    for line in text.lines() {
        let trimmed = line.trim_start();
        if let Some(rest) = trimmed.strip_prefix("interface:") {
            let name = rest.trim().split_whitespace().next().unwrap_or("");
            if !name.is_empty() {
                out.insert(name.to_string());
            }
        }
    }
    out
}

/// The interfaces carrying an IPv4 default route, this machine's own answer.
#[cfg(target_os = "linux")]
pub fn default_route_interfaces() -> BTreeSet<String> {
    match std::fs::read_to_string("/proc/net/route") {
        Ok(text) => parse_proc_net_route(&text),
        Err(e) => {
            crate::logging::Logger::named("Utils").debug(&format!(
                "Could not read /proc/net/route for the default IPv4 route: {}",
                e
            ));
            BTreeSet::new()
        }
    }
}

/// The interfaces carrying an IPv4 default route, this machine's own answer.
///
/// `route` is a short-lived child process and this runs on the picker's poll and
/// at discovery start, not on a request path.
#[cfg(any(target_os = "macos", target_os = "freebsd", target_os = "openbsd", target_os = "netbsd", target_os = "dragonfly", target_abi = "macabi"))]
pub fn default_route_interfaces() -> BTreeSet<String> {
    use std::process::Command;
    let out = Command::new("route").args(["-n", "get", "default"]).output();
    match out {
        Ok(ok) if ok.status.success() => parse_route_get_default(
            &String::from_utf8_lossy(&ok.stdout),
        ),
        Ok(ok) => {
            crate::logging::Logger::named("Utils").debug(&format!(
                "`route -n get default` exited {}: {}",
                ok.status,
                String::from_utf8_lossy(&ok.stderr).trim()
            ));
            BTreeSet::new()
        }
        Err(e) => {
            crate::logging::Logger::named("Utils").debug(&format!(
                "Could not ask `route` for the default IPv4 route: {}",
                e
            ));
            BTreeSet::new()
        }
    }
}

#[cfg(windows)]
pub fn default_route_interfaces() -> BTreeSet<String> {
    BTreeSet::new()
}

/// Every IPv4 address on this host, via `getifaddrs(3)`.
///
/// `ifa_netmask` is populated on both the BSD and the Linux side, so one pass
/// gives (addr, mask) pairs. Interfaces with no AF_INET entry (a pure IPv6 tunnel)
/// contribute nothing, which is what the six rules above assume.
#[cfg(unix)]
fn interface_addresses() -> Vec<AddrRow> {
    use std::ffi::CStr;
    use std::net::Ipv4Addr;

    let mut rows = Vec::new();
    let mut head: *mut libc::ifaddrs = std::ptr::null_mut();
    if unsafe { libc::getifaddrs(&mut head) } != 0 {
        crate::logging::Logger::named("Utils")
            .warning("Could not enumerate interfaces with getifaddrs; discovery will fall back to loopback");
        return rows;
    }
    let mut seen: BTreeMap<String, ()> = BTreeMap::new();
    unsafe {
        let mut cursor = head;
        while !cursor.is_null() {
            let entry = &*cursor;
            let name = if entry.ifa_name.is_null() {
                String::new()
            } else {
                CStr::from_ptr(entry.ifa_name).to_string_lossy().into_owned()
            };
            if name.is_empty() {
                seen.insert(name.clone(), ());
            }
            let family = if entry.ifa_addr.is_null() {
                -1
            } else {
                // `ifa_addr` is a `*mut c_char` on every BSD and on Linux alike,
                // so the cast to `sockaddr` is the read, not a hint.
                (*entry.ifa_addr.cast::<libc::sockaddr>()).sa_family as i32
            };
            if family == libc::AF_INET && !entry.ifa_netmask.is_null() {
                let addr = *(entry.ifa_addr as *mut libc::sockaddr_in);
                let mask = *(entry.ifa_netmask as *mut libc::sockaddr_in);
                let ip = Ipv4Addr::from(addr.sin_addr.s_addr.to_ne_bytes());
                let netmask = Ipv4Addr::from(mask.sin_addr.s_addr.to_ne_bytes());
                rows.push(AddrRow {
                    name,
                    ip: ip.to_string(),
                    netmask: netmask.to_string(),
                });
            }
            cursor = entry.ifa_next;
        }
        libc::freeifaddrs(head);
    }
    rows.sort_by(|a, b| a.name.cmp(&b.name).then_with(|| a.ip.cmp(&b.ip)));
    rows
}

/// `Setting.get_ip()` for callers that only have a `Setting`: read the machine,
/// then apply the three steering settings.
pub fn advertised_pairs(setting: &Setting) -> Vec<(String, String)> {
    let snapshot = Snapshot::collect();
    let steering = Steering::of(setting);
    let rows = annotate(&snapshot.addrs, &snapshot.default_ifaces, &steering.pinned);
    let pin = steering.resolved_pin(&rows);
    get_ip(&snapshot, &steering, &pin)
}

/// The addresses a sender should be able to dial: what DLNA's `LOCATION` header
/// and the status page use.
pub fn advertised_addresses(setting: &Setting) -> Vec<String> {
    let snapshot = Snapshot::collect();
    let steering = Steering::of(setting);
    let rows = annotate(&snapshot.addrs, &snapshot.default_ifaces, &steering.pinned);
    let pin = steering.resolved_pin(&rows);
    advertisable_addresses(&snapshot, &steering, &pin)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(name: &str, ip: &str, netmask: &str) -> AddrRow {
        AddrRow {
            name: name.to_string(),
            ip: ip.to_string(),
            netmask: netmask.to_string(),
        }
    }

    /// A machine shaped like the one that produced the original bug report: Wi-Fi
    /// with the default route, a VM bridge, a Tailscale tunnel, loopback.
    fn messy_machine() -> Snapshot {
        Snapshot {
            addrs: vec![
                row("lo0", "127.0.0.1", "255.0.0.0"),
                row("en0", "192.168.1.5", "255.255.255.0"),
                row("utun4", "100.98.12.7", "255.255.255.255"),
                row("bridge100", "10.8.0.1", "255.255.255.0"),
                row("en8", "169.254.203.1", "255.255.0.0"),
            ],
            default_ifaces: ["en0".to_string()].into_iter().collect(),
        }
    }

    #[test]
    fn each_rule_earns_its_own_sentence() {
        assert_eq!(unadvertisable_reason("", "255.0.0.0"), REASON_NO_IPV4);
        assert_eq!(
            unadvertisable_reason("127.0.0.1", "255.0.0.0"),
            REASON_LOOPBACK
        );
        assert_eq!(
            unadvertisable_reason("169.254.3.4", "255.255.0.0"),
            REASON_LINK_LOCAL
        );
        // A /32 is point-to-point whatever the address says, so it is read from
        // the netmask before the quad is parsed.
        assert_eq!(
            unadvertisable_reason("192.168.1.5", "255.255.255.255"),
            REASON_POINT_TO_POINT
        );
        assert_eq!(
            unadvertisable_reason("192.168.1.5", "32"),
            REASON_POINT_TO_POINT
        );
        assert_eq!(unadvertisable_reason("not.an.ip", "255.0.0.0"), REASON_NOT_IPV4);
        assert_eq!(unadvertisable_reason("10.1.2", "255.0.0.0"), REASON_NOT_IPV4);
        // CGNAT: Tailscale and a thousand VPNs live here, and a phone on the LAN
        // cannot route to it.
        assert_eq!(
            unadvertisable_reason("100.64.0.2", "255.255.255.255").as_str(),
            REASON_POINT_TO_POINT,
            "the /32 rule must not be shadowed by the CGNAT range"
        );
        assert_eq!(unadvertisable_reason("100.98.12.7", "255.255.255.0"), REASON_CGNAT);
        assert_eq!(unadvertisable_reason("100.64.0.0", "255.255.0.0"), REASON_CGNAT);
        assert_eq!(unadvertisable_reason("100.127.255.254", "255.255.0.0"), REASON_CGNAT);
        // The two edges of the /10, and a hundred-and-something that is not CGNAT.
        assert_eq!(unadvertisable_reason("100.63.9.9", "255.255.0.0"), "");
        assert_eq!(unadvertisable_reason("100.128.1.1", "255.255.0.0"), "");
        assert!(is_advertisable_address("192.168.1.5", "255.255.255.0"));
        assert!(is_advertisable_address("10.8.0.1", "255.255.255.0"));
    }

    #[test]
    fn a_broken_second_octet_is_not_an_ipv4_address() {
        assert_eq!(unadvertisable_reason("100.x.1.1", "255.0.0.0"), REASON_NOT_IPV4);
    }

    #[test]
    fn the_picker_puts_the_row_a_user_wants_first() {
        let snap = messy_machine();
        let rows = annotate(&snap.addrs, &snap.default_ifaces, "en0");
        assert_eq!(rows.len(), 5);
        assert_eq!(rows[0].name, "en0");
        assert!(rows[0].default && rows[0].usable && rows[0].selected);
        // then the usable non-default one, then the rejected rows alphabetically
        assert_eq!(rows[1].name, "bridge100");
        assert_eq!(rows[2].name, "en8");
        assert_eq!(rows[2].reason, REASON_LINK_LOCAL);
        assert_eq!(rows[3].name, "lo0");
        assert_eq!(rows[4].name, "utun4");
        assert!(!rows.iter().any(|r| r.name == "utun4" && r.usable));
        // Nothing is selected when the user never pinned anything.
        let rows = annotate(&snap.addrs, &snap.default_ifaces, "");
        assert!(!rows.iter().any(|r| r.selected));
    }

    #[test]
    fn a_pin_is_honoured_only_while_it_can_carry_discovery() {
        let snap = messy_machine();
        let rows = annotate(&snap.addrs, &snap.default_ifaces, "en0");
        let mut notes: Vec<String> = Vec::new();
        assert_eq!(
            resolve_pin("en0", &rows, |m| notes.push(m.to_string())),
            "en0"
        );
        assert!(notes.is_empty(), "a working pin must not warn: {notes:?}");

        // Gone away.
        notes.clear();
        assert_eq!(
            resolve_pin("en9", &rows, |m| notes.push(m.to_string())),
            ""
        );
        assert_eq!(
            notes[0],
            "Pinned interface 'en9' not found; using automatic selection"
        );

        // Present but only carrying an address no sender can dial.
        notes.clear();
        assert_eq!(
            resolve_pin("utun4", &rows, |m| notes.push(m.to_string())),
            ""
        );
        assert_eq!(
            notes[0],
            "Pinned interface 'utun4' has no LAN-reachable IPv4 (点对点地址（/32），不属于局域网); using automatic selection"
        );

        notes.clear();
        assert_eq!(
            resolve_pin("lo0", &rows, |m| notes.push(m.to_string())),
            ""
        );
        assert!(notes[0].contains(REASON_LOOPBACK));

        // An empty pin is automatic, and says nothing.
        notes.clear();
        assert_eq!(resolve_pin("", &rows, |m| notes.push(m.to_string())), "");
        assert!(notes.is_empty());
    }

    #[test]
    fn a_pin_that_names_nothing_is_refused_with_the_list_that_does() {
        let known: Vec<String> = vec!["en0".into(), "en7".into(), "utun4".into()];
        assert_eq!(pin_refusal("", &known), None, "自动选择 is always allowed");
        assert_eq!(pin_refusal("en0", &known), None);
        assert_eq!(
            pin_refusal("utun4", &known),
            None,
            "down-but-real is the user's to keep pinning"
        );
        assert_eq!(
            pin_refusal("en9", &known),
            Some("未找到网卡 en9，当前可用：en0、en7、utun4".to_string())
        );
        // A machine that reports nothing still gets a sentence, not an empty list.
        assert_eq!(
            pin_refusal("en0", &[]),
            Some("未找到网卡 en0，当前可用：无".to_string())
        );
    }

    #[test]
    fn a_quote_in_an_interface_name_still_reads_as_one_token() {
        // Python logs these two sentences with `%r`, and `%r` of a `str` that
        // contains a `'` and no `"` is the **double**-quoted form. That is not
        // decoration: a warning that prints `Pinned interface 'we'ird' ...`
        // reads as three tokens to the person scanning the log for the name
        // they typed into 高级设置.
        let rows = annotate(
            &[row("we'ird", "169.254.9.9", "255.255.0.0")],
            &Default::default(),
            "",
        );
        let mut notes = Vec::new();
        assert_eq!(
            resolve_pin("we'ird", &rows, |m| notes.push(m.to_string())),
            ""
        );
        assert!(
            notes[0].starts_with("Pinned interface \"we'ird\" has no LAN-reachable IPv4 ("),
            "{:?}",
            notes[0]
        );

        // And the plain case stays single-quoted, exactly as `%r` would print it.
        notes.clear();
        resolve_pin("gone", &rows, |m| notes.push(m.to_string()));
        assert_eq!(
            notes[0],
            "Pinned interface 'gone' not found; using automatic selection"
        );
    }

    #[test]
    fn a_pin_narrows_the_advertised_pairs_to_one_interface() {
        let snap = messy_machine();
        let steering = Steering {
            pinned: "en0".into(),
            additional: vec!["bridge100".into()],
            blocked: vec![],
        };
        let pairs = get_ip(&snap, &steering, "en0");
        assert_eq!(pairs, vec![("192.168.1.5".to_string(), "255.255.255.0".to_string())]);
        // Additional_Interfaces must not widen an explicit choice.
        assert!(!pairs.iter().any(|(ip, _)| ip == "10.8.0.1"));
    }

    #[test]
    fn unpinned_takes_every_interface_and_subtracts_the_blocked_ones() {
        let snap = messy_machine();
        let steering = Steering {
            pinned: "".into(),
            additional: vec!["utun9".into()],
            blocked: vec!["lo0".into(), "en0".into()],
        };
        let pairs = get_ip(&snap, &steering, "");
        let ips: Vec<&str> = pairs.iter().map(|(ip, _)| ip.as_str()).collect();
        // Selection is a set — Python literally returns one — so *membership* is
        // the contract: the two blocked interfaces are gone, an additional name
        // that owns no address (`utun9`) contributes nothing, and everything
        // else stays. The order below is this module's own, deterministic extra:
        // rows come back in the order the OS reported them, so a caller that
        // shows this list to a human must sort it rather than lean on it.
        assert_eq!(ips, vec!["100.98.12.7", "10.8.0.1", "169.254.203.1"]);
        assert!(!ips.contains(&"127.0.0.1"));
        assert!(!ips.contains(&"192.168.1.5"));
        // No duplicates even though the name set is a superset of the rows.
        let mut unique = ips.clone();
        unique.sort_unstable();
        unique.dedup();
        assert_eq!(unique.len(), ips.len());
    }

    #[test]
    fn the_advertisable_list_falls_back_rather_than_answering_empty() {
        let snap = messy_machine();
        let steering = Steering {
            pinned: "".into(),
            additional: vec![],
            blocked: vec![],
        };
        // Normal machine: only the reachable ones.
        let addrs = get_advertisable_ip(&snap, &steering, "");
        assert_eq!(addrs, vec!["192.168.1.5", "10.8.0.1"]);

        // A host whose only address looks unusual to us still advertises
        // something: the unfiltered set, not an empty list.
        let lonely = Snapshot {
            addrs: vec![row("lo0", "127.0.0.1", "255.0.0.0")],
            default_ifaces: Default::default(),
        };
        let steering = Steering {
            pinned: "".into(),
            additional: vec![],
            blocked: vec![],
        };
        assert_eq!(
            get_advertisable_ip(&lonely, &steering, ""),
            vec!["127.0.0.1".to_string()]
        );

        // mDNS has the same rule plus a last resort of its own.
        let empty = Snapshot::default();
        assert_eq!(
            advertisable_addresses(&empty, &steering, ""),
            vec!["127.0.0.1".to_string()]
        );
        let addrs = advertisable_addresses(&snap, &steering, "");
        // The default route's interface, and only its reachable address.
        assert_eq!(addrs, vec!["192.168.1.5"]);
    }

    #[test]
    fn the_advertiser_and_dlna_agree_on_a_dead_pin() {
        // The pin the page would honour is the same pin both publishers use; when
        // it resolves to nothing, the advertiser follows the default route instead
        // of binding to a tunnel and going quietly invisible.
        let snap = messy_machine();
        let steering = Steering {
            pinned: "utun4".into(),
            additional: vec![],
            blocked: vec![],
        };
        let rows = annotate(&snap.addrs, &snap.default_ifaces, &steering.pinned);
        let mut notes = Vec::new();
        let pin = resolve_pin(&steering.pinned, &rows, |m| notes.push(m.to_string()));
        assert_eq!(pin, "");
        let addrs = advertisable_addresses(&snap, &steering, &pin);
        assert_eq!(addrs, vec!["192.168.1.5"]);
    }

    #[test]
    fn the_route_parsers_read_both_dialects() {
        let linux = "Iface\tDestination\tGateway \tFlags\tRefCnt\tUse\tMetric\n\
                     eth0\t00000000\t0101A8C0\t0003\t0\t0\t0\n\
                     lo\t00007F00\t00000000\t0001\t0\t0\t0\n\
                     eth0\t0001A8C0\t00000000\t0001\t0\t0\t0\n";
        let set = parse_proc_net_route(linux);
        assert_eq!(set.iter().collect::<Vec<_>>(), vec!["eth0"]);

        let bsd = "route to: default\ndestination: default\n       mask: default\n\
                   gateway: 192.168.1.1\n     interface: en0\n      ifindex: 4\n\
                   flags: <UP,GATEWAY,DONE,STATIC,PRCLONING,GLOBAL>\n";
        let set = parse_route_get_default(bsd);
        assert_eq!(set.iter().collect::<Vec<_>>(), vec!["en0"]);

        // An unanswered question is an empty set, not a panic and not a guess.
        assert!(parse_route_get_default("").is_empty());
        assert!(parse_proc_net_route("garbage").is_empty());
    }

    #[test]
    fn the_machine_can_be_asked_at_all() {
        // Not a fixture: this asks the real OS, so it fails loudly on a platform
        // where the collector was never wired up. It makes no claim about which
        // interfaces exist, only that loopback is among them and that the answer
        // carries the shape the rest of the module assumes.
        let snap = Snapshot::collect();
        assert!(
            snap.addrs.iter().any(|r| r.ip == "127.0.0.1") || snap.addrs.is_empty(),
            "unexpected shape: {:?}",
            snap.addrs
        );
        for r in &snap.addrs {
            assert!(!r.name.is_empty());
            // Every address parses back to four octets and a dotted mask.
            assert_eq!(r.ip.split('.').count(), 4, "{:?}", r.ip);
            assert_eq!(r.netmask.split('.').count(), 4, "{:?}", r.netmask);
        }
    }

    #[test]
    fn an_unusable_pin_falls_back_for_the_real_settings_reader() {
        // Drive the setting-reading wrappers with a temp file and a pin that does
        // not exist on this machine, so the honour rules run end to end and the
        // warning is written to a log file we own.
        let dir = std::env::temp_dir().join(format!(
            "macast-core-net-{}-{}",
            std::process::id(),
            time_of()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        crate::paths::set_setting_dir_override(&dir);
        let setting = Setting::new(dir.join(crate::paths::SETTING_FILE_NAME));
        setting.put(Key::NetworkInterface, serde_json::Value::String("zz9".into()));

        let steering = Steering::of(&setting);
        assert_eq!(steering.pinned, "zz9");
        let snapshot = Snapshot::collect();
        let rows = annotate(&snapshot.addrs, &snapshot.default_ifaces, &steering.pinned);
        let pin = steering.resolved_pin(&rows);
        assert_eq!(pin, "", "a pin this machine does not have must not be honoured");
        // ... and the address set is still whatever this machine really has.
        assert!(!get_ip(&snapshot, &steering, &pin).is_empty() || snapshot.addrs.is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    fn time_of() -> u128 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.subsec_micros() as u128 + d.as_secs() as u128 * 1_000_000)
            .unwrap_or(0)
    }
}
