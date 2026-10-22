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

import { call, read, threadState } from '../lib/bridge.js';
import { el, byId, setText, setChip, replaceChildren, emptyState } from '../lib/dom.js';
import { note, noteAnswer } from '../lib/feed.js';

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
export function paint(doc) {
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
export async function refresh() {
  paint(await read('subscriber_snapshot'));
}

/** Mount both cards. */
export function mount() {
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
