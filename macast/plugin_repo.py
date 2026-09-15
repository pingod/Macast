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
"""
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


def describe():
    """The plugin-repository half of the ``plugin-info`` API response."""
    return {'repo_url': REPO_URL, 'index_urls': list(INDEX_URLS)}
