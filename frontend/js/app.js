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

import { bridged, call, read, subscribe } from './lib/bridge.js';
import { mountFeed, note } from './lib/feed.js';
import * as playing from './user/now-playing.js';
import * as casting from './user/casting.js';
import * as accounts from './user/accounts.js';
import * as services from './admin/services.js';
import * as logs from './admin/logs.js';
import * as settings from './admin/settings.js';
import * as diagnostics from './admin/diagnostics.js';

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
