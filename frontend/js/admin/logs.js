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

import { read, fmtBytes } from '../lib/bridge.js';
import { byId, setText } from '../lib/dom.js';
import { note, noteAnswer, toast } from '../lib/feed.js';

/** `{ value }` slot for the module list, so the select is only rebuilt on a change. */
const slot = { modules: null };

/** Paint the module chooser from a `log_modules` document. */
export function paintModules(doc) {
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
export async function readTail(all = false) {
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
export async function loadModules() {
  const doc = await read('log_modules');
  if (doc) paintModules(doc);
}

/** Mount the card. */
export function mount() {
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
