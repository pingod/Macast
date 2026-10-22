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

import { call, read, fmtBytes } from '../lib/bridge.js';
import { byId, setText } from '../lib/dom.js';
import { note, noteAnswer, toast } from '../lib/feed.js';

/** The hint under the browser field, so the two refusals do not read as one. */
const BROWSER_GRAMMAR = '名字[:资料目录]。+钥匙串 与 ::容器 我们都不收 —— 那两样是从不替你挑的东西。';

/** Read the state and paint it. */
export async function refresh() {
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
export function mount() {
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
export const browserGrammar = BROWSER_GRAMMAR;
