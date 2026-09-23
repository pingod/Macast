# Copyright (c) 2026 by pingod. All Rights Reserved.
#
# Nothing in this file came from xfangfang/Macast: it is written in this fork,
# which is why it carries no "Derived from" line (docs/Provenance.md, §0).
"""What the 电脑投屏 state means on screen, computed in the app, not in the page.

`ScreenMirrorSetting.console_state()` reports facts (which devices answered, what
the encoder is, how far the assisted install got). Something still has to turn
those facts into a layout: which panels this machine warrants, which line is the
one to put in the red strip, what a pill may say about itself. This module is
that layer, and it answers with plain JSON-ready values so the settings page can
be a renderer rather than a second implementation of the rules.

Why it sits in the core instead of in the page or in the plugin:

* the page is a cached single file (`load_xml` reads it once per process, and a
  PWA service worker may hold an older copy), so a rule living in its JS is a
  rule that keeps running after an upgrade;
* the plugin owns *what is true*, not what to show -- it already had to grow a
  `banner`, a `prompt` and a `words` line each time the console needed a
  sentence, and those are presentation decisions;
* here they are testable as data. The regression suite feeds this module the
  states a mirror session actually produces and asks what came out; nothing in
  this file needs a display, a browser, or a running app.

The one rule worth keeping in mind while editing: **a panel the user can never
touch is a bug, not a spare row** (see `sections_for`).
"""

#: Bumped when the state/view contract changes. The page compares it with the
#: version it was written against, because a stale cached page against a newer
#: app is the failure this can name -- and only this can name it, since every
#: other field still answers politely. v2 is the protocol-first view model:
#: `channels` replaced `search`, and `requirements` appeared. v3 is the browser
#: console: the preview is PNG-only (no `fmt`), and the snapshot endpoint no
#: longer takes a format at all. v4 adds the 统计信息 card (`stats.diag`, the
#: search trace) so a delay can be read off numbers instead of guessed at.
VIEW_VERSION = 4

#: Every panel this module can ask for, in layout order. Not a preference list:
#: `sections_for` returns a subset of this, and the page renders in the order it
#: is given, so the ordering decision stays here.
SECTION_ORDER = ('channels', 'devices', 'requirements', 'profiles', 'quality',
                 'capture', 'audio', 'viewer', 'preview', 'diagnostics',
                 'activity')

#: Card names to fall back to when the app answers without a catalog -- which is
#: what it does while the app is still starting, or with a plugin that failed to
#: load. Without these the rows read 「cast」/「dlna」, which is a key name, not a
#: label. The regression suite pins each one against the plugin's own `OUTPUTS`.
OUTPUT_FALLBACK = {
    'cast': 'Chromecast / Google TV',
    'caststream': 'Chromecast 低延迟（实验 · 此通道无声音）',
    'dlna': 'DLNA 电视（老电视，MPEG-PS）',
    'browser': '浏览器（打开网址即可看）',
}

#: What a protocol with no devices to find is asked for. The browser target has
#: nothing to search, and 「没有发现」about it would be a lie -- its answer is the
#: address the page shows once a mirror runs.
NO_PROBE_WORDS = '不需要设备'

# Severity names for the strip, not colours: the page owns its palette, and the
# assisted-install steps need to say "this failed" in both themes.
GOOD = 'good'
BAD = 'bad'
WARN = 'warn'
MUTED = 'muted'
ACCENT = 'accent'
DIM = 'dim'


def sections_for(state):
    """Which panels this state warrants, in layout order.

    Not "everything, greyed out": a Windows machine has no BlackHole to install
    and an old TV target has no compatibility profiles to pick when the output is
    a browser, and rows that can never be touched make the page look broken
    rather than complete.

    「投屏方式」is always there: it answers "which protocols can this machine send
    to", which has an honest reply even when every search came back empty.
    「设备」belongs to whichever protocol is chosen, so it appears only for the two
    that have devices to find; 「需要安装」only while something is actually
    outstanding, because that is when it is the whole point.
    """
    kind = (state.get('output') or {}).get('kind', 'cast')
    channel = current_channel(state)
    sections = ['channels']
    if channel.get('needs_device') or kind in ('cast', 'caststream', 'dlna'):
        sections.append('devices')
    if state.get('requirements'):
        sections.append('requirements')
    if kind == 'dlna':
        sections.append('profiles')
    sections.append('quality')
    sections.append('capture')
    if state.get('platform') == 'darwin' or (state.get('audio') or {}).get('line'):
        sections.append('audio')
    if kind == 'browser':
        sections.append('viewer')
    sections.append('preview')
    # 「统计信息」has nothing to say until a session exists, and a row of empty
    # placeholders is worse than no card: this is a debugging surface, so it
    # appears exactly when there is something running to debug.
    if state.get('mirroring') and (state.get('stats') or {}).get('diag'):
        sections.append('diagnostics')
    sections.append('activity')
    return sections


def current_channel(state):
    """The channel row for the protocol being mirrored to, or `{}`.

    The device list, the「还要选一台」prompt and the compatibility profiles all
    hang off whichever protocol is chosen, so they read from this row rather than
    from a single list that silently changes meaning with the target.
    """
    kind = (state.get('output') or {}).get('kind')
    for row in state.get('channels') or []:
        if row.get('key') == kind:
            return row
    return {}


def channel_rows(channels):
    """[(key, label, words, selected, needs_device, choice, trade-off)].

    `words` is what that protocol's last search actually concluded -- per
    protocol, which is the point of the panel: an empty Chromecast answer says
    nothing about the DLNA side of the same LAN. `choice` is the device the
    stored settings would actually use, and the trade-off line is what choosing
    this protocol costs, which is the other half of the decision.
    """
    out = []
    for row in channels or []:
        needs = bool(row.get('needs_device'))
        out.append([row.get('key', ''),
                    row.get('label') or OUTPUT_FALLBACK.get(row.get('key'), ''),
                    row.get('words') or ('' if needs else NO_PROBE_WORDS),
                    bool(row.get('selected')), needs,
                    row.get('choice') or '',
                    row.get('hint') or ''])
    return out


def format_stats(state):
    """One line of live numbers for the header, or '' before there are any.

    The plugin already keeps `_status_line` for its own use; reusing its text is
    what stops the header and the menu from disagreeing about a mirror's state.
    """
    line = state.get('status_line') or ''
    if line:
        return line
    if state.get('starting'):
        return '正在开始镜像…'
    return '未镜像'


def device_rows(channel):
    """[(key, label, selected, name)] for one protocol's devices -- [] if none.

    The key is what `console_action` needs back, so it is carried here instead of
    being reconstructed from the label (the label has a ` · ` in it). `name`
    travels too: the plugin records targets by name, not by display string.
    """
    return [[item.get('id', ''), item.get('label', ''),
             bool(item.get('selected')), item.get('name', '')]
            for item in (channel or {}).get('devices') or []]


def search_note(channel):
    """The line under the device list: what this protocol's search concluded.

    Written by the plugin (`_search_words`), because the wording has to match the
    one a refused start reports -- two phrasings drift, and the user is left
    deciding which「没有发现」was meant.
    """
    return (channel or {}).get('words') or ''


def quality_text(quality):
    """({key: pill text}, note line) for a quality block.

    Four pills share one row, so each can only carry its resolution -- and the
    bandwidth cost is exactly what the choice turns on, so the chosen preset's
    full label goes on the line below rather than being dropped with it. The
    plugin's own note wins when it has one: it is situational (the low-latency
    channel writing「上限 4.5 Mbps」over a 10 Mbps preset is precisely the case
    where quoting the preset's number would mislead).
    """
    labels = {item.get('key'): item.get('label', '')
              for item in quality.get('options') or []}
    short = {key: (text.split('·')[0].strip() or text)
             for key, text in labels.items()}
    note = quality.get('note') or labels.get(quality.get('current')) or ''
    return short, note


def audio_step_mark(step):
    """(glyph, severity) for one assisted-install step.

    `skipped` is a state of its own, not "pending with a note": the v0.9 fix for
    the install loop was that 「跳过」has to be a value the step machine carries,
    or a machine that already has the driver re-downloads the package every click.
    """
    state = step.get('state')
    return {'running': ('▶', ACCENT), 'done': ('✓', GOOD),
            'fail': ('✗', BAD), 'skipped': ('—', DIM),
            'pending': ('○', DIM)}.get(state, ('?', MUTED))


def banner_for(state):
    """The strip that has to be read first: {'text': str, 'level': str|None}.

    Order is deliberate: a version mismatch beats a missing plugin beats an
    unprobed machine, because each one earlier explains why the ones after it are
    not worth acting on yet. ("Can't reach Macast" used to sit above these; it is
    the page's own connection state now, and it decided by a request failing
    rather than by a string in here.)
    """
    remote = state.get('console_version')
    if remote is not None and remote != VIEW_VERSION:
        return {'text': ('这个页面按 v{} 的接口写的，Macast 是 v{}：强制刷新一次'
                         '（缓存里可能是升级前的页面）').format(VIEW_VERSION, remote),
                'level': WARN}
    if not state.get('available'):
        return {'text': '没有可用的电脑投屏插件：在「插件」里启用 Screen Mirror',
                'level': WARN}
    if not (state.get('capture') or {}).get('probed'):
        return {'text': '正在探测这台电脑的采集设备…（首次打开需要一两秒）',
                'level': MUTED}
    return {'text': '', 'level': None}


def _mib(value):
    """Bytes as MiB with one decimal; '' when there is no number at all."""
    try:
        number = float(value)
    except (TypeError, ValueError):
        return ''
    if number <= 0:
        return '0'
    return '{:.1f} MiB'.format(number / 1048576.0)


#: The encoder choice, said the way the switch on the page says it. A raw
#: 'hardware' next to 'VideoToolbox 硬件编码' is the same fact in two languages.
ENCODER_LABELS = {'hardware': '硬件编码（VideoToolbox）', 'software': '软件编码（x264）'}


def diagnostics_rows(state):
    """[[label, value]]: the raw numbers behind one running session.

    This is the card that makes「延迟有点大」measurable. Every term the delay is
    assembled from -- the queue, the replay tail, the prefill, the keyframe
    interval, the encoder, the actual argv -- is on one screen, so the next
    report can say *which* term is the one that is too big instead of being
    argued about. Rows with nothing to say are left out rather than shown
    blank: an empty card is worse than no card.

    Labels are Chinese and values are strings, because the page is a renderer;
    the same rows are what `diagnostics_text` copies out.
    """
    stats = state.get('stats') or {}
    diag = stats.get('diag') or {}
    if not diag:
        return []
    rows = []

    def add(label, value):
        if value is None or value == '':
            return
        rows.append([label, str(value)])

    kind = diag.get('kind') or (state.get('output') or {}).get('kind', '')
    add('投屏方式', current_channel(state).get('label') or kind)
    add('采集', diag.get('capture'))
    add('编码器', ENCODER_LABELS.get(str(diag.get('encoder') or ''),
                                    diag.get('encoder')))
    if diag.get('bitrate'):
        height = diag.get('height')
        add('目标画质', '{}p · {:.1f} Mbps'.format(height, diag['bitrate'] / 1000000.0)
            if height else '{:.1f} Mbps'.format(diag['bitrate'] / 1000000.0))
    if diag.get('fps'):
        add('帧率', '{} fps'.format(diag['fps']))
    if diag.get('gop') and diag.get('fps'):
        add('关键帧间隔', '{} 帧 · {:.1f} 秒'.format(
            diag['gop'], diag['gop'] / float(diag['fps'])))
    if diag.get('audio'):
        add('系统声音', '已启用 · {}'.format(diag.get('audio_map') or ''))
    elif diag.get('audio_expected'):
        # Asked for and given up: this session retried without the tap.
        add('系统声音', '本次已放弃（采集口打不开，只采了画面）')
    else:
        add('系统声音', '未启用')
    if diag.get('cast_refused'):
        add('低延迟通道', '设备已拒绝，回落到兼容通道（{}）'
            .format(diag['cast_refused']))

    # -- what the sender holds in front of the viewer ------------------------
    if diag.get('prefill_bytes'):
        add('预填缓冲（延迟来源）', '{} · 约 {} 秒'.format(
            _mib(diag['prefill_bytes']), diag.get('prefill_seconds')))
        add('DLNA 档位', diag.get('profile'))
    elif diag.get('queue_seconds') is not None:
        add('发送队列（延迟来源）', '{:.2f} 秒 · {} 块'.format(
            diag['queue_seconds'], diag.get('queue_chunks')))
    if diag.get('replay_bytes'):
        add('回放缓冲（延迟来源）', _mib(diag['replay_bytes']))
    elif 'replay_bytes' in diag:
        add('回放缓冲', '无，客户端跟直播边缘')

    # -- live numbers, from the broadcaster --------------------------------
    if stats.get('mbps') is not None:
        add('实测码率', '{:.2f} Mbps'.format(stats['mbps']))
    if stats.get('clients') is not None:
        add('观看端', '{} 个'.format(stats['clients']))
    add('累计发送', _mib(stats.get('bytes')))
    add('分块', stats.get('chunks'))
    add('丢块', stats.get('drops'))
    if stats.get('seconds') is not None:
        add('已运行', '{} 秒'.format(stats['seconds']))
    if kind == 'caststream':
        add('在途帧', stats.get('in_flight'))
    if kind == 'dlna':
        add('电视上报状态', stats.get('state'))
        add('电视侧缓冲', _mib(stats.get('buffered')))

    # -- discovery, because "找不到设备" and "包没发出去" look the same
    for row in (state.get('search_trace') or {}).get('dlna') or []:
        detail = '{} 个应答'.format(row.get('answers'))
        if row.get('error'):
            detail = '发送失败：{}'.format(row['error'])
        rows.append(['发现尝试 · {}'.format(row.get('interface')),
                     detail])
    add('ffmpeg 命令', diag.get('command'))
    return rows


def diagnostics_text(state):
    """The same rows as one copyable block, plus what the search tried.

    A pasted block is how a report about latency becomes actionable: it carries
    the argv, the encoder, the budgets and the discovery trace in one piece of
    text, so nothing has to be asked for twice. Built from the rows, so the
    card and the clipboard can never disagree.
    """
    rows = diagnostics_rows(state)
    if not rows:
        return ''
    lines = ['Macast 电脑投屏统计信息',
             '插件 {} · 页面契约 v{}'.format(
                 state.get('version', '?'), VIEW_VERSION),
             '']
    for label, value in rows:
        lines.append('{}: {}'.format(label, value))
    words = (current_channel(state).get('words') or '')
    if words:
        lines += ['', '设备发现: {}'.format(words)]
    return '\n'.join(lines)


def view_for(state):
    """Everything the page lays out but cannot decide for itself.

    Deliberately a snapshot of derived *presentation* values, not a copy of the
    state: the fields the page reads straight out of `state` (`output.options`,
    `channels[].label`, `profiles.options`, `capture.screens`, …) are already
    finalised Chinese, and repeating them here would only give them a second
    place to drift.
    """
    channel = current_channel(state)
    pills, note = quality_text(state.get('quality') or {})
    return {
        'version': VIEW_VERSION,
        'sections': sections_for(state),
        'banner': banner_for(state),
        'header': format_stats(state),
        'channels': channel_rows(state.get('channels') or []),
        'devices': device_rows(channel),
        'device_note': search_note(channel),
        'quality': {'labels': pills, 'note': note},
        'diagnostics': diagnostics_rows(state),
        'diagnostics_text': diagnostics_text(state),
        'step_marks': [audio_step_mark(step)
                       for step in (state.get('audio') or {}).get('steps') or []],
    }
