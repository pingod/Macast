# Copyright (c) 2021 by xfangfang. All Rights Reserved.
#
# mDNS / DNS-SD advertisement helper.
#
# DLNA uses SSDP for discovery, but Chromecast and AirPlay are discovered over
# mDNS (DNS-SD). This small wrapper around `zeroconf` lets a Protocol plugin
# advertise itself on the local network without touching the rest of Macast.
#
# Chromecast:  service type  _googlecast._tcp.local.
# AirPlay:     service type  _airplay._tcp.local.   (video / screen)
#              optionally     _raop._tcp.local.      (audio, future work)
#

import logging
import re
import socket
import threading

from zeroconf import Zeroconf, ServiceInfo

from .utils import Setting, SettingProperty, is_advertisable_address

logger = logging.getLogger("Discovery")

# A DNS-SD instance name is a single DNS label: letters, digits and hyphens
# only, at most 63 bytes. Macast's *friendly name* predates mDNS and is built
# for DLNA, where it is free text — the default is
# ``Macast(MyHost-1754.local)``, parentheses and dots included.
# zeroconf accepts such a name without complaint and reports success, but the
# record never matches a browse query, so the device is silently invisible.
# Normalising here means no caller has to remember the rule.
_INSTANCE_ILLEGAL = re.compile(r"[^A-Za-z0-9-]")
_MAX_LABEL = 63


def sanitize_instance_name(name, service_type, fallback="Macast"):
    """Return a DNS-SD-legal full name for ``name`` under ``service_type``.

    ``name`` may be given with or without the ``service_type`` suffix.
    """
    suffix = service_type if service_type.endswith(".") else service_type + "."
    if name.endswith(suffix):
        stem = name[: -len(suffix)].rstrip(".")
    elif name.endswith(suffix.rstrip(".")):
        stem = name[: -len(suffix.rstrip("."))].rstrip(".")
    else:
        stem = name.rstrip(".")
    # The stem may itself carry labels (e.g. "Macast(host.local)") — keep only
    # the leading label, mirroring how a real Cast device names itself.
    stem = stem.split(".")[0]
    stem = _INSTANCE_ILLEGAL.sub("-", stem).strip("-")[:_MAX_LABEL]
    if not stem:
        stem = fallback
    return "{}.{}".format(stem, suffix)

# ---------------------------------------------------------------------------
# Shared Zeroconf instance
#
# Every protocol used to build its own Zeroconf, which meant one 5353 socket
# per protocol. Two sockets is not just wasteful: zeroconf logs a burst of
# `sendto ... Can't assign requested address` warnings and, on hosts with
# several interfaces, announcements can be split across them. One instance,
# refcounted, keeps the LAN view consistent and makes stop/start cheap.
# ---------------------------------------------------------------------------

_ZC_LOCK = threading.Lock()
_ZC = None
_ZC_USERS = 0


def _new_zeroconf():
    """Create a Zeroconf that can actually send on this host.

    The default is dual-stack, and the IPv6 socket is the one that fails on
    machines where no interface has a routable IPv6 multicast address: zeroconf
    binds it to 0.0.0.0:5353 and every sendto() then raises
    ``OSError: [Errno 49] Can't assign requested address``. The failure is only
    logged, but the announcement for that socket never leaves the machine, so
    `dns-sd -B` finds nothing. Restricting to IPv4 avoids it.
    """
    try:
        from zeroconf import IPVersion
        return Zeroconf(ip_version=IPVersion.V4Only)
    except Exception as e:  # older/newer zeroconf without IPVersion
        logger.debug("Zeroconf(ip_version=...) unavailable (%s), using default", e)
        return Zeroconf()


def _acquire_zc():
    global _ZC, _ZC_USERS
    with _ZC_LOCK:
        if _ZC is None:
            _ZC = _new_zeroconf()
        _ZC_USERS += 1
        return _ZC


def _release_zc():
    global _ZC, _ZC_USERS
    with _ZC_LOCK:
        _ZC_USERS -= 1
        if _ZC_USERS > 0 or _ZC is None:
            return
        _ZC_USERS = 0
        zc, _ZC = _ZC, None
    try:
        zc.close()
    except Exception as e:  # pragma: no cover - defensive
        logger.warning("mDNS shutdown error: %s", e)


def _normalize_server(host):
    """Return a fully-qualified .local. hostname without doubling the suffix."""
    host = (host or "").rstrip(".")
    if not host:
        host = socket.gethostname().rstrip(".")
    if not host.lower().endswith(".local"):
        host += ".local"
    return host + "."


# `_is_advertisable` used to live here; it now lives in utils as
# `is_advertisable_address()` so the network picker and the advertiser share
# one definition of "reachable".


def _pinned_interface():
    """The interface the user pinned, if it can actually carry the service.

    Delegates to ``Setting.resolved_network_interface()`` so the advertiser and
    SSDP agree on which interface is in use -- they used to answer this
    question separately and could disagree, leaving mDNS on Wi-Fi while SSDP
    bound to a dead tunnel.
    """
    return Setting.resolved_network_interface() or None


def _advertisable_interfaces():
    """Interfaces whose addresses other devices on the LAN can actually reach.

    ``Setting.get_ip()`` is deliberately permissive: it treats every interface
    that carries *any* gateway entry as usable, including the ``AF_LINK`` ones.
    On a machine running VMs or Tailscale that pulls in VM bridges and the
    Tailscale utun, so it returns five addresses where only one -- the Wi-Fi
    one -- is reachable by a phone.

    For mDNS that matters more than anywhere else: zeroconf publishes *every*
    address as an A record for the SRV host, so a sender resolving the device
    gets five answers, picks (roughly) at random, and usually fails to connect.
    The device is discovered and then cannot be cast to.

    We therefore advertise the interface the user pinned, or failing that only
    the one carrying the IPv4 default route, plus anything explicitly asked for
    via ``Additional_Interfaces``.
    """
    try:
        import netifaces as ni
    except ImportError:  # pragma: no cover - netifaces is a hard dep
        return None

    pinned = _pinned_interface()
    if pinned:
        return {pinned}

    blocked = set(Setting.get(SettingProperty.Blocked_Interfaces, []))
    extra = set(Setting.get(SettingProperty.Additional_Interfaces, []))
    ifaces = set()
    try:
        gateways = ni.gateways() or {}
        for entry in gateways.get(ni.AF_INET, []):
            # ('192.168.1.1', 'en0', True) -- last item marks the default route
            if len(entry) > 1:
                ifaces.add(entry[1])
    except Exception as e:  # pragma: no cover - defensive
        logger.debug("Could not read IPv4 gateways: %s", e)
    ifaces |= extra
    ifaces -= blocked
    return ifaces or None


def advertisable_addresses():
    """IPv4 addresses on this host that other devices can actually reach.

    Public counterpart of ``_local_addresses()``; also used by the status page
    so it reports the address a phone would connect to rather than every
    address on the machine.
    """
    ifaces = _advertisable_interfaces()
    addrs = []
    if ifaces:
        try:
            import netifaces as ni
            for name in ifaces:
                try:
                    addrs_v4 = ni.ifaddresses(name).get(ni.AF_INET, [])
                except ValueError:
                    continue
                for entry in addrs_v4:
                    ip = entry.get("addr")
                    if is_advertisable_address(ip, entry.get("netmask", "")):
                        addrs.append(ip)
        except Exception as e:  # pragma: no cover - defensive
            logger.warning("Failed to enumerate local addresses: %s", e)

    # Fall back to the DLNA address set, minus the addresses we know are
    # unreachable, so a host with an unusual network still advertises something.
    if not addrs:
        try:
            addrs = Setting.get_advertisable_ip()
        except Exception as e:  # pragma: no cover - defensive
            logger.warning("Failed to enumerate local addresses: %s", e)

    if not addrs:
        # Last resort: loopback, so registration at least succeeds locally.
        addrs = ["127.0.0.1"]
    return addrs


def _local_addresses():
    """Return the addresses to publish, as 4-byte packed strings."""
    return [socket.inet_aton(a) for a in advertisable_addresses()]


class MDNSAdvertiser:
    """Thin, restart-safe wrapper around the shared Zeroconf instance.

    Registered services are re-announced on a timer because some senders only
    *listen*: libvlc's renderer discovery starts microdns with
    ``mDNS: listening to _googlecast._tcp.local renderer`` and never sends a
    query of its own (a 5353 sniffer on the same LAN sees no packets from the
    phone at all), so a device it did not hear announce is invisible to it. Our
    records carry a 120s TTL, so one announcement at protocol start is not
    enough to stay visible -- which is why "the phone only sees Macast after a
    plugin restart / stop casting" used to be the workaround.
    """

    #: Re-announce interval, comfortably inside the 120s TTL.
    REANNOUNCE_SECONDS = 60

    def __init__(self):
        self._zc = None
        self._registered = {}  # key "type::name" -> ServiceInfo
        self._stop = threading.Event()
        self._thread = None

    def _ensure_zc(self):
        if self._zc is None:
            self._zc = _acquire_zc()
        return self._zc

    def advertise(self, service_type, name, port, properties, server=None):
        """Advertise a DNS-SD service.

        :param service_type: e.g. '_googlecast._tcp.local.'
        :param name:         e.g. 'Macast._googlecast._tcp.local.'
        :param port:         TCP port the service listens on
        :param properties:   dict of TXT records (values str or bytes)
        :param server:       optional hostname advertised in the SRV record

        Returns True on success. Registration failures are logged and swallowed:
        a machine with mDNS blocked should still be usable as a DLNA renderer
        rather than taking the whole app down.
        """
        try:
            zc = self._ensure_zc()
        except Exception as e:
            logger.error("mDNS unavailable, %s will not be discoverable: %s",
                         service_type, e)
            return False

        txt = {}
        for k, v in (properties or {}).items():
            if isinstance(v, str):
                txt[k] = v.encode("utf-8")
            else:
                txt[k] = v
        safe_name = sanitize_instance_name(name, service_type)
        if safe_name != name:
            logger.info("mDNS name %r normalised to %r for DNS-SD", name, safe_name)
        if server:
            server = _normalize_server(server)
        else:
            # Default the SRV target to a hostname derived from the service
            # instance, the way a real Chromecast does, rather than the machine
            # hostname. The machine name is already advertised by the OS with
            # its own (possibly different) addresses; publishing ours under a
            # name we own keeps the A records consistent with what we publish.
            server = _normalize_server(safe_name.split(".")[0])
        info = ServiceInfo(
            service_type,
            safe_name,
            addresses=_local_addresses(),
            port=port,
            properties=txt,
            server=server,
        )
        # Key on the name the caller passed, so unadvertise() stays symmetric
        # with advertise() even though the registered record uses safe_name.
        key = "{}::{}".format(service_type, name)
        try:
            zc.register_service(info)
            self._registered[key] = info
            logger.info("mDNS advertised %s on port %d", safe_name, port)
            self._ensure_keepalive()
            return True
        except Exception as e:
            logger.error("mDNS advertise failed for %s: %s", safe_name, e)
            return False

    def _ensure_keepalive(self):
        if self._thread is not None and self._thread.is_alive():
            return
        self._stop.clear()
        self._thread = threading.Thread(
            target=self._keepalive, name="MDNS_KEEPALIVE", daemon=True)
        self._thread.start()

    def _keepalive(self):
        while not self._stop.wait(self.REANNOUNCE_SECONDS):
            self.reannounce()

    def reannounce(self):
        """Re-broadcast every registered service, without a goodbye in between.

        ``Zeroconf.update_service`` re-publishes the records (it is the same
        call path as registration minus the goodbye), so listeners that missed
        the first announcement -- or whose cache has expired -- pick the device
        up on their own.
        """
        if self._zc is None:
            return 0
        sent = 0
        for info in list(self._registered.values()):
            try:
                self._zc.update_service(info)
                sent += 1
            except Exception as e:  # pragma: no cover - defensive
                logger.debug("mDNS re-announce failed: %s", e)
        if sent:
            logger.debug("mDNS re-announced %d service(s)", sent)
        return sent

    def unadvertise(self, service_type, name):
        key = "{}::{}".format(service_type, name)
        info = self._registered.pop(key, None)
        if info is not None and self._zc is not None:
            try:
                self._zc.unregister_service(info)
            except Exception as e:  # pragma: no cover - defensive
                logger.warning("mDNS unregister failed: %s", e)

    def close(self):
        self._stop.set()
        thread, self._thread = self._thread, None
        if thread is not None and thread.is_alive():
            thread.join(timeout=2)
        if self._zc is None:
            return
        for key in list(self._registered.keys()):
            st, nm = key.split("::", 1)
            self.unadvertise(st, nm)
        self._zc = None
        _release_zc()

    def __del__(self):
        try:
            self.close()
        except Exception:  # pragma: no cover - defensive
            pass
