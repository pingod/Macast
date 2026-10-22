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

import { call, read, fmtSeconds } from '../lib/bridge.js';
import { el, byId, setText, replaceChildren, emptyState } from '../lib/dom.js';
import { note, noteResult, toast } from '../lib/feed.js';

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
export async function loadTargets(refresh = false) {
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
export function mount() {
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
