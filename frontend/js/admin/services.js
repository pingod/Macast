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

import { call, read, serviceState, threadState } from '../lib/bridge.js';
import { byId, setText, setChip } from '../lib/dom.js';
import { note, noteAnswer, toast } from '../lib/feed.js';

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
export function paint(document_) {
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
export async function refresh() {
  paint(await read('service_snapshot'));
}

/** Mount the card. */
export function mount() {
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
