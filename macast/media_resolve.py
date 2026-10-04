# Copyright (c) 2026 by pingod. All Rights Reserved.
# Page-address resolution for Macast's 「网页地址投屏」.
#
# Why this is a core module and not a plugin: what it produces (a media URL plus
# the headers that make that URL readable) is handed to *whichever renderer is
# currently selected*, and the relay that serves it lives in the CherryPy tree the
# core owns. A single-file plugin could not mount a route, and the three senders
# that can already deliver an address (mpv locally, cast_local_file,
# screen_mirror's bridge) all take it through `set_media_url`.
#
# Two engines, merged into one candidate list, because they fail in
# complementary ways (measured 2026-10 against the castor fixtures):
#
#   yt-dlp   ~1800 sites, needs the binary, 12-19 s on a real video page, and it
#            reports the *signed* address together with the headers the origin
#            demanded. Answers `ERROR: Unsupported URL` for a small self-hosted
#            site.
#   scrape   stdlib only, microseconds, and structurally blind: it sees only what
#            the HTML states, so a page that builds its `src` in JavaScript gives
#            it nothing.
#
# A browser that observes network traffic (castor's third engine, the one that
# caught the JS-only case in 2 s where `yt-dlp -g` failed outright) is
# deliberately absent: that means shipping CDP. Until phase B exists, this
# module's coverage hole is stated in the settings page instead of hidden.
#
# Every candidate is served from the Macast side (`macast/media_relay.py`). That
# is a decision, not a limitation: an address that is good for one request gets
# handed to a device which will fetch it minutes later, from another IP, with no
# Referer -- and "the TV says it won't play" is the symptom. `relay_reason()` is
# what the page says about it.
#
# The admission and preference tables are ported from castor
# (https://github.com/stupside/castor, MIT), `internal/source/rank/`, with two
# divergences marked at their rows:
#   * no minimum runtime. castor casts *titles* and treats a candidate under five
#     minutes as a spliced-in ad; someone pasting a link at Macast may well mean a
#     20-second clip, and refusing to cast it would be refusing their request.
#   * no `blob:` handling. Without a browser there is nothing to resolve one
#     against, so a scraped one is dropped at the door rather than admitted as a
#     last resort that cannot by definition be fetched.

import functools
import json
import os
import re
import subprocess
import sys
import threading
import time
import urllib.error
import urllib.parse
import urllib.request
from dataclasses import dataclass, field
from html.parser import HTMLParser

#: A page body is read to this ceiling and no further. Video sites inline a
#: stylesheet the size of a novel; the addresses we want are in the first blocks.
PAGE_MAX_BYTES = 2 * 1024 * 1024

#: The three waits. yt-dlp is the slow one on a real site (castor's browser path
#: needed 2 s where `yt-dlp -J` needed 12-19 s), which is why a resolve runs in a
#: thread and the page polls `query=resolve-status` rather than holding a request.
SCRAPE_TIMEOUT = 15
YTDLP_TIMEOUT = 120
#: Generous for one ffprobe because a manifest probe fetches the playlist *and* a
#: segment before it will report anything.
PROBE_TIMEOUT = 25

#: Measuring costs a fetch each, so it is capped: yt-dlp returns dozens of
#: renditions of the same title and the user is not learning anything after the
#: first few.
MAX_MEASURED = 8
#: ... and no more than this many from one host, so one site's ladder cannot
#: crowd out the other engine's only candidate.
MAX_MEASURED_PER_HOST = 5

#: Containers that name a document listing segments, versus a file a reader can
#: walk with byte offsets.
SEGMENTED_SUFFIXES = ('.m3u8', '.mpd')
PROGRESSIVE_SUFFIXES = ('.mp4', '.m4v', '.mov', '.mkv', '.webm', '.flv', '.ts')
#: Sound on its own is still castable, so these are candidates too.
AUDIO_SUFFIXES = ('.mp3', '.m4a', '.aac', '.ogg', '.opus', '.wav', '.flac')
PROGRESSIVE = PROGRESSIVE_SUFFIXES + AUDIO_SUFFIXES
MEDIA_SUFFIXES = SEGMENTED_SUFFIXES + PROGRESSIVE

#: A GUI app started from Finder/Dock does not inherit the shell's PATH, so a
#: brew-installed yt-dlp or ffprobe looks missing even though the user has it.
#: The same directories `macast_ytdlp.py` walks, for the same reason.
EXTRA_BIN_DIRS = (
    '/opt/homebrew/bin',
    '/usr/local/bin',
    '/opt/local/bin',
    os.path.join(os.path.expanduser('~'), '.local', 'bin'),
)

USER_AGENT = ('Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) '
              'AppleWebKit/537.36 (KHTML, like Gecko) Chrome/124.0 Safari/537.36')

#: castor's `Reach`: how far reading the origin got, as a fact about the origin
#: rather than about the media.
REACH_UNPROVEN = 'unproven'
REACH_OPENED = 'opened'
REACH_REFUSED = 'refused'

#: ffprobe states a refusal in its own words, and only these four codes count as
#: one. Anything else (timeout, TLS, DNS, a 500, a 503) is *unproven*, which the
#: admission table keeps as a last resort -- a slow or briefly unhappy origin must
#: cost the user a worse first choice, not no choice. The phrasings ffprobe uses
#: (`http error 404 not found`, `Server returned 403 Forbidden`) always carry the
#: digits, so the codes are the whole rule; matching on the words alone would make
#: a 500 a refusal and quietly delete the row above this one's rationale.
#: `media_relay.REFUSED_STATUS` is the same set for the same reason, and Part 59
#: pins the two against each other.
_REFUSED_STATUS = re.compile(r'\b(?:40[134]|410)\b')

#: Video tracks ffprobe reports as video that are a still image published as a
#: track -- a poster, not a program. castor's list, verbatim.
STILL_IMAGE_CODECS = frozenset((
    'png', 'apng', 'mjpeg', 'jpeg', 'jpegls', 'bmp', 'gif', 'tiff', 'webp', 'ppm',
))

#: Request headers that must never be forwarded. `Host`/`Content-Length` would
#: describe *our* request, and the rest are hop-by-hop; forwarding a captured
#: `Content-Length` on a GET is a malformed request. Everything else -- including
#: `Cookie` and `Authorization` -- is kept, because that is what makes a protected
#: page resolve at all, and the relay sends them to exactly one origin.
_UNFORWARDABLE_HEADERS = frozenset((
    'host', 'content-length', 'content-type', 'connection', 'proxy-connection',
    'upgrade', 'te', 'trailer', 'transfer-encoding', 'accept-encoding',
))


def find_command(name, platform=None, extra_dirs=None, env_path=None):
    """Path of an external command, or None when this machine has nothing usable.

    All three parameters are seams: the suite runs on macOS and needs to ask "what
    would a Windows machine find" without being one, which is §4.8's Part 40
    lesson (a sentence that follows the machine *running* the code instead of the
    one being described).

    Looked up per use, not at import: installing yt-dlp while Macast runs should
    take effect without a restart.
    """
    search = env_path if env_path is not None else os.environ.get('PATH', '')
    names = (name, name + '.exe') if (platform or sys.platform) == 'win32' else (name,)
    for entry in filter(None, search.split(os.pathsep)):
        for filename in names:
            candidate = os.path.join(entry, filename)
            if os.path.isfile(candidate) and os.access(candidate, os.X_OK):
                return candidate
    for entry in (extra_dirs or EXTRA_BIN_DIRS):
        for filename in names:
            candidate = os.path.join(entry, filename)
            if os.path.isfile(candidate) and os.access(candidate, os.X_OK):
                return candidate
    return None


def console_flags(platform=None):
    """`creationflags` so a probe never flashes a terminal on Windows.

    Every helper here is a console program and the packaged .exe is built
    `--noconsole`; `macast/utils.py` states the whole argument. Not called through
    `utils.hidden_flags` on purpose: that one reads `subprocess.Popen`'s patched
    global, and this module spawns with `subprocess.run`.
    """
    if (platform or sys.platform) != 'win32':
        return 0
    return getattr(subprocess, 'CREATE_NO_WINDOW', 0)


@dataclass
class Probe:
    """What one ffprobe run established about one address."""
    reach: str = REACH_UNPROVEN
    container: str = ''
    video_codec: str = ''
    audio_codec: str = ''
    width: int = 0
    height: int = 0
    duration: float = 0.0
    bitrate: float = 0.0          # bit/s; 0 when nothing reported it
    is_live: bool = False
    error: str = ''

    @property
    def measured(self):
        """Whether a reader got far enough to describe the media."""
        return self.reach == REACH_OPENED

    @property
    def moving_picture(self):
        return bool(self.video_codec) and self.video_codec not in STILL_IMAGE_CODECS


@dataclass
class Candidate:
    """One address that might be the video on the page."""
    url: str
    origin: str                       # 'ytdlp' | 'page'
    headers: dict = field(default_factory=dict)
    label: str = ''                   # yt-dlp's format note, or the HTML attribute
    title: str = ''                   # what to call this on the television
    probe: Probe = None
    last_resort: bool = False
    reason: str = ''
    # DASH hands the picture and the sound as two addresses. `audio_url` is the
    # partner this row was *paired* with at parse time, so the pairing decision
    # lives with the engine output and nowhere else.
    audio_url: str = ''
    audio_headers: dict = field(default_factory=dict)
    audio_label: str = ''

    @property
    def host(self):
        return urllib.parse.urlsplit(self.url).netloc.lower()

    @property
    def path_suffix(self):
        """The container the *address* advertises, ignoring the query string."""
        path = urllib.parse.urlsplit(self.url).path.lower()
        for suffix in MEDIA_SUFFIXES:
            if path.endswith(suffix):
                return suffix
        return ''

    @property
    def segmented(self):
        return self.path_suffix in SEGMENTED_SUFFIXES

    @property
    def audio_only(self):
        return self.path_suffix in AUDIO_SUFFIXES

    @property
    def silent(self):
        """A measured picture with no sound and no partner address to merge in.

        Only a *measured* row may answer this. An address ffprobe never got to is
        unknown, not silent, and hiding it would turn「这台机器没读到」into「这站
        没有声音」-- the one distinction this module exists to keep.
        """
        probe = self.probe
        return bool(probe and probe.measured and probe.moving_picture
                    and not probe.audio_codec and not self.audio_url)

    @property
    def height(self):
        return self.probe.height if self.probe else 0

    @property
    def width(self):
        return self.probe.width if self.probe else 0

    @property
    def duration(self):
        return self.probe.duration if self.probe else 0.0


# ---------------------------------------------------------------------------
# engine 1 -- scrape the page with the standard library
# ---------------------------------------------------------------------------

class _PageParser(HTMLParser):
    """Collect every media address the HTML states outright.

    `data-src` and its cousins are in here because lazy-load is the normal shape
    of a self-hosted video page: the `src` is a placeholder and the real file sits
    in an attribute the script copies across.
    """
    SRC_ATTRS = ('src', 'data-src', 'data-orig-file', 'data-video-src',
                 'data-hls-url', 'data-mp4', 'data-file')

    def __init__(self):
        HTMLParser.__init__(self, convert_charrefs=True)
        self.found = []
        self.page_title = ''
        self._in_title = False

    def handle_starttag(self, tag, attrs):
        if tag == 'title':
            self._in_title = True
            return
        values = {}
        for key, value in attrs:
            if value:
                values.setdefault(key.lower(), value)
        if tag in ('video', 'audio'):
            self._take(values, tag)
        elif tag == 'source':
            self._take(values, 'source')
        elif tag == 'embed':
            self._take(values, 'embed')
        elif tag == 'link':
            rel = ' '.join((values.get('rel') or '').lower().split())
            if 'preload' in rel and values.get('as') in ('video', 'fetch'):
                self._take({'href': values.get('href')}, 'preload')

    def handle_endtag(self, tag):
        if tag == 'title':
            self._in_title = False

    def handle_data(self, data):
        # The first non-empty <title> text is the page's; a site that animates the
        # tab title with a script would only be editing the same node.
        if self._in_title and not self.page_title and data.strip():
            self.page_title = data.strip()[:120]

    def _take(self, values, where):
        for name in self.SRC_ATTRS + ('href',):
            href = values.get(name)
            if href:
                self.found.append((href, '{}:{}'.format(where, name)))


#: og:video in its three spellings; secure_url first so a page offering both
#: gives the https one a stable place in the list.
_OG_VIDEO_NAMES = ('og:video:secure_url', 'og:video:url', 'og:video')
_META_ATTR = re.compile(r'([\w:-]+)\s*=\s*["\']([^"\']*)["\']')


def _meta_og_video(body):
    """`og:video` / `og:video:secure_url` / `og:video:url`.

    Read with a regex rather than the parser: attribute order is not stable
    across generators, `<meta>` may or may not be self-closed, and the tag can
    appear before `<body>` where a streaming parse would still find it but a
    one-shot read of the head is all we need.
    """
    found = []
    for tag in re.findall(r'<meta\b[^>]*>', body, flags=re.IGNORECASE):
        attrs = {}
        for key, value in _META_ATTR.findall(tag):
            attrs.setdefault(key.lower(), value)
        name = (attrs.get('property') or attrs.get('name') or '').lower()
        if name in _OG_VIDEO_NAMES and attrs.get('content'):
            found.append((attrs['content'], name))
    return found


#: An address with a media suffix anywhere in the text, quoted or not. This is the
#: half that catches a player configured by an inline script
#: (`file: "https://cdn.example/a.mp4"`); it is *still* not JavaScript that builds
#: the address, which is the documented hole.
_LITERAL_URL = re.compile(
    r'''((?:https?://|//|/)[^\s"'<>\\]*?\.(?:'''
    + '|'.join(s[1:] for s in MEDIA_SUFFIXES)
    + r''')(?:\?[^\s"'<>\\]*)?)''',
    re.IGNORECASE)


def _literal_urls(body):
    return [(match.group(1), 'literal') for match in _LITERAL_URL.finditer(body)]


def scrape_page(page_url, body=None, opener=None):
    """Candidates stated outright by a page, as the browser would see them.

    `opener` is a seam for the tests: one callable taking a URL and returning the
    body text. Nothing here authenticates -- a page that needs a cookie needs
    yt-dlp's, and `ytdlp_candidates` runs alongside this.
    """
    text = body if body is not None else (
        _fetch_page(page_url, opener) if page_url else '')
    if not text:
        return []
    raw = []
    parser = _PageParser()
    try:
        parser.feed(text)
        raw.extend(parser.found)
    except Exception:
        # A malformed page is not a reason to lose the og:video we could read.
        pass
    raw.extend(_meta_og_video(text))
    raw.extend(_literal_urls(text))

    found = []
    seen = set()
    stem = page_url.split('#')[0] if page_url else ''
    for href, where in raw:
        url = absolute_url(stem, href)
        if url is None or url.split('#')[0] == stem:
            continue
        if url in seen:
            continue
        seen.add(url)
        found.append(Candidate(url=url, origin='page', label=where,
                               title=parser.page_title))
    return found


def _fetch_page(page_url, opener=None):
    if opener is not None:
        return opener(page_url) or ''
    request = urllib.request.Request(
        page_url, headers={'User-Agent': USER_AGENT,
                           'Accept': 'text/html,application/xhtml+xml,*/*;q=0.8'})
    try:
        with urllib.request.urlopen(request, timeout=SCRAPE_TIMEOUT) as response:
            raw = response.read(PAGE_MAX_BYTES)
            charset = response.headers.get_content_charset() or ''
    except (urllib.error.URLError, OSError, ValueError):
        return ''
    return decode_body(raw, charset)


def decode_body(raw, charset=''):
    """Bytes to text, with the two fallbacks that actually happen on video pages."""
    if isinstance(raw, str):
        return raw
    for name in (charset, 'utf-8', 'gbk'):
        if not name:
            continue
        try:
            return raw.decode(name)
        except (UnicodeDecodeError, LookupError):
            continue
    return raw.decode('utf-8', 'replace')


def absolute_url(page_url, href):
    """An http(s) URL, or None for anything a reader cannot fetch.

    `blob:`/`data:`/`javascript:` are dropped here rather than at the admission
    table: without a browser there is nothing to resolve a `blob:` against, and
    admitting one as a "last resort" would offer the user an address that is
    un-fetchable by definition.
    """
    if not href:
        return None
    href = href.strip()
    if not page_url and href.startswith('/'):
        return None
    if href.startswith('//') and page_url:
        href = urllib.parse.urlsplit(page_url).scheme + ':' + href
    try:
        url = urllib.parse.urljoin(page_url, href)
    except ValueError:
        return None
    if urllib.parse.urlsplit(url).scheme.lower() not in ('http', 'https'):
        return None
    return url


# ---------------------------------------------------------------------------
# engine 2 -- yt-dlp
# ---------------------------------------------------------------------------

def ytdlp_candidates(page_url, binary=None, cookies=None, sink=None):
    """Every rendition yt-dlp can name for this page, with its headers.

    `-J` and not `-g`: `-g` prints one address and throws away the `http_headers`
    that make that address readable, and those headers are the reason the relay
    exists. The JSON also carries the whole ladder, so `rank` has something to
    rank.

    `sink` is where the engine's *own* sentence goes when it refuses. Before it, a
    refusal and an absent binary looked identical from the outside: the return
    code was checked, stderr was thrown away, and the card said「没有地址」while the
    engine had actually said「需要 fresh cookies」. That is a different answer and
    the user has to hear the one that was given.
    """
    binary = binary or find_command('yt-dlp')
    if binary is None:
        return []
    command = [binary, '-J', '--no-playlist', '--no-warnings', '--quiet',
               '--socket-timeout', '20']
    command += cookie_arguments(cookies)
    command += [page_url]
    try:
        completed = subprocess.run(command, stdout=subprocess.PIPE,
                                   stderr=subprocess.PIPE, timeout=YTDLP_TIMEOUT,
                                   creationflags=console_flags(), text=True,
                                   encoding='utf-8', errors='replace')
    except (OSError, subprocess.SubprocessError) as exc:
        _sink(sink, 'yt-dlp 没能运行（{}: {}）'.format(type(exc).__name__, exc))
        return []
    if completed.returncode != 0 or not (completed.stdout or '').strip():
        _sink(sink, engine_note(completed))
        return []
    return parse_ytdlp_json(completed.stdout)


def cookie_arguments(cookies):
    """yt-dlp's cookie switches from the caller's dict; never a cookie value.

    `{'file': path, 'browser': name}`, either half optional. The file wins when
    both are set, because a jar this app wrote is the one that accumulates the
    cookies the engine mints (yt-dlp saves the jar back), while a browser profile
    is somebody else's store we only borrow.
    """
    cookies = cookies or {}
    path = (cookies.get('file') or '').strip()
    if path:
        return ['--cookies', path]
    browser = (cookies.get('browser') or '').strip().lower()
    if browser:
        return ['--cookies-from-browser', browser]
    return []


def engine_note(completed, limit=200):
    """The first line the engine actually complained about, in its own words.

    `--quiet` silences progress, not `ERROR:` -- which is the whole reason this is
    worth reading. Truncated because a real stderr also carries tracebacks, and a
    traceback on a settings page is not an explanation.
    """
    text = (completed.stderr or '').strip()
    for line in text.splitlines():
        line = line.strip()
        if line:
            return line[:limit]
    if completed.returncode:
        return 'yt-dlp 退出码 {}'.format(completed.returncode)
    return 'yt-dlp 没有返回任何地址'


def _sink(sink, note):
    if sink is not None and note:
        sink.append(note)


#: What an engine says when the page will not hand anything over without a
#: session. Every extractor phrases it differently -- yt-dlp's bilibili module
#: writes 「cookies to access this webpage」, its tiktok module writes 「Fresh
#: cookies (not necessarily logged in) are needed」 -- but they all say one of
#: these words, and a return code never distinguishes them from "this site has
#: no video". Matched here rather than in the page for the same reason every
#: other reading on that card is computed here: it is our guess at somebody
#: else's sentence, so it must have one owner that the tests can pin.
_COOKIE_WORDS = ('cookie', 'log in', 'login', 'logged in', 'sign in',
                 'authentication', 'unauthorized', '需要登录', '授权')


def needs_cookies(note):
    """Whether an engine's own refusal sentence is asking for a session."""
    text = (note or '').lower()
    return any(word in text for word in _COOKIE_WORDS)


# ---------------------------------------------------------------------------
# a pasted cookie jar
# ---------------------------------------------------------------------------

#: A jar is a list of `(domain, name, value)` lines in a fixed shape. The cap is
#: not politeness: this text is written into the config directory and then handed
#: to a subprocess as a file path, and a 200 MB paste is a disk problem.
COOKIE_JAR_MAX_BYTES = 256 * 1024
COOKIE_JAR_FIELDS = 7

#: An HttpOnly cookie is a *cookie* whose line starts with this, not a comment.
#: Same prefix, same ordering rule as the standard library's
#: `http.cookiejar.MozillaCookieJar` -- and the ordering is the whole bug: a jar
#: exported from a browser marks its session cookies this way, so a reader that
#: skips every `#` line reads a pile of real login cookies as "nothing here".
HTTPOONLY_PREFIX = '#HttpOnly_'


def cookie_jar_state(text):
    """`(cookies, bad_lines)` for a pasted Netscape jar -- it reads, it does not run.

    Why validate a file whose only reader is yt-dlp: an empty or malformed jar
    produces *no* error at all from the engine (it simply resolves the page
    anonymously and comes back with fewer addresses), so the user's next clue that
    the paste was junk is the card saying「页面里没有读到视频地址」. Saying
    "3 条 cookie，2 行看不懂" at paste time is the only moment anyone can act on it.
    """
    text = text or ''
    if len(text.encode('utf-8')) > COOKIE_JAR_MAX_BYTES:
        raise ValueError('cookie 文件太大了（上限 {} KB）'.format(
            COOKIE_JAR_MAX_BYTES // 1024))
    cookies = bad = 0
    for line in text.splitlines():
        if line.startswith(HTTPOONLY_PREFIX):
            line = line[len(HTTPOONLY_PREFIX):]
        # Then the same skip the engine's own reader applies, *after* the prefix is
        # gone: the prefix binds at index 0 of the raw line, so an indented
        # `  #HttpOnly_…` stays a comment, while `#HttpOnly_# …` is a comment too.
        # Counting either as a cookie would promise a login state yt-dlp never loads.
        if not line.strip() or line.lstrip().startswith('#'):
            continue
        fields = line.rstrip('\r\n').split('\t')
        if len(fields) >= COOKIE_JAR_FIELDS and fields[0].strip():
            cookies += 1
        else:
            bad += 1
    return cookies, bad


def parse_ytdlp_json(text):
    """`yt-dlp -J` output to candidates. Split out so the tests feed it a fixture."""
    try:
        info = json.loads(text)
    except ValueError:
        return []
    if not isinstance(info, dict):
        return []
    if info.get('_type') == 'playlist':
        entries = [e for e in (info.get('entries') or []) if isinstance(e, dict)]
    else:
        entries = [info]
    # A page of several videos still has one primary title, and yt-dlp puts the
    # biggest format list on it; the rest are usually trailers.
    entries.sort(key=lambda one: len(one.get('formats') or []), reverse=True)
    found = []
    for entry in entries[:3]:
        shared = normalize_headers(entry.get('http_headers'))
        entry_title = entry.get('title')
        video_rows = []
        audio_rows = []
        for form in entry.get('formats') or []:
            if not isinstance(form, dict):
                continue
            url = form.get('url')
            if not isinstance(url, str) or not url.lower().startswith(('http://', 'https://')):
                continue
            # The entry title wins. yt-dlp labels each format
            # "<video title> (<index>)", and that index is a row in its own
            # format list -- handing "第三集 · 夏日回响 (2)" to the television
            # is a claim about the page that the page never made.
            title = entry_title if isinstance(entry_title, str) and entry_title.strip() \
                else form.get('title')
            candidate = Candidate(
                url=url, origin='ytdlp',
                headers=normalize_headers(form.get('http_headers') or shared),
                label=format_label(form),
                title=title if isinstance(title, str) else '')
            kind = track_kind(form)
            if kind == 'video':
                video_rows.append((candidate, form))
            elif kind == 'audio':
                audio_rows.append((candidate, form))
            else:
                found.append(candidate)
        pair_tracks(video_rows, audio_rows)
        found.extend(candidate for candidate, _ in video_rows)
        found.extend(candidate for candidate, _ in audio_rows)
    return dedupe(found)


def track_kind(form):
    """'video' / 'audio' when the engine states the tracks outright, else ''.

    `'none'` is yt-dlp's word for "this rendition does not carry that track", and
    it is the only reason DASH's split ladders can be paired at all. Anything else
    -- a file that holds both, or a page whose formats say nothing -- returns `''`
    rather than a guess from the file suffix: a silent 1080p row read as "has
    sound" is the lie this whole feature exists to stop telling.
    """
    if not isinstance(form, dict):
        return ''
    video = form.get('vcodec')
    audio = form.get('acodec')
    if not isinstance(video, str) or not isinstance(audio, str):
        return ''
    has_video = video != 'none'
    has_audio = audio != 'none'
    if has_video and not has_audio:
        return 'video'
    if has_audio and not has_video:
        return 'audio'
    return ''


def _track_bitrate(form):
    """The engine's own best guess at this track's bit rate, or 0.0."""
    for name in ('abr', 'tbr', 'br'):
        value = form.get(name)
        try:
            if value is not None and float(value) > 0:
                return float(value)
        except (TypeError, ValueError):
            continue
    return 0.0


def pair_tracks(video_rows, audio_rows):
    """Attach the best-sounding partner to every picture-only row, in place.

    One rule decides which sound a picture gets: the highest bit rate the engine
    named, ties broken by the address so a rerun says the same thing. Every video
    row may reuse the same audio row -- merging happens once per cast, and a
    480p/1080p pair of rungs that share one sound track is not two casts.
    """
    if not audio_rows:
        return
    best = max(audio_rows, key=lambda row: (_track_bitrate(row[1]), row[0].url))
    for candidate, _form in video_rows:
        candidate.audio_url = best[0].url
        candidate.audio_headers = best[0].headers
        candidate.audio_label = best[0].label


def normalize_headers(headers):
    """Keep what a fetch must send; drop what would describe our own request."""
    kept = {}
    if not isinstance(headers, dict):
        return kept
    for name, value in headers.items():
        if not isinstance(name, str) or value is None:
            continue
        lower = name.lower()
        if lower in _UNFORWARDABLE_HEADERS:
            continue
        kept[lower] = str(value)
    return kept


def format_label(form):
    """The one-line description yt-dlp itself gives a format, assembled."""
    parts = []
    if form.get('ext'):
        parts.append(str(form['ext']))
    if form.get('height'):
        parts.append('{}p'.format(form['height']))
    if form.get('format_note'):
        parts.append(str(form['format_note']))
    if form.get('tbr'):
        parts.append('{} k'.format(int(form['tbr'])))
    return ' '.join(parts)


def page_wording(title, page_title):
    """Drop yt-dlp's entry index when the page's own words explain it.

    A television showing "第三集 · 夏日回响 (1)" reads like a defect, and the "(1)"
    is yt-dlp numbering the hits it found on the page -- not something the site
    ever wrote. Only that exact shape is touched: a title the page itself ended in
    "(2)" stays untouched, because then nothing proves the digits are not part of
    the name.
    """
    if not title:
        return page_title
    if not page_title or title == page_title:
        return title
    if title.startswith(page_title) \
            and re.fullmatch(r' \(\d{1,2}\)', title[len(page_title):]):
        return page_title
    return title


def dedupe(candidates):
    """One candidate per address; the yt-dlp one wins when both engines found it.

    The losing page candidate still contributes one thing: the `<title>` as the
    page wrote it, which is the phrase the television should show.
    """
    by_url = {}
    for candidate in candidates:
        current = by_url.get(candidate.url)
        if current is None:
            by_url[candidate.url] = candidate
            continue
        if current.origin == 'page' and candidate.origin == 'ytdlp':
            candidate.title = page_wording(candidate.title, current.title)
            by_url[candidate.url] = candidate
    return list(by_url.values())


# ---------------------------------------------------------------------------
# measurement
# ---------------------------------------------------------------------------

def header_field(headers):
    """ffprobe's `-headers` value: CRLF-separated, per its HTTP implementation."""
    return '\r\n'.join('{}: {}'.format(name, value)
                       for name, value in sorted((headers or {}).items()))


def measure(candidate, binary=None):
    """Ask ffprobe what this address carries, with the headers attached.

    The probe is stored on the candidate and returned. An address that never got
    measured stays unproven, which `admit` keeps as a last resort: a slow origin
    costs the user a worse first choice, not no choice.
    """
    candidate.probe = probe_url(candidate.url, candidate.headers, binary=binary)
    return candidate.probe


def probe_url(url, headers=None, binary=None):
    binary = binary or find_command('ffprobe')
    if binary is None:
        return Probe(reach=REACH_UNPROVEN, error='no ffprobe on this machine')
    command = [binary, '-hide_banner', '-loglevel', 'error',
               '-print_format', 'json', '-show_format', '-show_streams',
               '-analyzeduration', '4000000', '-probesize', '4000000',
               # A manifest is not one file: without the segment protocols ffprobe
               # refuses to open it before reading a line of the playlist.
               '-protocol_whitelist', 'file,http,https,tcp,tls,crypto,httpproxy,data']
    if headers:
        command += ['-headers', header_field(headers)]
    command.append(url)
    try:
        completed = subprocess.run(command, stdout=subprocess.PIPE,
                                   stderr=subprocess.PIPE, timeout=PROBE_TIMEOUT,
                                   creationflags=console_flags(), text=True,
                                   encoding='utf-8', errors='replace')
    except subprocess.TimeoutExpired:
        return Probe(reach=REACH_UNPROVEN, error='probe timed out')
    except OSError as exc:
        return Probe(reach=REACH_UNPROVEN, error=str(exc))
    stderr = (completed.stderr or '').strip()
    if completed.returncode != 0:
        reach = REACH_REFUSED if _REFUSED_STATUS.search(stderr) else REACH_UNPROVEN
        return Probe(reach=reach, error=first_line(stderr))
    probe = parse_probe_json(completed.stdout)
    if probe is None:
        return Probe(reach=REACH_UNPROVEN, error='unreadable probe output')
    probe.reach = REACH_OPENED
    return probe


def first_line(text):
    for line in (text or '').splitlines():
        if line.strip():
            return line.strip()[:200]
    return ''


def parse_probe_json(text):
    """ffprobe's JSON into a `Probe`. Split out so the tests feed it a fixture."""
    try:
        info = json.loads(text)
    except ValueError:
        return None
    if not isinstance(info, dict):
        return None
    fmt = info.get('format') or {}
    streams = [s for s in (info.get('streams') or []) if isinstance(s, dict)]
    video = next((s for s in streams if s.get('codec_type') == 'video'), None)
    audio = next((s for s in streams if s.get('codec_type') == 'audio'), None)
    probe = Probe(container=str(fmt.get('format_name') or ''),
                  duration=_number(fmt.get('duration')),
                  bitrate=_number(fmt.get('bit_rate')))
    if video:
        probe.video_codec = str(video.get('codec_name') or '')
        probe.width = int(_number(video.get('width')))
        probe.height = int(_number(video.get('height')))
    if audio:
        probe.audio_codec = str(audio.get('codec_name') or '')
    if not probe.bitrate and video:
        probe.bitrate = _number(video.get('bit_rate'))
    # A live playlist reports no duration; that is the finding, not a gap.
    probe.is_live = not probe.duration
    return probe


def _number(value):
    try:
        return float(value)
    except (TypeError, ValueError):
        return 0.0


# ---------------------------------------------------------------------------
# the two tables (ported from castor, MIT -- see the module header)
# ---------------------------------------------------------------------------

REASON_CASTABLE = 'carried a castable program'
REASON_UNPROVEN = 'unmeasurable, offered as a last resort'
REASON_REFUSED = 'refused by the origin'
REASON_AUDIO_ONLY = 'carried sound but no picture, kept for an audio cast'
REASON_NO_PROGRAM = 'carried no moving picture'
REASON_HEADER_ONLY = 'an MP4 header with no timeline of its own, offered as a last resort'

#: Ordered rows -- (reason, when, keep, last_resort) -- and the first match
#: decides. The order *is* the contract: a refused address must not be rescued by
#: a later row, and an unmeasurable one must not be read as "no video".
#: castor's `reasonTooShort` row is absent by design (module header).
ADMISSION_TABLE = (
    (REASON_REFUSED,
     lambda c: c.probe is not None and c.probe.reach == REACH_REFUSED,
     False, False),
    (REASON_UNPROVEN,
     lambda c: c.probe is None or not c.probe.measured,
     True, True),
    # A measured, clean read that carries no moving picture. Sound on its own is
    # still castable (music, a podcast), so it is kept -- but said out loud.
    (REASON_AUDIO_ONLY,
     lambda c: not c.probe.moving_picture and bool(c.probe.audio_codec),
     True, False),
    (REASON_NO_PROGRAM,
     lambda c: not c.probe.moving_picture,
     False, False),
    # A fragment header captured instead of the file it heads: keep it, but a
    # manifest that lists it is the better attempt.
    (REASON_HEADER_ONLY,
     lambda c: (not c.segmented and c.probe.duration == 0
                and 'mp4' in (c.probe.container or '')),
     True, True),
    (REASON_CASTABLE, lambda c: True, True, False),
)


def admit(candidate):
    """Apply the table to one candidate: (keep, last_resort, reason)."""
    for reason, when, keep, last in ADMISSION_TABLE:
        if when(candidate):
            return keep, last, reason
    return True, False, REASON_CASTABLE


def exceeds_cap(candidate, ceiling):
    """Whether the measured picture is a real height above the ceiling.

    Zero is *unknown*, not short: a manifest nobody could read must not be thrown
    away for having no height, and neither may an unmeasured last resort.
    """
    if ceiling <= 0 or candidate.last_resort or candidate.segmented:
        return False
    return candidate.height > ceiling


def preference(a, b, ceiling=0):
    """castor's comparator, transposed to Python. <0 when `a` should come first.

    The order of the questions is the argument for that order: measured before
    unmeasured, inside the ceiling before outside it, then a progressive file
    before a manifest -- *for us*, because the relay can translate Range requests
    on a progressive file and the viewer can seek, which it cannot do on a remuxed
    stream. (castor puts a ladder first because a master carries rungs to fall
    back to; that is a recovery signal about *its* engine's hand-off, and ours
    serves the bytes itself.) Then taller, then higher bitrate, then the URL so a
    tie is reproducible.
    """
    if bool(a.last_resort) != bool(b.last_resort):
        return 1 if a.last_resort else -1
    a_over, b_over = exceeds_cap(a, ceiling), exceeds_cap(b, ceiling)
    if a_over != b_over:
        return 1 if a_over else -1
    if a.segmented != b.segmented:
        return -1 if not a.segmented else 1
    if (a.height > 0) != (b.height > 0):
        return 1 if b.height > 0 else -1
    if a.height != b.height:
        return -1 if a.height > b.height else 1
    a_bit = a.probe.bitrate if a.probe else 0.0
    b_bit = b.probe.bitrate if b.probe else 0.0
    if a_bit != b_bit:
        return -1 if a_bit > b_bit else 1
    return (a.url > b.url) - (a.url < b.url)


def rank(candidates, ceiling=0):
    """Best first, every admitted candidate present, each labelled with its fate."""
    kept = []
    for candidate in candidates:
        keep, last, reason = admit(candidate)
        if not keep:
            continue
        candidate.last_resort = last
        candidate.reason = reason
        kept.append(candidate)
    return sorted(kept, key=functools.cmp_to_key(
        lambda x, y: preference(x, y, ceiling)))


def visible(candidates):
    """(rows to show, rows hidden for having no sound at all).

    On a DASH ladder the *top* rung is routinely picture-only -- bilibili's 1080p
    is exactly that -- so leaving it on the card is how「投上去没声音」comes back as
    a bug report about our relay. Only a measured, silent, unpairable row is
    dropped; `Candidate.silent` says why an unknown row is not the same thing.
    """
    kept = [c for c in candidates if not c.silent]
    return kept, [c for c in candidates if c.silent]


# ---------------------------------------------------------------------------
# a pasted share blurb
# ---------------------------------------------------------------------------

#: Share text is a sentence with one URL buried in it, wrapped in the app's own
#: full-width punctuation -- which is why the class excludes it: `…https://v.douyin.
#: com/iAbc/，复制打开抖音` would otherwise carry the comma into the address.
_SHARE_URL = re.compile(
    '''https?://[^\\s<>"'\u3000，。、；：！？（）【】《》“”‘’]+''',
    re.IGNORECASE)


def looks_like_url(text):
    """Whether the whole paste *is* an address, and not a sentence holding one."""
    text = (text or '').strip()
    if not text or len(text.split()) != 1:
        return False
    return urllib.parse.urlsplit(text).scheme.lower() in ('http', 'https')


def extract_share_urls(text):
    """Every http(s) address inside a paste, in the order they appear, unique.

    The bare-host rule still holds: this only ever returns something that already
    carries a scheme, so a paste of `example.com/watch/7` yields nothing here and
    the caller still refuses it. Feeding that string to yt-dlp would turn it into a
    *search* for the text and hand back an answer that looks like "this site has no
    video".
    """
    found = []
    for match in _SHARE_URL.finditer(text or ''):
        url = match.group(0).rstrip('.,;:!?)\'"')
        if url not in found:
            found.append(url)
    return found


def resolve_target(text):
    """`(address, how many were found)` for whatever the user pasted.

    The share blurb is the whole reason this exists: what a phone actually hands
    over is a sentence -- `7.12 复制打开抖音，看看【…】https://v.douyin.com/iAbc/ …` --
    and asking people to cut the URL out of it is a UI failure we can fix here. The
    first address is the one we parse; the count is what the page says back, so a
    multi-part post is understood as "this part" rather than silently dropping the
    rest. `('', 0)` means the paste carried no address at all, which is the caller's
    refusal to phrase -- and the bare-host refusal in `looks_like_url`'s own words.
    """
    text = (text or '').strip()
    if not text:
        return '', 0
    if looks_like_url(text):
        return text, 1
    found = extract_share_urls(text)
    return (found[0], len(found)) if found else ('', 0)


def display_title(candidate, fallback=''):
    """What to call this on the television.

    `label` is provenance -- `video:src`, or yt-dlp's `mp4 720p` -- not a title,
    and handing it to `SetAVTransportURI` is how a DLNA renderer ends up printing
    "mp4" where the user expected the name of a film. So: the page's own title,
    then the file's own name without its container suffix, then the address we
    were pointed at. Nothing here invents a title.
    """
    title = (candidate.title or '').strip()
    if title:
        return title[:120]
    stem = os.path.basename(urllib.parse.urlsplit(candidate.url).path)
    stem = urllib.parse.unquote(stem).strip()
    for suffix in MEDIA_SUFFIXES:
        if stem.lower().endswith(suffix):
            stem = stem[:-len(suffix)].strip()
            break
    if stem:
        return stem[:120]
    return fallback or candidate.url


def container_label(candidate):
    """The one word to put on the card for "what container is this".

    `probe.container` is ffprobe's honest answer, which for an MP4 is the whole
    family list -- `mov,mp4,m4a,3gp,3g2,mj2` -- correct in a machine and noise in
    a badge. So the address's own suffix comes first, then the first name
    ffprobe offered. The raw list stays on the `Probe`, because the admission
    table asks "is mp4 anywhere in it", not "is it the first one".
    """
    suffix = candidate.path_suffix
    if suffix:
        return suffix.lstrip('.').upper()
    listed = (candidate.probe.container or '') if candidate.probe else ''
    first = listed.split(',')[0].strip()
    return first.upper()


def relay_reason(candidate):
    """Why this address is served from Macast rather than handed to the device.

    The rule castor states in `compose`, and that reading the headers yt-dlp
    returns confirms: one request header disqualifies the hand-off. The device
    will fetch the URL minutes from now, from its own IP, with no Referer at all.
    """
    if candidate.audio_url:
        return '画面与声音是两条地址（DASH 分轨）：由 Macast 合并成一条流供出'
    if candidate.segmented:
        return '清单流（HLS/DASH）：电视不会自己读清单，由 Macast 转封装供流'
    if candidate.headers:
        return '源站要求请求头（{}）：电视不会带上这些，由 Macast 代取'.format(
            '、'.join(sorted(candidate.headers)))
    return '由 Macast 供流：签名地址会过期，交给电视自己取迟早 403'


def plan(candidate):
    """'proxy', 'remux', or 'merge' -- which of the three shapes serves this one.

    `merge` is a remux with a second input: the picture and the sound arrive as two
    addresses and ffmpeg copies both into one growing MP4. It therefore inherits
    every property the relay gives a remuxed file (no length while it grows, no
    seeking until ffmpeg exits 0), which is why `media_relay` keeps one serving
    shape and two ways of filling it.
    """
    if candidate.audio_url:
        return 'merge'
    return 'remux' if candidate.segmented else 'proxy'


# ---------------------------------------------------------------------------
# the job the page polls
# ---------------------------------------------------------------------------

class ResolveJob:
    """One page, resolved on a background thread, with progress the page can read.

    Extraction is 12-19 s on a real site, so the caller cannot wait inside a
    request, and a half-minute spinner teaches the user nothing. `status()` names
    which of the four steps is running and how long it has taken.
    """

    STEPS = ('scrape', 'ytdlp', 'measure', 'rank')

    def __init__(self, page_url, max_height=0, on_finish=None, cookies=None,
                 note=''):
        self.page_url = page_url
        self.max_height = max_height
        self.on_finish = on_finish
        self.cookies = cookies or {}
        self.note = note
        self.candidates = []
        self.error = ''
        self.done = False
        self.started = time.time()
        self.step = 'queued'
        self.step_started = self.started
        self.scraped = 0
        self.from_ytdlp = 0
        self.measured = 0
        self.rejected = 0
        self.engine_note = ''
        self.hidden_silent = 0
        self._lock = threading.Lock()
        self._thread = None

    def start(self):
        self._thread = threading.Thread(target=self.run, daemon=True,
                                        name='RESOLVE_PAGE')
        self._thread.start()

    def _enter(self, step, **facts):
        with self._lock:
            self.step = step
            self.step_started = time.time()
            for name, value in facts.items():
                setattr(self, name, value)

    def run(self):
        try:
            self._resolve()
        except Exception as exc:
            # A resolver must never take the app down; but it must say so, in a
            # sentence the page can show, rather than stopping mid-step.
            with self._lock:
                self.error = '解析出错：{}: {}'.format(type(exc).__name__, exc)
        finally:
            with self._lock:
                self.done = True
                self.step = 'done'
            if self.on_finish is not None:
                try:
                    self.on_finish(self)
                except Exception:
                    pass

    def _resolve(self):
        self._enter('scrape')
        scraped = scrape_page(self.page_url)
        self._enter('ytdlp', scraped=len(scraped))
        notes = []
        resolved = ytdlp_candidates(self.page_url, cookies=self.cookies,
                                    sink=notes)
        self._enter('measure', from_ytdlp=len(resolved),
                    engine_note=notes[0] if notes else '')
        pool = dedupe(scraped + resolved)
        if not pool:
            with self._lock:
                self.candidates = []
                self.error = '页面里没有读到视频地址' + self._why_empty()
            return

        hosts = {}
        for candidate in pool:
            with self._lock:
                if self.measured >= MAX_MEASURED:
                    break
            host = candidate.host
            if hosts.get(host, 0) >= MAX_MEASURED_PER_HOST:
                continue
            measure(candidate)
            hosts[host] = hosts.get(host, 0) + 1
            self._enter('measure', measured=self.measured + 1)

        self._enter('rank', measured=self.measured)
        with self._lock:
            ranked = rank(pool, self.max_height)
            shown, hidden = visible(ranked)
            self.candidates = shown
            self.hidden_silent = len(hidden)
            self.rejected = len(pool) - len(shown)
            if not shown:
                self.error = ('读到的 {} 个候选里，{} 条只有画面、又配不出能合并的声音，'
                              '其余没有一个能通过判定'.format(len(pool), len(hidden))
                              if hidden else
                              '读到的 {} 个地址没有一个能通过判定（源站拒绝，或没有画面）'.format(
                                  len(pool)))

    def _why_empty(self):
        """Why nothing came back, in whichever of the two voices actually spoke.

        A machine without yt-dlp and an engine that answered are different
        sentences -- and the engine's own words are the only explanation a
        cookie-walled site ever gives, which is the reason `ytdlp_candidates`
        hands its first stderr line back instead of swallowing it.
        """
        if self.engine_note:
            return '（yt-dlp 说：{}）'.format(self.engine_note)
        if find_command('yt-dlp') is None:
            return '（这台机器没有装 yt-dlp，只能抓 HTML 里写明的地址）'
        return ''

    def status(self):
        """The dict `query=resolve-status` hands the page.

        The counters and the step name come from the same lock the worker writes
        under, so a page polling twice cannot see the count go backwards.
        """
        with self._lock:
            return {
                'url': self.page_url,
                'note': self.note,
                'done': self.done,
                'step': self.step,
                'step_label': STEP_LABELS.get(self.step, self.step),
                'seconds': round(time.time() - self.started, 1),
                'step_seconds': round(time.time() - self.step_started, 1),
                'scraped': self.scraped,
                'from_ytdlp': self.from_ytdlp,
                'measured': self.measured,
                'rejected': self.rejected,
                'hidden_silent': self.hidden_silent,
                'engine_note': self.engine_note,
                # Computed here and not in the page: which of an engine's many
                # refusal sentences means "bring a session" is our guess at
                # somebody else's wording, so it needs one owner the tests can pin.
                'needs_cookies': needs_cookies(self.engine_note),
                'error': self.error,
                'candidates': [describe(c) for c in self.candidates],
            }


#: The Chinese names the page prints for a step. Kept next to `STEPS` because a
#: new step with no label would render as an English word in a Chinese sentence.
STEP_LABELS = {
    'queued': '排队中',
    'scrape': '抓取页面',
    'ytdlp': 'yt-dlp 解析页面',
    'measure': '度量候选地址',
    'rank': '排序',
    'done': '完成',
}


def describe(candidate):
    """One candidate as the page shows it -- every number read from the probe.

    The page computes none of this: a second copy of the arithmetic in the front
    end is the「提示比代码活得久」failure again, and here the values come from a
    probe that ran on this machine, so nothing else could state them correctly.
    """
    probe = candidate.probe
    return {
        'url': candidate.url,
        'origin': candidate.origin,
        'label': candidate.label,
        'title': display_title(candidate),
        'container': container_label(candidate),
        'video_codec': probe.video_codec if probe else '',
        'audio_codec': probe.audio_codec if probe else '',
        'width': candidate.width,
        'height': candidate.height,
        'duration': round(candidate.duration, 1),
        'bitrate': round(probe.bitrate / 1000) if probe and probe.bitrate else 0,
        'measured': bool(probe and probe.measured),
        'reach': probe.reach if probe else REACH_UNPROVEN,
        'live': bool(probe and probe.is_live),
        'segmented': candidate.segmented,
        'last_resort': candidate.last_resort,
        'reason': candidate.reason,
        'mode': plan(candidate),
        'relay': relay_reason(candidate),
        'headers': sorted(candidate.headers),
        # The card's「这条没有音轨」badge and its merged sibling. Computed here and
        # not in the page: `audio_codec` comes from a probe this machine ran, and a
        # second copy of that reading in JavaScript is「提示比代码活得久」again.
        'merged': bool(candidate.audio_url),
        'audio_label': candidate.audio_label,
        'silent': candidate.silent,
    }


def resolve_now(page_url, max_height=0, cookies=None):
    """Synchronous resolve, for a caller that can afford to wait (tests, CLI)."""
    job = ResolveJob(page_url, max_height=max_height, cookies=cookies)
    job.run()
    return job
