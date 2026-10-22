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

import { el, replaceChildren } from './dom.js';
import { fmtStamp } from './bridge.js';

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
export function mountFeed(container) {
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
export function note(text, tone = '') {
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

export function toast(text, tone = 'ok') {
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
export function noteResult(label, result) {
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
export function noteAnswer(label, answer) {
  if (answer && answer.code === 0) {
    note(`${label} → ${answer.message ?? 'success'}`);
    toast(answer.message ?? '已生效');
    return;
  }
  const text = answer?.message ?? '没有答复';
  note(`${label} → ${text}`, 'bad');
}
