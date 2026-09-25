# Copyright (c) 2021 by xfangfang. All Rights Reserved.
# Copyright (c) 2026 by pingod. All Rights Reserved.
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

#: Read-only state accessors declared as no-ops on ``Protocol`` and implemented
#: for real on ``DLNAProtocol``. The group has to re-route all of them, or the
#: base-class stubs win and every reader sees a lie (``'STOPPED'``, ``''``, 80).
_STATE_GETTERS = (
    "get_state",
    "get_state_title",
    "get_state_url",
    "get_state_position",
    "get_state_duration",
    "get_state_volume",
    "get_state_mute",
    "get_state_transport_state",
    "get_state_transport_status",
    "get_state_speed",
    "get_state_display_subtitle",
)


def _is_meaningful(value):
    """Whether a state getter's answer carries information.

    ``''`` and ``None`` mean "this child knows nothing", so the search carries
    on to the next one. ``False`` and ``0`` are real answers and stop it —
    testing truthiness instead would read a muted/zero-volume player as
    unknown and hand back whatever a later child defaults to.
    """
    return value is not None and value != ""


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
        """Shadow the base class' state no-ops with fan-out / delegate methods.

        This is the subtle bit. ``Protocol`` defines ``set_state_play`` and
        friends as *no-op methods*, so ``group.set_state_play`` resolves on the
        class alone and ``__getattr__`` — normally responsible for fanning out —
        is never consulted. The group would silently swallow every playback
        update instead of relaying it to the children. Installing the fan-out
        functions as instance attributes puts them ahead of the class in
        attribute lookup, which fixes that.

        Writes fan out to every child; reads come back from the child that
        actually holds state. Doing this for ``set_state``/``get_state`` and
        the ``get_state_*`` accessors too is not optional: ``DLNAProtocol`` is
        the only child that keeps a playback ledger, and it is the ledger the
        status page, the subtitle toggle and the Chromecast state machine all
        read. Leaving them to the inherited base-class no-ops made
        ``/api?query=status`` report an empty track, no volume and 0:00:00
        forever.
        """
        for name in [n for n in self.__dict__ if n.startswith(("set_state", "get_state"))]:
            del self.__dict__[name]
        # `methods()` only enumerates `set_state_<something>`; the generic
        # `set_state(name, value)` write is a base-class no-op of the same
        # family (`Renderer.set_state` is how the player reports a track or
        # volume update), so it has to be routed explicitly.
        for name in self.methods() + ["set_state"]:
            setattr(self, name, self._fanout(name))
        for name in _STATE_GETTERS:
            setattr(self, name, self._delegate_getter(name))

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
        """The child whose handler becomes the CherryPy tree root.

        Two requirements pull in the same direction:

        * It must be a handler that serves the UPnP endpoints *and* the web UI
          / PWA / management API. Every SSDP-based protocol's handler
          subclasses the base `Handler` and does exactly that, so preference
          goes to an SSDP-based protocol.
        * Where several qualify, the *most capable* one has to win. A protocol
          may extend another's handler to add its own routes -- the NVA
          protocol subclasses `DLNAHandler` and mounts SETUP/RESTORE endpoints
          plus an extra SCPD -- and only the most-derived handler offers the
          union. Picking by insertion order (as this did) meant that whenever
          NVA ran alongside DLNA, DLNA's handler was mounted and every NVA
          route became unreachable, with no error anywhere to say so.

        `handler_priority` is the explicit tie-break; deriving it from class
        depth would work too, but it would silently change which handler wins
        as soon as someone introduces an unrelated subclass.
        """
        candidates = [p for _, p in self._children
                      if getattr(p, "uses_ssdp", True)] or [p for _, p in self._children]
        if not candidates:
            return None
        return max(candidates,
                   key=lambda p: getattr(p, "handler_priority", 0))

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

    def _delegate_getter(self, name):
        """Read `name` from the children, first meaningful answer wins.

        ``primary`` is the natural owner (its handler serves the UI and, with
        DLNA enabled, it is the one holding the ledger), so it is asked first.
        Falling back to the rest keeps the other arrangement honest: when DLNA
        is switched off there is no ledger at all and the sender-facing
        protocols read this value to decide what to report, so an answer of
        ``'STOPPED'`` from a base-class stub would be worse than useless.
        """

        def call(*args, **kwargs):
            ordered = [self.primary] + [p for _, p in self._children
                                        if p is not self.primary]
            for protocol in ordered:
                if protocol is None:
                    continue
                method = getattr(protocol, name, None)
                # `name` is an instance attribute on this very group; asking a
                # child that happens to be a group again would recurse.
                if not callable(method) or method is getattr(self, name, None):
                    continue
                try:
                    value = method(*args, **kwargs)
                except KeyError:
                    # The child's ledger does not track this name at all. That
                    # is normal, not an error: the settings page asks for a
                    # superset of the state names any one protocol defines
                    # (`CurrentURI` is an *action argument* in AVTransport.xml,
                    # never a state variable). Logging it at ERROR produced one
                    # line per missing name per 3-second status poll, which is
                    # exactly the kind of noise that hides a real failure.
                    logger.debug("%s does not track %s", type(protocol).__name__,
                                 args[0] if args else name)
                    continue
                except Exception as e:
                    logger.error("Protocol %s raised in %s: %s",
                                 type(protocol).__name__, name, e)
                    continue
                if _is_meaningful(value):
                    return value
            return ''

        return call

    @property
    def event_subscribes(self):
        """Every DLNA event subscriber, from every child that has any.

        ``DLNAProtocol`` owns this dict (it is not on the handler), so
        ``getattr(protocol, 'event_subscribes')`` used to resolve to whatever
        the *primary* child had — and answered ``{}`` outright whenever the
        primary was not the DLNA protocol. The status page renders this as the
        "client information" table, so that also came back empty.
        """
        merged = {}
        for title, protocol in list(self._children):
            # One read of the attribute per child, then iterate that: the
            # owning protocol's event thread *replaces* its subscriber dict
            # rather than resizing it (see the note in `DLNAProtocol.__init__`),
            # so the dict behind this reference cannot change under the loop.
            # This property is called from CherryPy workers on every status
            # poll, and it used to raise `dictionary changed size during
            # iteration` there.
            subscribers = getattr(protocol, "event_subscribes", None)
            if not isinstance(subscribers, dict):
                continue
            for sid, client in subscribers.items():
                # A SID is globally unique, but a child handing out a clashing
                # one must not silently overwrite another child's subscriber.
                if sid in merged and merged[sid] is not client:
                    sid = "{}::{}".format(title, sid)
                merged[sid] = client
        return merged

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
