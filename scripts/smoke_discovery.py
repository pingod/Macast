#!/usr/bin/env python3
# Copyright (c) 2026 by pingod. All Rights Reserved.
"""Real-network smoke test for the Chromecast / AirPlay receivers.

Where ``verify_cast_airplay.py`` drives the protocols in-process with mock
sockets, this one brings them up on the *actual* LAN: it binds the real TLS and
RTSP servers and publishes real mDNS records, so you can point a phone at the
machine, or browse from a second terminal with::

    dns-sd -B _googlecast._tcp local
    dns-sd -B _airplay._tcp    local

Run it standalone (it does not need the Macast GUI)::

    python scripts/smoke_discovery.py [--seconds 60]

It exits on Ctrl-C. Nothing outside the process is modified.
"""

import argparse
import logging
import sys
import time

logging.basicConfig(level=logging.INFO,
                    format="%(asctime)s %(levelname)-7s %(message)s")

sys.path.insert(0, ".")

from macast.protocol_airplay import AirPlayProtocol  # noqa: E402
from macast.protocol_cast import ChromecastProtocol  # noqa: E402


class _PrintRenderer:
    """Stands in for MPVRenderer: prints whatever a sender asked us to play."""

    def set_media_url(self, url, header=None, title=None):
        print("\n>>> PLAY  url={}\n".format(url))
        return True

    def set_media_title(self, title):
        print(">>> TITLE {}".format(title))

    def pause(self):
        print(">>> PAUSE")
        return True

    def resume(self):
        print(">>> RESUME")
        return True

    def stop(self):
        print(">>> STOP")
        return True

    def set_volume(self, v):
        print(">>> VOLUME {}".format(v))
        return True

    def methods(self):
        return []


_RENDERER = _PrintRenderer()


def _bound(protocol):
    """Return the port each protocol actually bound (or None)."""
    for attr in ("port", "rtsp_port", "tls_port"):
        port = getattr(protocol, attr, None)
        if isinstance(port, int):
            return attr, port
    return None, None


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--seconds", type=int, default=60,
                    help="how long to stay discoverable (default: 60)")
    args = ap.parse_args()

    protocols = []
    for cls in (ChromecastProtocol, AirPlayProtocol):
        # Protocol.renderer resolves through the CherryPy bus; there is no bus
        # here, so swap in the stand-in renderer on a throwaway subclass.
        proto = type("Standalone" + cls.__name__, (cls,), {
            "renderer": property(lambda self: _RENDERER),
        })()
        try:
            proto.start()
        except Exception as e:  # one protocol failing must not hide the other
            print("FAILED to start {}: {}: {}".format(cls.__name__, type(e).__name__, e))
            continue
        protocols.append(proto)
        attr, port = _bound(proto)
        print("STARTED {:<20} {} = {}".format(cls.__name__, attr, port))

    if not protocols:
        print("No protocol started; aborting.")
        return 1

    print("\nNow discoverable on the LAN. From another terminal:")
    print("  dns-sd -B _googlecast._tcp local")
    print("  dns-sd -B _airplay._tcp    local")
    print("\nCtrl-C to stop.\n")

    try:
        time.sleep(args.seconds)
    except KeyboardInterrupt:
        pass
    finally:
        for proto in protocols:
            try:
                proto.stop()
            except Exception as e:
                print("stop error: {}".format(e))
        print("Stopped.")
    return 0


if __name__ == "__main__":
    sys.exit(main())
