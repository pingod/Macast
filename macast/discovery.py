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
import socket

from zeroconf import Zeroconf, ServiceInfo

from .utils import Setting

logger = logging.getLogger("Discovery")


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
    """Thin, restart-safe wrapper around a single Zeroconf instance."""

    def __init__(self):
        self._zc = None
        self._registered = {}  # key "type::name" -> ServiceInfo

    def _ensure_zc(self):
        if self._zc is None:
            self._zc = Zeroconf()
        return self._zc

    def advertise(self, service_type, name, port, properties, server=None):
        """Advertise a DNS-SD service.

        :param service_type: e.g. '_googlecast._tcp.local.'
        :param name:         e.g. 'Macast._googlecast._tcp.local.'
        :param port:         TCP port the service listens on
        :param properties:   dict of TXT records (values str or bytes)
        :param server:       optional hostname advertised in the SRV record
        """
        zc = self._ensure_zc()
        txt = {}
        for k, v in (properties or {}).items():
            if isinstance(v, str):
                txt[k] = v.encode("utf-8")
            else:
                txt[k] = v
        server = server or (socket.gethostname() + ".local.")
        info = ServiceInfo(
            service_type,
            name,
            addresses=_local_addresses(),
            port=port,
            properties=txt,
            server=server,
        )
        key = "{}::{}".format(service_type, name)
        try:
            zc.register_service(info)
            self._registered[key] = info
            logger.info("mDNS advertised %s on port %d", name, port)
        except Exception as e:
            logger.error("mDNS advertise failed for %s: %s", name, e)
            raise

    def unadvertise(self, service_type, name):
        key = "{}::{}".format(service_type, name)
        info = self._registered.pop(key, None)
        if info is not None and self._zc is not None:
            try:
                self._zc.unregister_service(info)
            except Exception as e:  # pragma: no cover - defensive
                logger.warning("mDNS unregister failed: %s", e)

    def close(self):
        if self._zc is not None:
            for key in list(self._registered.keys()):
                st, nm = key.split("::", 1)
                self.unadvertise(st, nm)
            try:
                self._zc.close()
            except Exception:  # pragma: no cover - defensive
                pass
            self._zc = None

    def __del__(self):
        try:
            self.close()
        except Exception:  # pragma: no cover - defensive
            pass
