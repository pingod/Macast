# Copyright (c) 2021 by xfangfang. All Rights Reserved.
#
# Let Macast run several casting protocols at the same time.
#
# Macast was built around "Service(renderer, protocol)": one protocol owns the
# discovery channel, the CherryPy tree root and the bus subscription, and the
# menu lets the user swap that single protocol out. Real receivers (a TV, an
# Apple TV) advertise DLNA, Chromecast and AirPlay simultaneously, because each
# sender speaks only one of them.
#
# Rather than rewriting every place that assumes a single protocol, this module
# provides `ProtocolGroup`: an object that *is* a Protocol to everything around
# it, and fans operations out to the protocols inside it. Existing code keeps
# working unchanged:
#
#   * start()/stop()/reload()   -> every child
#   * uses_ssdp                 -> true if any child wants SSDP
#   * handler                   -> the richest handler (see `primary`)
#   * set_state_*               -> every child that implements it
#   * anything else             -> delegated to `primary`
#
# The round trip matters: Renderer.set_state_* resolves its protocol through
# the bus, so without fan-out only whichever protocol the bus happened to
# return would ever learn about playback progress.
#

import logging

from .protocol import Protocol, Handler

logger = logging.getLogger("ProtocolGroup")


class ProtocolGroup(Protocol):
    """A composite Protocol that runs several protocols concurrently.

    Children are kept in insertion order as ``(title, protocol)`` pairs so the
    group stays deterministic; ``title`` is the value persisted in the settings
    file and shown in the menu.
    """

    def __init__(self, protocols=None):
        super(ProtocolGroup, self).__init__()
        self._children = []
        for item in (protocols or []):
            title, protocol = item
            self.add(title, protocol)

    # -- membership ---------------------------------------------------------

    def add(self, title, protocol):
        """Add a protocol. Adding the same title twice replaces it."""
        self.remove(title)
        self._children.append((title, protocol))
        self._install_state_methods()

    def remove(self, title):
        """Remove a protocol by title, returning it (or None)."""
        for index, (existing, protocol) in enumerate(self._children):
            if existing == title:
                del self._children[index]
                self._install_state_methods()
                return protocol
        return None

    def _install_state_methods(self):
        """Shadow the base class' ``set_state_*`` no-ops with fan-out methods.

        This is the subtle bit. ``Protocol`` defines ``set_state_play`` and
        friends as *no-op methods*, so ``group.set_state_play`` resolves on the
        class alone and ``__getattr__`` — normally responsible for fanning out —
        is never consulted. The group would silently swallow every playback
        update instead of relaying it to the children. Installing the fan-out
        functions as instance attributes puts them ahead of the class in
        attribute lookup, which fixes that.
        """
        for name in [n for n in self.__dict__ if n.startswith("set_state_")]:
            del self.__dict__[name]
        for name in self.methods():
            if name.startswith("set_state_"):
                setattr(self, name, self._fanout(name))

    def get(self, title):
        for existing, protocol in self._children:
            if existing == title:
                return protocol
        return None

    def titles(self):
        return [title for title, _ in self._children]

    def protocols(self):
        return [protocol for _, protocol in self._children]

    def __contains__(self, title):
        return any(existing == title for existing, _ in self._children)

    def __len__(self):
        return len(self._children)

    def __iter__(self):
        return iter(self._children)

    @property
    def primary(self):
        """The child that answers non-broadcast questions.

        Preference goes to the SSDP-based protocol when one is enabled: its
        handler subclasses the base `Handler`, so it serves the UPnP endpoints
        *and* the web UI / PWA / management API. Choosing anything else would
        quietly drop `/description.xml` from the CherryPy tree.
        """
        for _, protocol in self._children:
            if getattr(protocol, "uses_ssdp", True):
                return protocol
        return self._children[0][1] if self._children else None

    # -- Protocol interface -------------------------------------------------

    @property
    def uses_ssdp(self):
        """SSDP is needed if any running child needs it."""
        return any(getattr(p, "uses_ssdp", True) for _, p in self._children)

    @property
    def handler(self):
        primary = self.primary
        if primary is not None:
            return primary.handler
        # No protocol enabled: still serve the web UI rather than nothing.
        return Handler()

    def start(self):
        for title, protocol in list(self._children):
            try:
                protocol.start()
            except Exception as e:
                # One protocol failing must not take the others down — losing
                # Chromecast should never cost you AirPlay.
                logger.error("Protocol %s failed to start: %s", title, e)

    def stop(self):
        for title, protocol in reversed(list(self._children)):
            try:
                protocol.stop()
            except Exception as e:
                logger.error("Protocol %s failed to stop: %s", title, e)

    def reload(self):
        self.stop()
        self.start()

    def methods(self):
        """Union of the children's ``set_state_*`` methods."""
        names = []
        for _, protocol in self._children:
            for name in protocol.methods():
                if name not in names:
                    names.append(name)
        return names

    # -- fan-out / delegation ----------------------------------------------

    def _fanout(self, name):
        """Call `name` on every child that implements it.

        Returns the first child's result, mirroring what a single protocol
        would have returned — callers such as ``Renderer.get_state()`` expect
        one value, not a list.
        """

        def call(*args, **kwargs):
            results = []
            for title, protocol in list(self._children):
                method = getattr(protocol, name, None)
                if not callable(method):
                    continue
                try:
                    results.append(method(*args, **kwargs))
                except Exception as e:
                    logger.error("Protocol %s raised in %s: %s", title, name, e)
            return results[0] if results else None

        return call

    def __getattr__(self, name):
        # Guard against recursion: a missing dunder/private attribute (e.g.
        # `_children` before __init__ finishes) must fail loudly, not loop.
        if name.startswith("_"):
            raise AttributeError(name)
        if name.startswith("set_state_"):
            return self._fanout(name)
        primary = self.primary
        if primary is None:
            raise AttributeError(
                "'{}' object has no attribute '{}' (and no protocol is "
                "enabled to delegate to)".format(type(self).__name__, name))
        return getattr(primary, name)
