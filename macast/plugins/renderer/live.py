# Copied from xfangfang/Macast-plugins (GPLv3). See docs/Provenance.md.
# mpv-based live plugin for macast
#
# Macast Metadata
# <macast.title>Live Renderer</macast.title>
# <macast.renderer>LiveRenderer</macast.renderer>
# <macast.platform>win32,darwin,linux</macast.platform>
# <macast.version>0.3</macast.version>
# <macast.host_version>0.7</macast.host_version>
# <macast.author>dushan555</macast.author>
# <macast.desc>Live support for Macast,It developed based on MPVRenderer,Can be used to watch live TV.</macast.desc>
#
# Upstream note: this module used to fetch its channel list over the network
# and write it into the config directory *at import time*, with no timeout and
# no error handling. Since Macast now imports bundled plugins while building
# its plugin list, that meant one unreachable host (or a read-only config dir)
# took the whole app down at startup. The fetch is now lazy, bounded and
# failure-tolerant: no channels simply means an empty submenu.
import gettext
import os
import threading
import cherrypy
import logging
import requests
from macast import Setting, MenuItem, gui
from macast_renderer.mpv import MPVRenderer, MPVRendererSetting
from macast.utils import SETTING_DIR, RENDERER_DIR

# Translation function. The module-level menu items are built before any
# renderer instance exists, so this has to be a module global rather than an
# attribute: leaving it undefined made every `_('...')` below a NameError.
_ = gettext.gettext

logger = logging.getLogger("LiveRenderer")
logger.setLevel(logging.INFO)

#: Where the upstream channel list lives, and how long we are willing to wait
#: for it. Kept short: this runs on the UI thread path that builds the menu.
LIVE_LIST_URL = 'http://notag.cn/live/macast_live.m3u8'
LIVE_LIST_TIMEOUT = 5

RENDERER_PATH = os.path.join(SETTING_DIR, RENDERER_DIR)
LIVE_PATH = os.path.join(RENDERER_PATH, 'macast_live.m3u8')

_cached_live_list = None
#: In-flight background fetch, so repeated menu rebuilds do not stack requests.
_refresh_thread = None


def _read_local_list():
    if not os.path.exists(LIVE_PATH):
        return None
    try:
        with open(LIVE_PATH, 'r', encoding='UTF-8') as f:
            logger.info('live list loaded from %s', f.name)
            return f.read()
    except OSError as e:
        logger.error('cannot read live list %s: %s', LIVE_PATH, e)
        return None


def _download_list():
    """Fetch the channel list, tolerating every failure.

    Returns True when a list was written. Never raises: a plugin that cannot
    reach its channel list must still be a plugin that loads.
    """
    try:
        os.makedirs(RENDERER_PATH, exist_ok=True)
        response = requests.get(LIVE_LIST_URL, timeout=LIVE_LIST_TIMEOUT)
        response.raise_for_status()
        with open(LIVE_PATH, 'wb') as f:
            f.write(response.content)
        logger.info('live list downloaded to %s', LIVE_PATH)
        return True
    except Exception as e:
        logger.warning('live list unavailable (%s); the Live renderer will '
                       'come up with an empty channel list', e)
        return False


def _refresh_in_background():
    """Download the channel list off the UI thread.

    `_download_list` blocks for up to LIVE_LIST_TIMEOUT. The menu is built on
    the UI thread (`Macast._rebuild_menu` -> `RendererSetting.build_menu`), so
    calling it from there would freeze the menu bar for the duration of a
    request to a host we do not control — and this list is only ever a
    convenience, never something playback depends on.
    """
    global _refresh_thread
    if _refresh_thread is not None and _refresh_thread.is_alive():
        return False
    _refresh_thread = threading.Thread(target=_download_list, daemon=True,
                                       name="LIVE_LIST_FETCH")
    _refresh_thread.start()
    return True


def get_live_list(force_download=False):
    """The channel list, as a list of raw m3u8 chunks (never None).

    Reads only the *local* copy; a missing file launches a background fetch and
    answers "no channels" for now. Nothing here may block: this runs while the
    menu is being assembled.
    """
    global _cached_live_list
    if force_download:
        # The explicit "refresh" menu action: fetching here is fine because it
        # is a direct response to a click, and the result is what the caller
        # is about to display.
        if _download_list():
            _cached_live_list = None
    if _cached_live_list is not None:
        return _cached_live_list
    text = _read_local_list()
    if text is None and not force_download:
        _refresh_in_background()
    _cached_live_list = [] if not text else text.split('#EXTINF:-1 ,')
    return _cached_live_list


def get_base_path(path="."):
    """Kept for backwards compatibility; the app already has this helper.

    The local copy assumed the frozen layout puts data next to `_MEIPASS`,
    which is not where Macast keeps its bundle resources. Delegating means the
    live list is looked for in the same place as everything else.
    """
    return Setting.get_base_path(path)


def get_lang():
    """Resolve the translation for this plugin and return it.

    Returns the `gettext` callable to use; it used to install the translation
    into `builtins` and return nothing, which left the module-level `_` unset
    on the failure path.
    """
    locale = Setting.get_locale()
    i18n_path = get_base_path('i18n')
    if not os.path.exists(os.path.join(i18n_path, locale, 'LC_MESSAGES', 'macast.mo')):
        locale = locale.split("_")[0]
    logger.info("Live Loading Language: {}".format(locale))
    try:
        return gettext.translation('macast', localedir=i18n_path,
                                   languages=[locale]).gettext
    except Exception:
        logger.info("Live Loading Default Language en_US")
        return gettext.gettext


class LiveRenderer(MPVRenderer):
    def __init__(self, path=None):
        global _
        _ = get_lang()
        super(LiveRenderer, self).__init__(lang=_, path=path or Setting.mpv_default_path)
        self.renderer_setting = LiveRendererSetting()


class LiveRendererSetting(MPVRendererSetting):
    def __init__(self):
        super(LiveRendererSetting, self).__init__()
        self.liveItem = MenuItem(_('LiveList'), children=[])
        self.lastItem = None
        self.reload_channels()

    def reload_channels(self):
        """(Re)build the channel submenu from whatever list we have locally.

        Deliberately never downloads: this runs on the UI thread while the menu
        is being assembled, so a dead channel host would freeze the menu for as
        long as the request took. `update_channels` is the explicit, off-thread
        way to fetch a fresh list.
        """
        items = []
        for live_item in get_live_list():
            lines = live_item.split('\n')
            if live_item.startswith('#EXTM3U'):
                items.append(MenuItem(_('PlayList'), self.on_start_click,
                                      data=LIVE_PATH))
            elif len(lines) >= 2 and lines[1].strip():
                # A malformed chunk used to raise IndexError while building the
                # menu, which took the whole settings menu down with it.
                items.append(MenuItem(lines[0], self.on_start_click,
                                      data=lines[1]))
            else:
                logger.warning('skipping malformed live entry: %r', live_item[:60])
        if not items:
            items.append(MenuItem(_('No channels (refresh to fetch)'),
                                  enabled=False))
            items.append(MenuItem(_('Refresh channel list'), self.on_update_click))
        self.liveItem.children = items
        return items

    def on_update_click(self, item):
        if get_live_list(force_download=True):
            self.reload_channels()
            cherrypy.engine.publish('app_notify', 'Macast',
                                    'Live channel list updated')
        else:
            cherrypy.engine.publish('app_notify', 'Macast',
                                    'Could not fetch the live channel list')

    def build_menu(self):
        live_menu = [MenuItem('live v0.3', enabled=False), self.liveItem]
        for menu_item in super(LiveRendererSetting, self).build_menu():
            live_menu.append(menu_item)
        return live_menu

    @property
    def renderer(self):
        renderers = cherrypy.engine.publish('get_renderer')
        if len(renderers) == 0:
            logger.error("Unable to find an available renderer.")
            return None
        return renderers.pop()

    @property
    def protocol(self):
        protocols = cherrypy.engine.publish('get_protocol')
        if len(protocols) > 0:
            return protocols.pop()

    def on_start_click(self, item):
        if self.lastItem and self.lastItem.data == item.data:
            return
        logger.info('live: text=%s data=%s', item.text, item.data)
        renderer = self.renderer
        if renderer is None:
            cherrypy.engine.publish('app_notify', 'Macast',
                                    'No player available')
            return
        renderer.set_media_title(item.text)
        renderer.set_media_url(item.data)
        protocol = self.protocol
        if protocol is not None:
            # publish() can legitimately come back empty (the protocol is
            # being swapped while the user clicks); the state push is a
            # nicety, so a missing protocol must not abort the playback.
            protocol.set_state_url(item.data)

        if self.lastItem:
            self.lastItem.checked = False
        item.checked = True
        self.lastItem = item


if __name__ == '__main__':
    gui(LiveRenderer(path='bin/mpv.exe'))
