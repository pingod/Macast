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

from .utils import Setting

logger = logging.getLogger("Discovery")

# A DNS-SD instance name is a single DNS label: letters, digits and hyphens
# only, at most 63 bytes. Macast's *friendly name* predates mDNS and is built
# for DLNA, where it is free text — the default is
# ``Macast(Pavia-MacBookPro-1754.local)``, parentheses and dots included.
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


def _local_addresses():
    """Return the list of local IPv4 addresses as 4-byte packed strings."""
    addrs = []
    try:
        for ip, _netmask in Setting.get_ip():
            try:
                addrs.append(socket.inet_aton(ip))
            except OSError:
                continue
    except Exception as e:  # pragma: no cover - defensive
        logger.warning("Failed to enumerate local addresses: %s", e)
    if not addrs:
        # Fallback: loopback so registration at least succeeds locally.
        addrs = [socket.inet_aton("127.0.0.1")]
    return addrs


class MDNSAdvertiser:
    """Thin, restart-safe wrapper around the shared Zeroconf instance."""

    def __init__(self):
        self._zc = None
        self._registered = {}  # key "type::name" -> ServiceInfo

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
        if server:
            server = _normalize_server(server)
        else:
            # macOS already reports a hostname ending in ".local"; appending a
            # second one yields "My-Mac.local.local." in the SRV record.
            server = _normalize_server(socket.gethostname())
        safe_name = sanitize_instance_name(name, service_type)
        if safe_name != name:
            logger.info("mDNS name %r normalised to %r for DNS-SD", name, safe_name)
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
            return True
        except Exception as e:
            logger.error("mDNS advertise failed for %s: %s", safe_name, e)
            return False

    def unadvertise(self, service_type, name):
        key = "{}::{}".format(service_type, name)
        info = self._registered.pop(key, None)
        if info is not None and self._zc is not None:
            try:
                self._zc.unregister_service(info)
            except Exception as e:  # pragma: no cover - defensive
                logger.warning("mDNS unregister failed: %s", e)

    def close(self):
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
