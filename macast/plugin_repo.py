# Copyright (c) 2026 by pingod. All Rights Reserved.
"""Where the settings page looks for installable plugins.

Macast historically read ``info.json`` from the upstream collection repo
(``xfangfang/Macast-plugins``). Every plugin that index publishes -- IINA,
Web, Live, PotPlayer, PIFMRDS and the NVA protocol -- now ships inside this
package (``macast/plugins/``, see ``MacastPluginManager._load_bundled_plugins``).
Reading that index again only produced a second card for each bundled plugin,
plus "update available" badges pointing back at the upstream copies.

So the index moved into this fork's own ``plugins/`` directory. It starts out
empty on purpose: the settings page fetches it, finds nothing installable and
shows only what is on this machine. Add an entry there when there is something
genuinely worth installing -- see ``plugins/README.md`` for the schema.

The coordinates live here and nowhere else; ``/api?query=plugin-info`` hands
them to the settings page, which is what makes them testable from Python.

The "启用国内镜像地址" switch (persisted as ``Github_CN_Mirror``) also lands
here: every GitHub URL the app fetches goes through :func:`mirror_url`, so one
setting rewrites the index, the install downloads and the update check.
"""
from .utils import Setting, SettingProperty

REPO = 'pingod/Macast'
BRANCH = 'main'
INDEX_PATH = 'plugins/info.json'

# Target of the "打开插件仓库" button in the settings page.
REPO_URL = 'https://github.com/{}/tree/{}/plugins'.format(REPO, BRANCH)

# Ordered: the settings page tries each in turn and stops at the first that
# answers. raw.githubusercontent.com is canonical and always fresh, but it is
# routinely unreachable from mainland China. The ghproxy-style mirror in the
# middle is fresh too (it proxies raw on demand) and is the one this fork's own
# git remote already goes through. jsDelivr comes last on purpose: it works
# nearly everywhere, but it caches branch files for hours, so it can serve a
# stale index -- fine as a last resort, wrong as the first choice.
# All three send CORS headers, which matters because this fetch runs in the
# browser, not in Python.
INDEX_URLS = (
    'https://raw.githubusercontent.com/{}/{}/{}'.format(REPO, BRANCH, INDEX_PATH),
    'https://ghproxy.net/https://raw.githubusercontent.com/{}/{}/{}'.format(
        REPO, BRANCH, INDEX_PATH),
    'https://cdn.jsdelivr.net/gh/{}@{}/{}'.format(REPO, BRANCH, INDEX_PATH),
)

# ---------------------------------------------------------------------------
# "启用国内镜像地址" (Github_CN_Mirror)
# ---------------------------------------------------------------------------

MIRROR_PREFIX = 'https://ghproxy.net/'

# The canonical GitHub hosts the app fetches at runtime. With the switch on,
# every URL on one of these is prefixed with MIRROR_PREFIX. jsDelivr's `gh`
# URLs are deliberately absent: they already *are* a GitHub mirror and are
# reachable from the mainland, so rewriting them would only add a hop.
MIRROR_HOSTS = (
    'https://github.com/',
    'https://raw.githubusercontent.com/',
    'https://api.github.com/',
)


def mirror_enabled():
    """Whether the user asked for domestic mirrors (default: off).

    Reads through `has` first because `Setting.get(key, False)` would persist
    the default on a mere read (AGENTS §4.2).
    """
    prop = SettingProperty.Github_CN_Mirror
    return bool(Setting.get(prop, False)) if Setting.has(prop) else False


def set_mirror_enabled(on):
    """Persist the switch. Off means the key is absent, not False."""
    if on:
        Setting.set(SettingProperty.Github_CN_Mirror, True)
    else:
        Setting.unset(SettingProperty.Github_CN_Mirror)


def to_mirror_url(url):
    """`url` with GitHub hosts replaced by the domestic mirror. Pure: does
    not consult the setting, and leaves non-GitHub (and already-mirrored)
    URLs untouched."""
    for host in MIRROR_HOSTS:
        if url.startswith(host):
            return MIRROR_PREFIX + url
    return url


def mirror_url(url):
    """The URL as the app should actually fetch it right now."""
    return to_mirror_url(url) if mirror_enabled() else url


def index_urls():
    """The ordered index candidates for the current mirror setting.

    In mirror mode the canonical raw URL collapses onto the ghproxy entry
    already in the list, so the de-duplicated chain stays fresh-first:
    ghproxy (proxies raw on demand), then jsDelivr as the caching last resort.
    """
    urls = INDEX_URLS if not mirror_enabled() else map(to_mirror_url, INDEX_URLS)
    out = []
    for url in urls:
        if url not in out:
            out.append(url)
    return out


def describe():
    """The plugin-repository half of the ``plugin-info`` API response."""
    return {'repo_url': mirror_url(REPO_URL), 'index_urls': index_urls(),
            'mirror_enabled': mirror_enabled()}
