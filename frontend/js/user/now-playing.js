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

import { call, read, fmtClock } from '../lib/bridge.js';
import { el, byId, setText, setChip, replaceChildren } from '../lib/dom.js';
import { note, noteResult, noteAnswer } from '../lib/feed.js';

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
export function paintSnapshot(snapshot) {
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
export async function refresh() {
  paintSnapshot(await read('player_snapshot'));
}

/** Wire the controls. Called once, by the shell, when this view mounts. */
export function mount() {
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
