(() => {
'use strict';

/* ---- lib/bridge.js ---- */
const __bridge = (() => {
/*
 * The one place that talks to the backend.
 *
 * Every command goes through `call()`, which means: one shape for a failure, one place
 * that logs a rejected promise, and one place to look when the bridge is missing.
 *
 * ### Why a `call()` wrapper instead of `invoke` at each call site
 *
 * Two defects in this page's history were *silent*: a renderer that read a key nothing
 * sent, and a boot block that was deleted while its cards stayed. Both had the same tell —
 * nothing threw. A wrapper cannot catch a missing key, but it does catch the other half:
 * a command that fails now always reaches the event feed, so "nothing happened" stops
 * being indistinguishable from "it failed quietly".
 */

const core = globalThis.__TAURI__?.core;
const eventBridge = globalThis.__TAURI__?.event;

/** True when this document is running inside the shell. */
const bridged = typeof core?.invoke === 'function';

/**
 * Invoke one command, normalising every outcome to `{ok, data, error}`.
 *
 * A rejection is *not* rethrown: the caller decides what an empty state looks like, and a
 * page that loses an unhandled rejection also loses the rest of its boot sequence.
 */
async function call(name, args) {
  if (!bridged) {
    return { ok: false, error: '这个窗口没有后端桥（请通过 Tauri 运行）', data: null };
  }
  try {
    return { ok: true, data: await core.invoke(name, args), error: null };
  } catch (error) {
    return { ok: false, data: null, error: String(error?.message ?? error) };
  }
}

/** Invoke and return the data, or `null` — for reads whose absence has a rendering. */
async function read(name, args) {
  const { ok, data } = await call(name, args);
  return ok ? data : null;
}

/**
 * Subscribe to one backend event; a missing bridge is not an error worth showing.
 *
 * `payload` is the array `cherrypy.engine.publish` was given, so a listener that wants
 * the first argument reads `payload[0]`. Passing the array through unchanged is what lets
 * the feed show exactly what the back end published instead of a summary of it.
 */
function subscribe(name, handler) {
  if (typeof eventBridge?.listen !== 'function') return;
  eventBridge.listen(name, ({ payload }) => handler(payload)).catch(() => {});
}

/* ------------------------------------------------------------------ formatting */

/** Human bytes. `—` rather than `0 B` when the number is not one: an absent size is not
 * a size of zero. */
function fmtBytes(n) {
  if (typeof n !== 'number' || !Number.isFinite(n)) return '—';
  if (n < 1024) return `${n} B`;
  if (n < 1048576) return `${(n / 1024).toFixed(1)} KiB`;
  return `${(n / 1048576).toFixed(1)} MiB`;
}

/** Seconds to `m:ss` / `h:mm:ss`. The player reports seconds as a decimal string. */
function fmtClock(seconds) {
  const total = Number(seconds);
  if (!Number.isFinite(total) || total < 0) return '—';
  const whole = Math.floor(total);
  const h = Math.floor(whole / 3600);
  const m = Math.floor((whole % 3600) / 60);
  const s = whole % 60;
  const mm = h ? String(m).padStart(2, '0') : String(m);
  return `${h ? `${h}:` : ''}${mm}:${String(s).padStart(2, '0')}`;
}

/** A duration in words, for the candidate rows where `212.3 秒` reads better than a clock. */
function fmtSeconds(v) {
  return typeof v === 'number' && Number.isFinite(v) ? `${v.toFixed(1)} 秒` : '—';
}

/** Time of day for the event feed's stamp: seconds matter when two things race. */
function fmtStamp(date = new Date()) {
  return date.toLocaleTimeString('zh-CN', { hour12: false });
}

/**
 * Three-state service health, shared by every readout.
 *
 * `healthy` is only meaningful while `running`; the backend's own `is_healthy()` answers
 * *true* for a deliberately stopped server (a stopped server is not a fault), so a caller
 * that reads `healthy` alone reports green for a dead service. That asymmetry is why this
 * function exists instead of two conditionals at each call site.
 */
function serviceState(running, healthy) {
  if (!running) return { tone: 'warn', text: '已停止' };
  if (healthy) return { tone: 'ok', text: '在听' };
  return { tone: 'bad', text: '端口在听，工作线程已不在' };
}

/** The same three states for GENA, whose second question is "is the thread alive". */
function threadState(running, alive) {
  if (!running) return { tone: 'warn', text: '未启动' };
  if (alive) return { tone: 'ok', text: '在跑' };
  return { tone: 'bad', text: '要它跑，线程已不在' };
}

return { bridged, call, read, subscribe, fmtBytes, fmtClock, fmtSeconds, fmtStamp, serviceState, threadState };
})();

/* ---- lib/dom.js ---- */
const __dom = (() => {
/*
 * DOM helpers. No framework: this is one window with two views, and a build step would
 * cost more than the framework saves.
 *
 * The rule enforced here is the one that matters for security and for correctness alike:
 *
 * **text goes in through `textContent`, never through markup.** Every string this window
 * displays except its own copy comes from somewhere else — a DIDL `dc:title` a phone sent,
 * a URL from a page we scraped, a SID a control point invented, a log line an engine
 * wrote. `AGENTS.md` §4.10 records what happens when a title with an `<img onerror=…>`
 * reaches `v-html`: the settings page executes it. So there is no `html()` helper in this
 * file, deliberately — the capability is absent, not merely unused.
 */

/**
 * Build one element.
 *
 * `props` sets properties (not attributes) — `className`, `textContent`, `value`, `id` —
 * because those are the ones that behave identically for every element type.
 */
function el(tag, props = {}, children = []) {
  const node = document.createElement(tag);
  for (const [key, value] of Object.entries(props)) {
    if (value === undefined || value === null) continue;
    if (key === 'dataset') {
      Object.assign(node.dataset, value);
    } else if (key === 'style' && typeof value === 'object') {
      Object.assign(node.style, value);
    } else if (key.startsWith('on') && typeof value === 'function') {
      node.addEventListener(key.slice(2).toLowerCase(), value);
    } else {
      node[key] = value;
    }
  }
  for (const child of [].concat(children)) {
    if (child === null || child === undefined || child === false) continue;
    node.append(child instanceof Node ? child : document.createTextNode(String(child)));
  }
  return node;
}

/** An element found by id, with a message that names the id when it is missing. */
function byId(id) {
  const node = document.getElementById(id);
  if (!node) throw new Error(`页面缺少 #${id} —— 标记与脚本对不上了`);
  return node;
}

/** Write text into the element with this id, if it exists. */
function setText(id, text) {
  const node = document.getElementById(id);
  if (node) node.textContent = text ?? '';
}

/**
 * Set a state chip: its `data-tone` and its words.
 *
 * One function for both halves, because a chip whose colour and text disagree is worse
 * than no chip — `data-tone` is the only variant axis the stylesheet knows.
 */
function setChip(id, state) {
  const node = document.getElementById(id);
  if (!node) return;
  node.dataset.tone = state.tone;
  node.textContent = state.text;
}

/**
 * Replace a container's children, keeping the scroll position of the container itself.
 *
 * Lists on this page are rebuilt on a 1.5 s poll. Without this, a pane a user has scrolled
 * would jump back to the top on every tick — and a list rebuilt while the pointer is over
 * a row swallows the click that was about to land.
 */
function replaceChildren(container, children) {
  const { scrollTop } = container;
  container.replaceChildren(...[].concat(children).filter(Boolean));
  container.scrollTop = scrollTop;
}

/**
 * Render only when a signature changes.
 *
 * Every polled list on this page is compared by signature first. The reason is not
 * performance: rebuilding a list mid-interaction replaces the element the pointer is
 * pressing, which is exactly how "I had to click twice" bugs appear.
 */
function onceChanged(slot, signature, render) {
  if (slot.value === signature) return false;
  slot.value = signature;
  render();
  return true;
}

/** An empty-state block. Says what would fill it; a bare dash is a dead end. */
function emptyState(text) {
  return el('p', { className: 'empty', textContent: text });
}

/** An inline notice with a tone, for the result of one action. */
function notice(text, tone = '') {
  return el('p', { className: 'notice', dataset: tone ? { tone } : {}, textContent: text });
}

return { el, byId, setText, setChip, replaceChildren, onceChanged, emptyState, notice };
})();

/* ---- lib/feed.js ---- */
const __feed = (() => {
/*
 * The event feed and the toast area — the window's one channel for "something happened
 * that you did not ask to see".
 *
 * Two levels, and the split is deliberate:
 *
 * * **feed** — an append-only list of every backend event and every command result. This
 *   is the diagnostic surface: it is what a user pastes into a bug report, and it is where
 *   `AGENTS.md` §4.2's "non-answers are answers too" gets recorded.
 * * **toast** — one line, disappears. For the result of a click whose effect is also
 *   visible elsewhere, so the toast is a confirmation and not the only evidence.
 *
 * Neither one ever receives markup: a payload can contain a phone's `dc:title`, and §4.10
 * records what that does to a page that renders it as HTML.
 */

const { el, replaceChildren } = __dom;
const { fmtStamp } = __bridge;

const MAX_FEED_LINES = 200;

/** `{ value: null }` slots so the feed's own writes can skip a no-op re-render. */
const feed = {
  entries: [],
  render: null,
};

/**
 * Mount the feed. Called once by the shell; `selector` is the `<pre>` that shows it.
 *
 * The feed keeps its own bounded buffer rather than reading back out of the DOM: a `<pre>`
 * truncated from the top cannot be appended to without reading it, and reading it back is
 * the O(n) we are trying to avoid on a hot event stream.
 */
function mountFeed(container) {
  feed.render = () => {
    // Newest first, because the thing that just happened is the thing being looked for.
    // Joined by a newline in one text node: 200 rows as separate nodes makes the pane
    // reflow on every tick.
    replaceChildren(container, document.createTextNode(feed.entries.join('\n')));
  };
  feed.render();
}

/**
 * Append one line to the feed.
 *
 * `tone` only affects the toast; the feed stays monochrome on purpose, because colouring
 * 200 historical lines by severity turns the one thing that needs attention into a
 * pattern the eye learns to skip.
 */
function note(text, tone = '') {
  feed.entries.unshift(`[${fmtStamp()}] ${text}`);
  if (feed.entries.length > MAX_FEED_LINES) feed.entries.length = MAX_FEED_LINES;
  feed.render?.();
  // The rail shows the newest line, so a note is never invisible just because the admin
  // half is not the one on screen.
  const rail = document.getElementById('last-event');
  if (rail) rail.textContent = text;
  if (tone) toast(text, tone);
}

/**
 * The one-line confirmation.
 *
 * Lives in `#toast` and is replaced rather than queued: two clicks in a row should show
 * the second answer, not the first one again. A timer clears it, so a stale "已保存" can
 * never be mistaken for a fresh one.
 */
let toastTimer = null;

/**
 * The toast element, created on first use and kept.
 *
 * Created here rather than shipped in the markup because it is a *behaviour*, not a piece
 * of layout: a page that declares a toast container it never fills is a page with a dead
 * id in it, and the id test in `host.rs` counts those.
 */
let toastBox = null;

function ensureToast() {
  if (toastBox && document.body.contains(toastBox)) return toastBox;
  toastBox = el('p', {
    id: 'toast',
    className: 'notice',
    role: 'status',
    hidden: true,
    style: { position: 'fixed', left: '50%', bottom: '24px', transform: 'translateX(-50%)',
             'z-index': '10', 'max-width': 'min(680px, 90vw)', 'box-shadow': '0 8px 24px rgba(0,0,0,0.28)' },
  });
  document.body.append(toastBox);
  return toastBox;
}

function toast(text, tone = 'ok') {
  const box = ensureToast();
  box.dataset.tone = tone;
  box.textContent = text;
  box.hidden = false;
  clearTimeout(toastTimer);
  toastTimer = setTimeout(() => {
    box.hidden = true;
  }, tone === 'bad' ? 8000 : 4000);
}

/** Record a command's outcome in one line, using the back end's own words. */
function noteResult(label, result) {
  if (result.ok) {
    note(`${label} → ${JSON.stringify(result.data)}`);
    return;
  }
  note(`${label} 失败：${result.error}`, 'bad');
}

/**
 * Record an answer that carries `{code, message}` — the shape most management routes use.
 *
 * `code === 0` is success and everything else is a refusal with a sentence; the sentence
 * is the back end's, so it goes to the feed unchanged rather than being re-worded here.
 */
function noteAnswer(label, answer) {
  if (answer && answer.code === 0) {
    note(`${label} → ${answer.message ?? 'success'}`);
    toast(answer.message ?? '已生效');
    return;
  }
  const text = answer?.message ?? '没有答复';
  note(`${label} → ${text}`, 'bad');
}

return { mountFeed, note, toast, noteResult, noteAnswer };
})();

/* ---- user/now-playing.js ---- */
const __now_playing = (() => {
/*
 * **What is playing** — the top card of the user view, and the only thing on this page a
 * person looks at while casting something.
 *
 * ## One read, five readouts
 *
 * Everything here comes from a single `player_snapshot` document, refreshed on a poll. The
 * card never caches a second copy of any field: `transport_state`, `position`, `title` and
 * the playlist are all straight out of that one answer, which is what keeps this card from
 * disagreeing with the history card below it.
 *
 * ## Three states that look the same on screen and are not
 *
 * * `running: false` — no player process. The controls are disabled, not hidden: a control
 *   that vanishes reads as a missing feature.
 * * `settled: false` on a control - the command reached the player but the state table did
 *   not confirm it within the wait. On an idle player the reference writes nothing at all
 *   (`mpv.py:273-300`), so this is *normal*; the card says so rather than painting it red.
 * * `media_live: null` — the sender declared nothing about seekability. `null` is not
 *   `false`: it means "no opinion", and this port keeps the three-valued answer all the way
 *   to the player's command line.
 */

const { call, read, fmtClock } = __bridge;
const { el, byId, setText, setChip, replaceChildren } = __dom;
const { note, noteResult, noteAnswer } = __feed;

/** `{ value }` slots for the two polled lists, so a rebuild only happens on a change. */
const historySlot = { value: null };

/** What the volume button will send next; it alternates so a second press is visible. */
let nextVolume = 80;

/**
 * Render the "is it live" chip.
 *
 * The three values are the module's whole reason for existing: `true` is a live stream
 * (the player must not buffer ahead), `false` is a seekable file, and `null` is "the
 * sender said nothing" — which must *not* be rendered as "file", because a live stream
 * mis-declared as seekable loses its play-forward behaviour and looks like a broken video.
 */
function liveness(live) {
  if (live === true) return { tone: 'accent', text: '直播流（不许预读）' };
  if (live === false) return { tone: 'ok', text: '可拖动' };
  return { tone: '', text: '发送端没声明' };
}

/** The progress bar. No duration means no fill — a bar that never moves reads as stuck. */
function renderProgress(position, duration) {
  const bar = byId('progress-fill');
  const pos = Number(position);
  const total = Number(duration);
  const known = Number.isFinite(pos) && Number.isFinite(total) && total > 0;
  bar.style.width = known ? `${Math.min(100, Math.max(0, (pos / total) * 100)).toFixed(2)}%` : '0';
  setText('progress-left', fmtClock(position));
  setText('progress-right', Number.isFinite(total) && total > 0 ? fmtClock(total) : '—');
}

/**
 * The history list.
 *
 * Every row's title came from the sender (a DIDL `dc:title`), so the whole list is built
 * with `textContent` — the same boundary as the log pane (§4.10).
 *
 * Re-casting sends **the stored title together with the address**. Sending the address
 * alone lets the back end derive a name from the URL, and a relay address ends in
 * `/relay/<id>/media`, so the player window would read `media` — §4.2 records that exact
 * symptom ("改播放器的地址，就必须改播放器的名字").
 */
function renderHistory(rows, onCast) {
  const list = rows ?? [];
  const signature = JSON.stringify(list);
  if (historySlot.value === signature) return;
  historySlot.value = signature;

  const box = byId('history');
  if (!list.length) {
    replaceChildren(box, el('p', {
      className: 'empty',
      textContent: '还没有投过。用上面的卡片投一个地址，它就会留在这里。',
    }));
    return;
  }
  // **A div with a button inside, not a button wrapping spans.** A `<button>` may not contain
  // another `<button>`, and when the inner 重投 exists the browser reparents it — the chip
  // then lands in the row's first grid cell and covers the title. Measured in a real browser
  // on 2026-10-21; the row keeps its click-to-recast behaviour through the inner button, and
  // the whole row stays keyboard reachable because that button is a real one.
  replaceChildren(box, list.map((row) => el('div', { className: 'rowitem' }, [
    el('span', { className: 'rowitem__title', textContent: row.title || '（没有名字）' }),
    el('span', { className: 'rowitem__meta', textContent: row.url }),
    el('button', {
      className: 'rowitem__go btn btn--sm',
      type: 'button',
      textContent: '重投',
      title: '重新投这个地址（带回当时那个名字）',
      onclick: () => onCast(row),
    }),
  ])));
}

/**
 * Paint one `player_snapshot`.
 *
 * Exported because the shell's poll calls it and the control buttons re-paint after their
 * own action — one function, so a click cannot leave a stale readout behind.
 */
function paintSnapshot(snapshot) {
  if (!snapshot) return;
  const s = snapshot;

  setChip('player-chip', s.running
    ? { tone: 'ok', text: '播放器在跑' }
    : { tone: 'warn', text: '播放器没起来' });

  setText('title', s.title || '（没有在播的东西）');
  setText('current_url', s.current_url || '—');
  setChip('live-chip', liveness(s.media_live));
  setText('transport_state', s.transport_state || '—');
  renderProgress(s.position, s.duration);

  setText('ipc', s.ipc_running
    ? `已连接${s.ipc_once_connected ? '' : '（这次刚连上）'}`
    : (s.ipc_once_connected ? '连过，现在断了' : '没连上过'));
  setText('pid', s.pid === null || s.pid === undefined ? '—' : String(s.pid));
  setText('mpv_path', s.mpv_path || '—');
  setText('socket_path', s.socket_path || '—');
  setText('playlist', s.playlist?.length ? `${s.playlist.length} 项` : '空');

  const idle = !s.running;
  for (const id of ['pause', 'resume', 'stop', 'volume']) {
    const node = document.getElementById(id);
    if (node) node.disabled = idle;
  }

  renderHistory(s.history, recast);
}

/**
 * One control's answer.
 *
 * The distinction the card has to keep: `settled: false` means the state table did not
 * confirm within the wait, **not** that the command failed. Both render as a note, with
 * different wording, because an idle player legitimately produces the first.
 */
function paintControl(label, result) {
  if (!result.ok) {
    noteResult(label, result);
    return;
  }
  const answered = result.data;
  if (answered && answered.settled === false) {
    note(`${label} → 没等到播放器认账（空闲播放器上参考实现本来就不写这一格）`, 'warn');
    return;
  }
  note(`${label} → ${JSON.stringify(answered)}`);
}

/** Re-cast a history row, name included. */
async function recast(row) {
  const result = await read('cast_url', { uri: row.url, title: row.title || null });
  if (result) note(`重投 → 已交给播放器：${row.title || row.url}`, 'ok');
  else note('重投失败：后端没有答复', 'bad');
  await refresh();
}

/** Re-read the snapshot and repaint. Safe to call from any handler. */
async function refresh() {
  paintSnapshot(await read('player_snapshot'));
}

/** Wire the controls. Called once, by the shell, when this view mounts. */
function mount() {
  byId('pause').addEventListener('click', async () => {
    paintControl('pause', await call('pause'));
    await refresh();
  });
  byId('resume').addEventListener('click', async () => {
    paintControl('resume', await call('resume'));
    await refresh();
  });
  byId('stop').addEventListener('click', async () => {
    paintControl('stop', await call('stop_playback'));
    await refresh();
  });

  // The button alternates 80 / 60 on purpose: sending the same number twice can be
  // answered `settled: true` for a value the table already held, i.e. a confirmation of a
  // change that did not happen.
  const volume = byId('volume');
  volume.addEventListener('click', async () => {
    const value = nextVolume;
    nextVolume = value === 80 ? 60 : 80;
    volume.textContent = `音量 ${nextVolume}`;
    paintControl(`音量 ${value}`, await call('set_volume', { value }));
    await refresh();
  });

  // **Subtitle visibility is not part of the state table.** `set-subtitle-show` publishes to
  // the player and answers success whether or not a player is running; the readout changes
  // because mpv reports the property back, not because this command wrote it. So an idle
  // player is expected to answer `success` and change nothing, and the note says so.
  for (const [id, value] of [['sub-show', '1'], ['sub-hide', '0']]) {
    byId(id).addEventListener('click', async () => {
      const result = await call('set_subtitle_show', { value });
      if (!result.ok) {
        noteResult(`字幕 ${value}`, result);
        return;
      }
      note(`字幕 ${value === '1' ? '显示' : '隐藏'} → ${result.data?.message ?? 'success'}`
        + '（空闲播放器上不会变：那一格是播放器回报的）', 'ok');
    });
  }

  // This build ships one renderer, so the only accepted name is the one already in force.
  // Everything else gets the reference's `未找到播放器：{title}`, which is the back end's
  // sentence and is quoted rather than pre-empted here.
  byId('renderer-set').addEventListener('click', async () => {
    const result = await call('set_renderer', { title: 'MPV' });
    if (!result.ok) {
      noteResult('渲染器', result);
      return;
    }
    noteAnswer('渲染器', result.data);
  });

  byId('clear-history').addEventListener('click', async () => {
    const answer = await read('clear_play_history');
    note(answer ? '已清空播放历史' : '清空历史失败：后端没有答复', answer ? 'ok' : 'bad');
    await refresh();
  });
}

return { paintSnapshot, refresh, mount };
})();

/* ---- user/casting.js ---- */
const __casting = (() => {
/*
 * Casting: a direct media address, or a web page parsed into candidates.
 *
 * ## Why these are two cards and not one form
 *
 * They answer different questions. "Play this URL" is one step and the answer is a toast.
 * "Parse this page" is many steps, takes seconds, produces a *list*, and can end in four
 * different "nothing to play" sentences that each mean a different next action. Putting
 * them behind one input would make the fast path wait on the slow path's vocabulary.
 *
 * ## The one rule both share
 *
 * **The page's own address never leaves this machine.** A parsed candidate is served from
 * `/relay/<random id>/media` on this host; the signed, short-lived, header-hungry URL the
 * page carried is only ever used by the relay. That is why the candidate rows show the
 * media label and the relay's own reason sentence rather than the source URL.
 *
 * ## Two empties, two sentences
 *
 * `resolve_status` answers two different "nothing": an unknown job (`code 1`, `status:
 * null`) and a job that is still running. Rendering the first as "still parsing" leaves a
 * card spinning until the job's TTL expires, so the two are handled apart.
 */

const { call, read, fmtSeconds } = __bridge;
const { el, byId, setText, replaceChildren, emptyState } = __dom;
const { note, noteResult, toast } = __feed;

/**
 * Which browser/OS cookie-source failure this is, and what to do about it.
 *
 * The vocabulary has one owner — `macast_cast::resolve::COOKIE_PROBLEMS` — and `host.rs`
 * asserts that this table has exactly those four keys and no fifth. The rule being kept is
 * `AGENTS.md` §4.14's: four different failures, four different actions. One generic
 * sentence would send three of the four readers to fix something that is not broken.
 */
const COOKIE_NEXT_STEP = {
  not_found: '这句点的是本机这一侧，不是站点要不要登录：我们按那个目录去找 cookie 库，里面没有 '
    + 'Cookies 文件。在「账号与登录态」里写成「名字:资料目录」（指到那个浏览器真正存 cookie 的'
    + '目录），或者改用粘贴导出的 cookie 文件。',
  locked: '这句点的是本机这一侧：那个浏览器正开着，cookie 库被它锁住了，我们复制不出来。'
    + '完全退出那个浏览器（不是关掉窗口）再解析一次，或者改用粘贴导出的 cookie 文件。',
  decrypt: '这句点的是本机这一侧：cookie 库读到了，但解密它的钥匙不在手边（钥匙串／密钥环没解锁）。'
    + '解锁它再解析一次；或者改用粘贴导出的 cookie 文件 —— 那条路不需要这把钥匙。',
  bad_profile: '这句点的是本机这一侧：那个浏览器不接受「:资料目录」这一半写法。'
    + '把那一栏改成只写浏览器名字，冒号后面那半截去掉。',
};

/** The job this card is showing, and the job the poll is watching — two variables, on purpose. */
const state = {
  /** Which job the poller should keep asking about; `null` stops polling. */
  polling: null,
  /**
   * Which job the **candidate list** belongs to.
   *
   * Kept apart from `polling` because the poll clears its slot when it finishes, and the
   * cast button reads this one. With a single variable, clicking 投 on a completed card
   * sent `job: null` — measured on the real instance — and the back end answered `success`
   * because that path only reads the candidate list. Nothing on screen looked wrong, which
   * is why the two names are separate rather than one clever one.
   */
  finished: null,
  timer: null,
  candidates: [],
};

function stopPolling() {
  clearTimeout(state.timer);
  state.timer = null;
  state.polling = null;
}

/**
 * The explanation block: the engine's own words, the cookie next step, and the hidden
 * silent rows.
 *
 * The empty-result sentence is the important one. `ResolveJob::why_empty` produces four
 * sentences — the engine does not know the site / the engine said something else / this
 * machine has no yt-dlp / the engine ran and returned nothing (a JavaScript hole) — and
 * they differ in *what the user does next*. So the sentence is shown on the card, at the
 * moment there is nothing to list, rather than only in the event feed.
 */
function renderNote(status) {
  const box = byId('resolve-note');
  const parts = [];

  if (status.done && !(status.candidates ?? []).length && status.error) {
    parts.push(el('p', { className: 'notice', dataset: { tone: 'warn' }, textContent: status.error }));
  }
  // The job's own remark (`note`): what the back end wants said about *this* parse, e.g.
  // which address the share text contained when it had to dig one out. Distinct from
  // `engine_note`, which is the engine's raw stderr line.
  if (status.note) {
    parts.push(el('p', { className: 'card__hint', textContent: status.note }));
  }
  if (status.engine_note && !status.from_ytdlp) {
    parts.push(el('p', { className: 'notice', textContent: `yt-dlp 这一路说：${status.engine_note}` }));
  }
  if (status.cookie_problem) {
    parts.push(el('p', {
      className: 'notice',
      dataset: { tone: 'warn' },
      textContent: COOKIE_NEXT_STEP[status.cookie_problem] ?? status.cookie_problem,
    }));
  } else if (status.needs_cookies) {
    // Only reachable when the failure is *not* on our side: the back end's `status()` has
    // already inverted this once (`needs_cookies(note) and not cookie_source_problem(note)`),
    // and a second inversion here would be two owners for one judgement.
    parts.push(el('p', {
      className: 'notice',
      textContent: '这通常是站点要会话 cookie —— 用「账号与登录态」那张卡给它凭据。',
    }));
  }
  if (status.hidden_silent) {
    parts.push(el('p', {
      className: 'card__hint',
      textContent: `另外 ${status.hidden_silent} 条只有画面、源站又没交回可合并的音轨，`
        + '投上去必然没声音，所以这里不列它们。',
    }));
  }
  replaceChildren(box, parts);
}

/** One candidate row: what it is, why it can be served, and the button that casts it. */
function candidateRow(candidate, index) {
  const bits = [
    candidate.label,
    candidate.container,
    candidate.width && candidate.height ? `${candidate.width}×${candidate.height}` : '',
    fmtSeconds(candidate.duration),
    candidate.mode,
    candidate.video_codec && (candidate.audio_codec
      ? `${candidate.video_codec}+${candidate.audio_codec}`
      : candidate.video_codec),
    candidate.merged ? '音画合并' : '',
    candidate.last_resort ? '未度量' : '',
  ].filter(Boolean);

  return el('div', { className: 'rowitem' }, [
    el('span', { className: 'rowitem__title', textContent: candidate.title || '（没有名字）' }),
    el('span', { className: 'rowitem__meta', textContent: bits.join(' · ') }),
    // `relay` is the back end's own sentence for *why* this row is servable ("反向转发" vs
    // "转封装：写完之前没有可信长度"). It has one owner (`relay_reason`); the page must not
    // compose a synonym.
    el('span', { className: 'rowitem__why', textContent: candidate.relay || '' }),
    el('button', {
      className: 'rowitem__go btn btn--sm',
      type: 'button',
      textContent: `投 ${index + 1}`,
      onclick: () => castCandidate(index),
    }),
  ]);
}

function renderCandidates(candidates) {
  state.candidates = candidates ?? [];
  const box = byId('resolve-rows');
  if (!state.candidates.length) {
    replaceChildren(box, el('p', {
      className: 'empty',
      textContent: '这次没有可用候选。上面那句话说的是哪一种「没有」。',
    }));
    return;
  }
  replaceChildren(box, state.candidates.map(candidateRow));
}

/** Cast one candidate at the chosen target. */
async function castCandidate(index) {
  const target = byId('cast-target').value || 'local';
  const result = await call('cast_resolved', {
    job: state.finished,
    candidate: String(index),
    target,
  });
  if (!result.ok) {
    noteResult('投屏', result);
    return;
  }
  const answer = result.data ?? {};
  if (answer.code === 0) {
    note(`已交给「${target === 'local' ? '本机' : target}」`, 'ok');
  } else {
    note(`投屏被拒：${answer.message ?? '没有答复'}`, 'bad');
  }
}

/** Ask once about a job, and keep asking while it is running. */
async function poll() {
  state.timer = null;
  const job = state.polling;
  if (!job) return;

  const doc = await read('resolve_status', { job });
  if (!doc) {
    setText('resolve-meta', '轮询失败：后端没有答复');
    stopPolling();
    return;
  }
  // The other empty: no such job (finished, expired, or this process restarted). Treating
  // it as "still parsing" spins the card until the TTL runs out.
  if (doc.code !== 0 || !doc.status) {
    setText('resolve-meta', `${doc.message ?? '没有这个解析任务'}（已停止轮询）`);
    stopPolling();
    return;
  }

  const s = doc.status;
  const head = `${s.step_label || s.step} · ${fmtSeconds(s.seconds)}`
    + `（这一步 ${fmtSeconds(s.step_seconds)}）`
    + ` · 页面 ${s.scraped} · 引擎 ${s.from_ytdlp} · 已度量 ${s.measured}`
    + ` · 剔除 ${s.rejected} · 藏起无声 ${s.hidden_silent}`;
  setText('resolve-meta', s.done ? `${head} · 完成` : head);
  renderNote(s);
  renderCandidates(s.candidates);

  if (s.done) {
    stopPolling();
    // **The first device search starts here, not when the page opens.** An SSDP M-SEARCH
    // plus an mDNS browse puts queries on the user's LAN, and someone who merely opened the
    // window never asked for that — the reference says the same in its own comment.
    loadTargets(true);
    return;
  }
  state.timer = setTimeout(poll, 700);
}

/**
 * The device dropdown.
 *
 * Its first row is **本机**, inserted by the back end (`cast_targets` → `_local_target`), not
 * by this page: a page that prepended its own would render two entries for one renderer,
 * both with `id: "local"`.
 *
 * `searching` is not a fault when it stays true: on a machine that discovers nothing, the
 * cold-start kick means every poll finds a sweep already in flight. The note says that
 * out loud, because "搜索还在跑" reads like "stuck" otherwise.
 */
async function loadTargets(refresh = false) {
  const doc = await read('cast_targets', { refresh: Boolean(refresh) });
  if (!doc) {
    setText('targets-note', '读设备列表失败：后端没有答复');
    return;
  }
  const view = doc.targets ?? {};
  const items = view.items ?? [];
  const select = byId('cast-target');
  const previous = select.value;
  select.replaceChildren(...items.map((row) => el('option', {
    value: row.id,
    textContent: row.kind === 'local' ? `本机 · ${row.name}` : `${row.name}${row.address ? ` · ${row.address}` : ''}`,
  })));
  select.value = items.some((row) => row.id === previous) ? previous : 'local';

  const only = items.length <= 1;
  setText('targets-note', [
    `${items.length} 行（含本机）`,
    view.searching ? '搜索还在跑' : '搜索已停',
    view.known ? '' : '还没有拿到过完整一轮结果',
    only && view.searching
      ? '只有本机：这个网络里没有应答的接收端（没有 Chromecast 或电视，或者不在同一网段）—— 搜不到不是故障，投本机仍然可用'
      : '',
  ].filter(Boolean).join(' · '));
}

/** Mount the casting cards. */
function mount() {
  byId('cast').addEventListener('click', async () => {
    const uri = byId('uri').value.trim();
    const title = byId('title-in').value.trim();
    if (!uri) {
      toast('没有地址，后端不会动', 'warn');
      return;
    }
    const result = await call('cast_url', { uri, title: title || null });
    noteResult('投地址', result);
    if (result.ok) byId('uri').value = '';
  });

  byId('resolve-go').addEventListener('click', async () => {
    const text = byId('page-url').value.trim();
    if (!text) {
      toast('没有地址，后端不会动', 'warn');
      return;
    }
    stopPolling();
    const result = await call('resolve_page', { text });
    if (!result.ok) {
      noteResult('解析', result);
      return;
    }
    const answer = result.data ?? {};
    if (answer.code !== 0) {
      setText('resolve-meta', answer.message ?? '解析被拒绝');
      renderCandidates([]);
      note(`解析被拒：${answer.message ?? ''}`, 'bad');
      return;
    }
    state.polling = answer.job;
    state.finished = answer.job;
    setText('resolve-meta', `已开始解析 ${answer.url}${answer.note ? `（${answer.note}）` : ''}`);
    replaceChildren(byId('resolve-note'), []);
    renderCandidates([]);
    poll();
  });

  byId('resolve-stop').addEventListener('click', () => {
    stopPolling();
    setText('resolve-meta', '已停止轮询（后台那一次解析仍会跑完，只是这一格不再看它）');
  });

  byId('targets-refresh').addEventListener('click', () => loadTargets(true));

  // The dropdown starts with only the local row's promise: no LAN query until a parse
  // finishes or the user asks for one.
  setText('targets-note', '还没搜过设备 —— 解析完成时会自动搜一轮，也可以按「重新搜索设备」。');
}

return { loadTargets, mount };
})();

/* ---- user/accounts.js ---- */
const __accounts = (() => {
/*
 * **Accounts and logins** — the cookie card.
 *
 * This is the one place in the window where a value must never be displayed, and the whole
 * module is shaped by that:
 *
 * * the back end answers **counts and a path**, never a cookie value (`_cookie_state`);
 * * this module never asks for anything else, and never renders what it was not given;
 * * the jar is a credential — 0600 on disk, and the settings JSON never holds it.
 *
 * So the readout says "已保存 · 3 条能读懂的", and the textarea the user pastes into is
 * cleared as soon as the back end accepts it. A page that echoed the paste back would keep
 * the credential in the DOM for the rest of the session.
 *
 * ## Why validation is the back end's job
 *
 * A malformed jar is not an error to yt-dlp: it resolves *anonymously* and returns fewer
 * addresses, which the user reads as "this page has no video". The refusal therefore has to
 * name how many lines were unreadable, and it happens before anything is written. This
 * module only refuses the empty paste, because that is a UI state and not a format question.
 */

const { call, read, fmtBytes } = __bridge;
const { byId, setText } = __dom;
const { note, noteAnswer, toast } = __feed;

/** The hint under the browser field, so the two refusals do not read as one. */
const BROWSER_GRAMMAR = '名字[:资料目录]。+钥匙串 与 ::容器 我们都不收 —— 那两样是从不替你挑的东西。';

/** Read the state and paint it. */
async function refresh() {
  const doc = await read('resolve_cookies');
  if (!doc) {
    setText('ck-jar', '读不到 cookie 状态：后端没有答复');
    return;
  }
  const s = doc.state ?? {};
  // `jar` is "is there a file", `cookies` is "how many lines we can read". Both are shown:
  // reporting only the second makes a saved-but-unreadable jar look identical to no jar.
  setText('ck-jar', s.jar
    ? `已保存 · ${s.cookies} 条能读懂的${s.bad ? ` · 另有 ${s.bad} 行看不懂（也一并存着）` : ''}`
    : '未保存（解析会匿名进行，需要登录的站点会少给地址）');
  setText('ck-browser', s.browser ? `${s.browser}（文件优先于浏览器）` : '关（不从浏览器读取）');
  setText('ck-max', `${fmtBytes(s.max_bytes)} · 超过就在粘贴那一刻拒绝`);
  setText('ck-engine', s.yt_dlp ? '本机有 yt-dlp' : '本机没有 yt-dlp —— 只会抓页面里写明的地址');

  const field = byId('ck-browser-in');
  // The poll does not clobber what the user is typing; an explicit save/reload does.
  if (!field.dataset.touched && s.browser) field.value = s.browser;
}

/** Mount the card. */
function mount() {
  const field = byId('ck-browser-in');
  field.addEventListener('input', () => {
    field.dataset.touched = '1';
  });

  byId('ck-save').addEventListener('click', async () => {
    const box = byId('ck-text');
    const text = box.value;
    if (!text.trim()) {
      toast('文本框是空的，后端不会动', 'warn');
      return;
    }
    const result = await call('save_cookies', { text });
    if (!result.ok) {
      note(`保存 cookie 失败：${result.error}`, 'bad');
      return;
    }
    noteAnswer('保存 cookie', result.data);
    // Only clear on success: a refused jar is text the user needs to fix.
    if (result.data?.code === 0) box.value = '';
    await refresh();
  });

  byId('ck-clear').addEventListener('click', async () => {
    const result = await call('clear_cookies');
    if (!result.ok) {
      note(`清除 cookie 失败：${result.error}`, 'bad');
      return;
    }
    noteAnswer('清除 cookie', result.data);
    await refresh();
  });

  byId('ck-browser-set').addEventListener('click', async () => {
    const result = await call('set_cookie_browser', { text: field.value });
    if (!result.ok) {
      note(`设置浏览器失败：${result.error}`, 'bad');
      return;
    }
    noteAnswer('从浏览器读 cookie', result.data);
    field.dataset.touched = '';
    await refresh();
  });

  byId('ck-browser-off').addEventListener('click', async () => {
    // "Off" is one of the six words, and it **removes** the key rather than storing false
    // (§4.2: a `get` would otherwise persist the literal into the table).
    const result = await call('set_cookie_browser', { text: 'off' });
    if (!result.ok) {
      note(`关闭浏览器读取失败：${result.error}`, 'bad');
      return;
    }
    noteAnswer('从浏览器读 cookie', result.data);
    field.value = '';
    field.dataset.touched = '';
    await refresh();
  });
}

/** Exported so the card can advertise its grammar where the user types, not in a manual. */
const browserGrammar = BROWSER_GRAMMAR;

return { refresh, mount, browserGrammar };
})();

/* ---- admin/services.js ---- */
const __services = (() => {
/*
 * **Services and the network** — the first card of the admin view.
 *
 * Three services, three lifecycles, and the readout has to keep them apart:
 *
 * | service | second question | why it matters |
 * |---|---|---|
 * | HTTP | `healthy` | the port can be listening while the worker is gone |
 * | SSDP | `healthy` | same |
 * | GENA | `thread_alive` | if the event thread is gone, subscribers never enter the table |
 *
 * `is_healthy()` answers **true for a deliberately stopped server** — a stopped server is
 * not a fault (`http.rs`, `ssdp.rs` both say so). So `healthy` alone is meaningless; the
 * three-state chip is `serviceState(running, healthy)` and it is the only place that
 * decides what the combination means.
 *
 * ## The interface picker
 *
 * Two facts only this side can say, both derived from the document rather than from a new
 * field:
 *
 * * **a pinned interface that is no longer present** — the back end's fallback is silent
 *   (`resolve_pin` logs a warning and answers `""`), so the page has to point out that the
 *   stored value is not what is in force;
 * * **a pinned interface with no usable IPv4** — same fallback, same silence.
 *
 * Unusable rows are listed rather than filtered out: an unplugged port is still a real
 * option, and hiding it makes the machine look like it does not have it.
 */

const { call, read, serviceState, threadState } = __bridge;
const { byId, setText, setChip } = __dom;
const { note, noteAnswer, toast } = __feed;

/** `{ value }` slots so the two polled controls are only rewritten when they change. */
const slots = { interfaces: null, nameTouched: false, ifaceTouched: false };

/**
 * The sentence for a pin that is not in force.
 *
 * Returns `''` when the pinned card is present and usable, in which case the note area
 * stays empty — a card that always says something is a card nobody reads.
 */
function pinNote(rows, current) {
  if (!current) return '';
  const row = (rows ?? []).find((r) => r.name === current);
  if (!row) {
    return `指定的网卡 ${current} 现在不在这台机器的表里：后端的回落是静默的（只往日志写一行 `
      + 'warning），发现其实已经改由默认路由决定，而你存的值没有被改写。';
  }
  if (!row.usable) {
    return `指定的网卡 ${current} 没有局域网可达的 IPv4（${row.reason || '原因见日志'}）：`
      + '后端的回落是静默的，发现已由默认路由决定，而你存的值没有被改写。';
  }
  return '';
}

/** Paint the interface select, keeping what the user is choosing if the list changed. */
function renderInterfaces(rows, current) {
  const list = rows ?? [];
  const signature = JSON.stringify([list.map((r) => [r.name, r.ip, r.usable, r.reason, r.default, r.selected]), current]);
  if (slots.interfaces === signature) return;
  slots.interfaces = signature;

  const select = byId('iface');
  const previous = select.value;
  select.replaceChildren(
    new Option('自动（跟随默认路由）', ''),
    ...list.map((row) => {
      const marks = [
        row.usable ? '' : '不可用',
        row.default ? '默认路由' : '',
        row.selected ? '已选中' : '',
      ].filter(Boolean).join(' · ');
      const text = `${row.name}  ${row.ip || '（没有 IPv4）'}`
        + `${row.reason ? ` ${row.reason}` : ''}${marks ? ` 【${marks}】` : ''}`;
      return new Option(text, row.name);
    }),
  );
  if (slots.ifaceTouched && list.some((row) => row.name === previous)) {
    select.value = previous;
    return;
  }
  select.value = current || '';
  if (select.value !== (current || '')) select.value = '';
}

/** One `service_snapshot`. */
function paint(document_) {
  if (!document_) return;
  const s = document_.server ?? {};
  const nw = document_.network ?? {};

  setChip('svc-http', serviceState(document_.http?.running, document_.http?.healthy));
  setChip('svc-ssdp', serviceState(document_.ssdp?.running, document_.ssdp?.healthy));
  setChip('svc-gena', threadState(document_.eventing?.running, document_.eventing?.thread_alive));

  // `bound_port` is reported on the row that owns it, once: the three services have three
  // lifecycles and `server.running` only speaks for the HTTP face.
  let port = String(s.port ?? '—');
  if (s.bound_port === null || s.bound_port === undefined) {
    port += '（还没有真的绑上）';
  } else if (s.bound_port !== s.port) {
    port += ` · 实际 ${s.bound_port}（${s.port} 被占用，按参考实现回退，USN 也跟着改了）`;
  }
  setText('svc-port', port);
  setText('svc-https', s.https_enabled ? `已启用 · ${s.https_port}` : '未启用');
  setText('svc-advertised', nw.advertised?.length
    ? nw.advertised.join(' / ')
    : '（读不到可广播的地址：手机看得到我们也投不上）');
  setText('svc-protocol', s.protocol?.length ? s.protocol.join(' · ') : '（没有启用任何协议）');
  setText('svc-renderer', s.renderer || '（默认 mpv）');
  setText('svc-system', `${s.version} · ${s.system} ${s.system_version}（${s.platform}）`);

  const name = byId('friendly-name');
  if (!slots.nameTouched) name.value = s.friendly_name || '';

  renderInterfaces(nw.interfaces, nw.interface);
  const pin = pinNote(nw.interfaces, nw.interface);
  setText('cfg-note', s.service_stopped ? `服务已被停用：${pin}` : pin);

  selectPainted(s.protocol ?? []);
}

/**
 * The protocol toggles.
 *
 * Shown as two switches rather than a checklist because the back end's write path is
 * per-title: `toggle-protocol` flips one and refuses to leave the list empty. A checklist
 * submitting the whole set would be a second implementation of that refusal.
 */
function selectPainted(active) {
  for (const button of document.querySelectorAll('[data-protocol]')) {
    const title = button.dataset.protocol;
    const on = active.includes(title);
    button.dataset.on = on ? 'true' : 'false';
    button.textContent = `${on ? '停用' : '启用'} ${title}`;
    button.setAttribute('aria-pressed', on ? 'true' : 'false');
  }
}

/** Read and paint. */
async function refresh() {
  paint(await read('service_snapshot'));
}

/** Mount the card. */
function mount() {
  byId('friendly-name').addEventListener('input', () => {
    slots.nameTouched = true;
  });
  byId('iface').addEventListener('change', () => {
    slots.ifaceTouched = true;
  });

  // The stored name keeps whatever the user typed, trailing space included: the reference
  // trims interface names and never trims the friendly name. But the description XML is
  // baked at construction, so `GET /description.xml` only follows after a restart — the
  // back end says so in `restart_pending`, and this note must repeat it, or the user reads
  // "I changed it and nothing happened, the UI is broken".
  byId('save-name').addEventListener('click', async () => {
    const result = await call('set_friendly_name', { name: byId('friendly-name').value });
    if (!result.ok) {
      note(`保存设备名失败：${result.error}`, 'bad');
      return;
    }
    const r = result.data ?? {};
    if (r.code === 0) {
      slots.nameTouched = false;
      note(`设备名已存为「${r.saved}」· 读回「${r.friendly_name}」`
        + `· 描述文件要到下次重启才跟着变（restart_pending=${r.restart_pending}）`, 'ok');
    } else {
      note(`保存设备名：${r.message ?? '被拒绝'}`, 'bad');
    }
    await refresh();
  });

  // An unknown interface name is refused by the **back end**, not here: a typo would
  // otherwise fall back to loopback silently, which looks like "configured, but the device
  // cannot be found". So the refusal is quoted rather than pre-empted.
  byId('save-iface').addEventListener('click', async () => {
    const result = await call('set_network_interface', { name: byId('iface').value });
    if (!result.ok) {
      note(`切换网卡失败：${result.error}`, 'bad');
      return;
    }
    const r = result.data ?? {};
    if (r.code === 0) {
      slots.ifaceTouched = false;
      note(`${r.message}${r.ssdp_reannounced
        ? ' · SSDP 已重新通告'
        : ' · SSDP 当前没在跑，所以这次没有重通告任何东西'}`, 'ok');
      await refresh();
    } else {
      note(r.message ?? '切换被拒绝', 'bad');
    }
  });

  for (const button of document.querySelectorAll('[data-protocol]')) {
    button.addEventListener('click', async () => {
      const answer = await read('toggle_protocol', { title: button.dataset.protocol });
      if (answer) noteAnswer(`协议开关 ${button.dataset.protocol}`, answer);
      else toast('协议开关失败：后端没有答复', 'bad');
      await refresh();
    });
  }
}

return { paint, refresh, mount };
})();

/* ---- admin/logs.js ---- */
const __logs = (() => {
/*
 * **Logs** — the module chooser, the tail reader, and the clearer.
 *
 * ## Two keys, not three
 *
 * `log_modules()` answers `{main, modules}` — the main `macast.log`'s size, and the module
 * files on disk. That is the reference's shape (`protocol.py:2120-2128` +
 * `logsplit.list_logs()`), produced by one builder both the HTTP face and this window call.
 *
 * This panel once read a third key, `known` ("on disk ∪ already claimed"), which nothing
 * ever sent: `(m.known || [])` was permanently empty, so the loop that was supposed to list
 * "claimed but not yet written" logs never ran once — and nothing failed, because reading a
 * missing key in JavaScript is `undefined`. The branch was deleted rather than the back end
 * being given a field only our own page reads.
 *
 * ## The log body is untrusted text
 *
 * It contains whatever a sender put in a DIDL `dc:title` and whatever URL it handed us.
 * `AGENTS.md` §4.10 records what that does to a page that renders it as HTML. So the pane is
 * one `textContent` write — never `innerHTML`, never a per-line element with markup.
 *
 * ## What "truncated" means here
 *
 * The tail read is bounded by a **byte window**, not by a line ceiling. Asking for more
 * lines grows the window; the sentence has to say "the byte window did not fit" rather than
 * "at most N lines", because the former is what the code does.
 */

const { read, fmtBytes } = __bridge;
const { byId, setText } = __dom;
const { note, noteAnswer, toast } = __feed;

/** `{ value }` slot for the module list, so the select is only rebuilt on a change. */
const slot = { modules: null };

/** Paint the module chooser from a `log_modules` document. */
function paintModules(doc) {
  const onDisk = doc?.modules ?? [];
  const signature = JSON.stringify([doc?.main, onDisk]);
  if (slot.modules === signature) return;
  slot.modules = signature;

  const select = byId('log-module');
  const previous = select.value;
  select.replaceChildren(
    new Option(`整体日志 · ${fmtBytes(doc?.main?.size)}`, ''),
    ...onDisk.map((row) => new Option(`${row.name} · ${fmtBytes(row.size)}`, row.name)),
  );
  select.value = [...select.options].some((o) => o.value === previous) ? previous : '';
}

/** Read the chooser's current selection. */
function selection() {
  return { module: byId('log-module').value || null, tail: Number(byId('log-tail').value) };
}

/**
 * Read one tail.
 *
 * `all` asks for the whole file. The line count is **not** clamped here: `read_tail` clamps
 * to its own ceiling, and repeating that ceiling in the page is a second source of truth
 * that drifts the moment the back end moves it.
 */
async function readTail(all = false) {
  const { module, tail } = selection();
  const args = { module, all: all || null };
  if (!all && Number.isFinite(tail) && tail >= 1) args.tail = Math.floor(tail);

  const doc = await read('log_view', args);
  if (!doc) {
    setText('log-text', '读取失败：后端没有答复');
    setText('log-meta', '');
    return;
  }
  if (doc.code !== 0) {
    setText('log-text', '（没有这一份日志）');
    setText('log-meta', `${doc.message}：${doc.module}`);
    return;
  }

  setText('log-text', doc.logs || '（空的）');
  const which = doc.module ? `「${doc.module}」` : '整体日志';
  const head = all
    ? `${which} · 全文 ${fmtBytes(doc.size)}，读到 ${doc.lines} 行`
    : `${which} · 要 ${args.tail ?? '默认'} 行，读到 ${doc.lines} 行，文件 ${fmtBytes(doc.size)}`;
  setText('log-meta', doc.truncated
    ? `${head} · 字节窗口装不下，只给了末尾这一段（把行数调大就看得更多）`
    : `${head} · 没被截断`);
}

/** Re-read the module list. */
async function loadModules() {
  const doc = await read('log_modules');
  if (doc) paintModules(doc);
}

/** Mount the card. */
function mount() {
  byId('log-read').addEventListener('click', () => readTail(false));
  byId('log-all').addEventListener('click', () => readTail(true));

  // `clear_log_file` counts the files it emptied or removed, and the log itself is one of
  // them — so the backup count is `removed − 1`. The page says that number rather than a
  // bare "cleared", because "3" and "cleared one file" are different news.
  byId('log-clear').addEventListener('click', async () => {
    const { module } = selection();
    const result = await read('clear_log', { module });
    if (!result) {
      toast('清空日志失败：后端没有答复', 'bad');
      return;
    }
    if (result.code === 0) {
      const backups = Math.max(0, (result.removed ?? 0) - 1);
      note(`已清空${module ? `「${module}」` : '整体日志'}`
        + `${backups ? `，连同 ${backups} 个轮转备份` : '（没有轮转备份）'}`, 'ok');
    } else {
      noteAnswer('清空日志', result);
    }
    await loadModules();
    await readTail(false);
  });
}

return { paintModules, readTail, loadModules, mount };
})();

/* ---- admin/settings.js ---- */
const __settings = (() => {
/*
 * **Module settings** — every persisted key, grouped by the module that owns it.
 *
 * The grouping is not cosmetic: it answers "whose key is this", which is the question a
 * user has when they see a name like `PlayerHW_Disable` in a JSON file. The ownership table
 * lives in `macast_core::module_settings` (one owner) and the read route and the write route
 * both consult it, so a key this card renders as editable is a key the writer will accept.
 *
 * Two shapes, and the difference is the back end's decision, not this module's:
 *
 * * `editable: true` — a scalar. Rendered as an input plus save; the value is sent as the
 *   text the user typed and the back end type-guesses it (`set_value`).
 * * `editable: false` — a list or dict, or a key this build has no writer for. Rendered
 *   read-only, with a pointer to the JSON editor rather than a dead input.
 */

const { call, read } = __bridge;
const { el, byId, replaceChildren, emptyState, notice: noticeNode } = __dom;
const { note, noteAnswer } = __feed;

/** One editable setting: label, key, hint, input, save. */
function editableSetting(item) {
  const input = el('input', {
    className: 'input',
    value: item.present ? (typeof item.value === 'string' ? item.value : JSON.stringify(item.value)) : '',
    placeholder: item.type || '字符串',
  });
  return el('div', { className: 'setting' }, [
    el('div', { className: 'setting__what' }, [
      el('span', { className: 'setting__label', textContent: item.label }),
      el('span', { className: 'setting__key', textContent: item.key }),
      item.hint ? el('span', { className: 'setting__hint', textContent: item.hint }) : null,
    ]),
    input,
    el('button', {
      className: 'btn btn--sm',
      type: 'button',
      textContent: '保存',
      onclick: async () => {
        const answer = await read('set_module_setting', {
          key: item.key,
          value: input.value,
          remove: false,
        });
        if (answer) noteAnswer(item.key, answer);
        else note(`${item.key} 保存失败：后端没有答复`, 'bad');
        await refresh();
      },
    }),
  ]);
}

/** One read-only setting: same first column, the value in monospace. */
function readonlySetting(item) {
  return el('div', { className: 'setting setting--ro' }, [
    el('div', { className: 'setting__what' }, [
      el('span', { className: 'setting__label', textContent: item.label }),
      el('span', { className: 'setting__key', textContent: item.key }),
      item.hint ? el('span', { className: 'setting__hint', textContent: item.hint }) : null,
    ]),
    el('code', {
      className: 'setting__value',
      textContent: item.present
        ? (typeof item.value === 'string' ? item.value : JSON.stringify(item.value))
        : '（未设置）',
    }),
    el('span'),
  ]);
}

/** Paint one `module_settings_snapshot`. */
function paint(doc) {
  const box = byId('mods');
  const groups = doc?.groups ?? [];
  if (!groups.length) {
    replaceChildren(box, emptyState('这台机器上还没有任何可显示的配置键。'));
    return;
  }
  const parts = [];
  for (const group of groups) {
    parts.push(el('h3', { className: 'settingsgroup__title', textContent: group.title }));
    const rows = (group.keys ?? []).map((item) => (item.editable ? editableSetting(item) : readonlySetting(item)));
    parts.push(el('div', { className: 'settingsgroup' }, rows));
  }
  replaceChildren(box, parts);
}

/** Read and paint. */
async function refresh() {
  paint(await read('module_settings_snapshot'));
}

/** Mount the card. */
function mount() {
  // The card is read on first mount; the shell does not poll it, because settings change
  // only when someone on this machine changes them and every writer refreshes on its own.
  refresh().catch(() => {});
}

/**
 * The **advanced settings** editor: the whole table as JSON.
 *
 * `save-launch-param` replaces the file and restarts the process, so "read first, then
 * edit" is not a convenience — it is the only way to change one key without deleting the
 * other thirty-five. The page refuses only the empty box; whether the text is JSON at all is
 * the back end's judgement, and it has its own sentence for that (`json format error`).
 */
function mountAdvanced() {
  byId('lp-load').addEventListener('click', async () => {
    const doc = await read('launch_param');
    if (!doc) {
      note('读取整份设置失败：后端没有答复', 'bad');
      return;
    }
    // `launch-param` answers the table itself — no `code`, no `message` — so this is a
    // straight serialisation rather than a document to unpack.
    byId('lp-text').value = JSON.stringify(doc, null, 4);
    note(`已读入 ${Object.keys(doc).length} 个键；提交会整表替换并重启`, 'ok');
  });

  byId('lp-save').addEventListener('click', async () => {
    const text = byId('lp-text').value;
    if (!text.trim()) {
      note('高级设置：文本框是空的，后端不会动', 'warn');
      return;
    }
    const result = await call('save_launch_param', { value: text });
    if (!result.ok) {
      note(`写入失败：${result.error}`, 'bad');
      return;
    }
    const answer = result.data ?? {};
    noteAnswer('高级设置', answer);
    if (answer.code === 0) {
      replaceChildren(byId('lp-note'), [
        noticeNode('进程正在重启 —— 这个窗口会在新进程里重新加载。', 'ok'),
      ]);
    }
  });

  // The hint that this editor is where hooks are configured. Written as a note rather than
  // an input, because four free-text shell commands deserve a paragraph, not a field row.
  replaceChildren(byId('lp-note'), [
    noticeNode(
      '自动化钩子也在这里：Hook_On_Cast / Hook_On_Pause / Hook_On_Resume / Hook_On_Stop，'
      + '值是一条 shell 命令。它会拿到 MACAST_EVENT、MACAST_URL、MACAST_TITLE 三个环境变量 —— '
      + '地址只走环境变量，绝不拼进命令（拼进去就等于让任何投屏的网页在这台机器上执行命令）。',
    ),
  ]);
}

return { paint, refresh, mount, mountAdvanced };
})();

/* ---- admin/diagnostics.js ---- */
const __diagnostics = (() => {
/*
 * **Subscribers and the danger zone** — the two cards that exist to explain a failure.
 *
 * ## Three counters, three different deaths
 *
 * `pending_append` non-zero means the connection was accepted and the client never entered
 * the table — which is *not* the same as "nobody subscribed". `pending_state` means state
 * changes are queued and not going out. `pending_remove` means the reaper is not draining.
 * The third one is not in the reference at all, because the reference has no witness for it;
 * this port added it because "a stuck subscriber never leaves" needs an owner.
 *
 * A counter that is not zero is the entire point of showing it, so those are the only values
 * on this page allowed to change colour (`data-bad`).
 *
 * ## The danger zone
 *
 * Quitting is the last card and visually separated. It answers *before* it acts — the reply
 * arrives, then the process goes away — so the note says what happened rather than leaving
 * the user to wonder whether the click registered.
 */

const { call, read, threadState } = __bridge;
const { el, byId, setText, setChip, replaceChildren, emptyState } = __dom;
const { note, noteAnswer } = __feed;

/** `{ value }` slot for the subscriber rows. */
const slot = { rows: null };

/** One counter. `bad` is decided by the caller: only non-zero means a queue is stuck. */
function counter(label, value) {
  const bad = Number(value) > 0;
  return el('div', { className: 'counter', dataset: bad ? { bad: 'true' } : {} }, [
    el('span', { className: 'counter__label', textContent: label }),
    el('span', { className: 'counter__value', textContent: String(value ?? 0) }),
  ]);
}

/** Paint one `subscriber_snapshot`. */
function paint(doc) {
  if (!doc) return;
  const sids = doc.aggregated ?? [];
  setText('sub-aggregated', sids.length ? sids.join(' · ') : '暂无订阅客户端');

  const children = doc.children ?? [];
  if (!children.length) {
    setChip('sub-thread', { tone: '', text: '没有这一路' });
    return;
  }

  // This family has one child today (the DLNA receiver). Written for many, so adding
  // Chromecast or AirPlay here does not change this function.
  const child = children[0];
  setChip('sub-thread', threadState(child.running, child.event_thread_alive));
  replaceChildren(byId('sub-counters'), [
    counter('待入表', child.pending_append),
    counter('待广播', child.pending_state),
    counter('待移除', child.pending_remove),
  ]);

  const rows = child.rows ?? [];
  const signature = JSON.stringify(rows);
  if (slot.rows === signature) return;
  slot.rows = signature;

  const box = byId('sub-rows');
  if (!rows.length) {
    replaceChildren(box, emptyState(sids.length
      ? '表里没有行，但上面的计数器不是零 —— 队列没被排干，看「待入表」。'
      : '没有订阅者：手机还没连上来，或者它续约的 path 不是我们答的那一个。'));
    return;
  }
  replaceChildren(box, rows.map((row) => el('div', {
    className: row.timed_out ? 'rowitem rowitem--dead' : 'rowitem',
  }, [
    el('span', {
      className: 'rowitem__title mono',
      textContent: `${row.sid} · ${row.service} · seq ${row.seq}${row.timed_out ? ' · 已超时' : ''}`,
    }),
    // The callback URL came from the sender, so it is untrusted text like a log line.
    el('span', {
      className: 'rowitem__meta',
      textContent: `${row.url} · 续约 ${row.timeout} 秒 · 失败 ${row.errors} 次`,
    }),
  ])));
}

/** Read and paint. */
async function refresh() {
  paint(await read('subscriber_snapshot'));
}

/** Mount both cards. */
function mount() {
  byId('quit').addEventListener('click', async () => {
    const result = await call('app_action', { action: 'quit' });
    if (!result.ok) {
      note(`退出失败：${result.error}`, 'bad');
      return;
    }
    // The answer arrives first and the process leaves ~250 ms later, so this note is
    // written while the page is still alive — which is the point of answering first.
    noteAnswer('退出', result.data);
  });
}

return { paint, refresh, mount };
})();

/* ---- app.js ---- */
/*
 * The shell: the rail, the two views, and the polling that keeps them current.
 *
 * ## Two views, one process
 *
 * The split is by **who the reader is**, not by feature:
 *
 * * **使用** — what a person who is casting something needs: what is playing, how to cast
 *   something else, and where to log in for a page that needs it. Nothing here mentions a
 *   port, a thread or a queue.
 * * **管理** — the diagnostic half: services and the interface picker, logs, the module
 *   settings table, the advanced JSON editor, subscribers, and the way out.
 *
 * Both are mounted at once and shown/hidden, rather than created on demand. Two reasons,
 * both learned the hard way in this window: a view rebuilt on every switch loses the scroll
 * position and any half-typed value (the friendly-name field, the log textarea), and a mount
 * that throws takes its whole view down with no visible reason.
 *
 * ## Polling
 *
 * Two cadences, because the two halves age differently:
 *
 * * **1.5 s** — the player snapshot. This is what a person watches while casting.
 * * **5 s** — services, subscribers, and the log module list. These are `stat`-level reads
 *   plus an OS interface enumeration (`getifaddrs`), which is not something to do 40 times a
 *   minute; and the log list **must** be polled, because its two byte counts are of files
 *   that grow while the window is open.
 *
 * Every poll is guarded: a rejected promise in one cadence must not stop the other, and the
 * boot sequence catches each call separately. A missing initialisation used to leave every
 * field at its placeholder with nothing in the console to point at it.
 */

const { bridged, call, read, subscribe } = __bridge;
const { mountFeed, note } = __feed;
const playing = __now_playing;
const casting = __casting;
const accounts = __accounts;
const services = __services;
const logs = __logs;
const settings = __settings;
const diagnostics = __diagnostics;

/**
 * Everything the window wrote to its own console before the first paint.
 *
 * Collected from the first statement of this module, because the failures worth reporting
 * happen *before* any handler exists — a module that does not import, a `null` element.
 */
const earlyErrors = [];
globalThis.addEventListener?.('error', (event) => earlyErrors.push(String(event.message ?? event)));
globalThis.addEventListener?.('unhandledrejection', (event) => earlyErrors.push(String(event.reason ?? event)));

/** Which view is showing. `null` means "not decided yet". */
let current = null;

/**
 * Show one view.
 *
 * `hidden` on the section, `aria-current` on the rail item: the first is what the browser
 * paints, the second is what a screen reader announces, and having only one of them is a
 * view that is invisible to one of the two readers.
 */
function show(view) {
  if (view === current) return;
  current = view;
  for (const section of document.querySelectorAll('[data-view]')) {
    section.hidden = section.dataset.view !== view;
  }
  for (const item of document.querySelectorAll('[data-goto]')) {
    if (item.dataset.goto === view) item.setAttribute('aria-current', 'page');
    else item.removeAttribute('aria-current');
  }
  // The deep link stays shareable and reloadable: `#manage` opens the admin view.
  if (history.replaceState) history.replaceState(null, '', `#${view}`);
  // Not `?.` on `scrollTo`: an element's `scrollTo` exists but can throw in a webview whose
  // engine has no scrolling implementation for it, and an exception here would abort `show()`
  // — leaving both views hidden, which is exactly the "two nav labels and nothing else"
  // symptom this function exists to prevent. Swallowed, because scrolling to the top is a
  // courtesy, not a requirement.
  try {
    document.querySelector('.content')?.scrollTo?.({ top: 0 });
    window.scrollTo({ top: 0 });
  } catch (ignored) {
    /* a view that cannot scroll is still a view that renders */
  }
}

/** Mount everything, once. */
function mountShell() {
  mountFeed(document.getElementById('events'));

  document.querySelectorAll('[data-goto]').forEach((item) => {
    item.addEventListener('click', () => show(item.dataset.goto));
  });

  // Each module mounts only its own controls. A mount that throws is caught per module so
  // one broken card cannot take the other five with it — and the failure is *visible*,
  // because that is what an empty view with no console message cost us before.
  const mounts = [
    ['播放控制', playing.mount],
    ['投屏', casting.mount],
    ['账号与登录态', accounts.mount],
    ['服务与网络', services.mount],
    ['日志', logs.mount],
    ['模块设置', settings.mount],
    ['高级设置', settings.mountAdvanced],
    ['订阅与退出', diagnostics.mount],
  ];
  for (const [label, mount] of mounts) {
    try {
      mount();
    } catch (error) {
      note(`${label} 的界面没能挂上：${error?.message ?? error}`, 'bad');
    }
  }

  // Backend events. The payload is the argument array `cherrypy.engine.publish` was given,
  // so the feed shows exactly what was published rather than a summary of it.
  for (const name of [
    'mpv_start',
    'mpvipc_start',
    'renderer_start',
    'app_notify',
    'mpv_update_duration',
    'renderer_av_uri',
    'renderer_av_stop',
  ]) {
    subscribe(name, (payload) => note(`${name} ${JSON.stringify(payload ?? [])}`));
  }
}

/**
 * The boot sequence.
 *
 * Every call is caught individually and named in its failure. A single unhandled rejection
 * here used to take the rest of the initialisation with it, which reads as "five cards are
 * broken" when the truth is "one function is missing".
 */
/**
 * The rail's two identity lines.
 *
 * `runtime_status` answers `{backend, version, migration, status}`. The rail shows two of
 * them because that is all a rail has room for; the other two are the build's own prose and
 * belong in the admin view rather than beside the app name.
 */
async function paintIdentity() {
  const doc = await read('runtime_status');
  if (!doc) return;
  const version = document.getElementById('version');
  if (version) version.textContent = doc.version ? `v${doc.version}` : '—';
  const backend = document.getElementById('backend');
  if (backend) backend.textContent = `${doc.backend ?? '—'} · ${doc.status ?? '—'}`;
}

function boot() {
  if (!bridged) {
    note('这个窗口没有后端桥：请通过 Tauri 运行，页面上的按钮不会有效果。', 'bad');
    return;
  }
  paintIdentity().catch((error) => note(`读取运行时状态失败：${error}`, 'bad'));
  playing.refresh().catch((error) => note(`读取播放状态失败：${error}`, 'bad'));
  services.refresh().catch((error) => note(`读取服务状态失败：${error}`, 'bad'));
  diagnostics.refresh().catch((error) => note(`读取订阅客户端失败：${error}`, 'bad'));
  logs.loadModules().catch((error) => note(`读取日志清单失败：${error}`, 'bad'));
  accounts.refresh().catch((error) => note(`读取 cookie 状态失败：${error}`, 'bad'));
  settings.refresh().catch((error) => note(`读取模块设置失败：${error}`, 'bad'));

  setInterval(() => playing.refresh().catch(() => {}), 1500);
  setInterval(() => {
    services.refresh().catch(() => {});
    diagnostics.refresh().catch(() => {});
    logs.loadModules().catch(() => {});
  }, 5000);
}

/**
 * Tell the back end what the first paint actually produced.
 *
 * A page that fails to load looks identical from the outside to a page that is fine: the
 * markup's placeholders stay on screen and nothing is logged anywhere. Twice now the answer
 * to "the window is blank" was found only by opening devtools — once for a deleted boot
 * block, once for a binary older than its modules. This line makes the page answer for
 * itself, once, in `macast.log`:
 *
 * ```
 * window: page rendered — view=use cards=9 placeholders=0 errors=0
 * ```
 *
 * `placeholders` counts elements still holding the markup's own loading text, and `errors`
 * counts what the console saw. Both zero is what a working window reports, and both are
 * what a broken one cannot fake — a hand-written string could not name the view or count
 * the cards.
 */
function reportRendered(errors) {
  const placeholders = [...document.querySelectorAll('dd, h2, p, span')]
    .filter((node) => node.textContent.trim() === '加载中…').length;
  const cards = [...document.querySelectorAll('[data-view]:not([hidden]) .card, [data-view]:not([hidden]) .now')].length;
  const view = document.querySelector('[data-view]:not([hidden])')?.dataset.view ?? 'none';
  // The modules may not have imported cleanly, in which case `bridged` is false and there is
  // no one to tell — but the console error above already said so.
  Promise.resolve(call('page_ready', {
    report: `view=${view} cards=${cards} placeholders=${placeholders} errors=${errors.length}`,
  })).catch(() => {});
}

// The initial view comes from the fragment, so `#manage` opens the admin half — a reload
// keeps you where you were.
const start = location.hash === '#manage' ? 'manage' : 'use';
mountShell();
show(start);
boot();
// **After the initial reads have landed, not at the first frame.** The three placeholder
// strings in the markup (`player-chip`, `title`, `ck-jar`) are filled by async commands, so a
// report taken at the first frame counts them as "still loading" and reports a healthy window
// as half-broken — measured 2026-10-22: `placeholders=1` on a window that was rendering fine.
// 600 ms is longer than any of those three round trips and shorter than the first poll, so the
// number means "placeholders still showing after the initial reads settled".
setTimeout(() => reportRendered(earlyErrors), 600);
})();
