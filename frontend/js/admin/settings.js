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

import { call, read } from '../lib/bridge.js';
import { el, byId, replaceChildren, emptyState, notice as noticeNode } from '../lib/dom.js';
import { note, noteAnswer } from '../lib/feed.js';

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
export function paint(doc) {
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
export async function refresh() {
  paint(await read('module_settings_snapshot'));
}

/** Mount the card. */
export function mount() {
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
export function mountAdvanced() {
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
