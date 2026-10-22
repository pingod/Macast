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
export const bridged = typeof core?.invoke === 'function';

/**
 * Invoke one command, normalising every outcome to `{ok, data, error}`.
 *
 * A rejection is *not* rethrown: the caller decides what an empty state looks like, and a
 * page that loses an unhandled rejection also loses the rest of its boot sequence.
 */
export async function call(name, args) {
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
export async function read(name, args) {
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
export function subscribe(name, handler) {
  if (typeof eventBridge?.listen !== 'function') return;
  eventBridge.listen(name, ({ payload }) => handler(payload)).catch(() => {});
}

/* ------------------------------------------------------------------ formatting */

/** Human bytes. `—` rather than `0 B` when the number is not one: an absent size is not
 * a size of zero. */
export function fmtBytes(n) {
  if (typeof n !== 'number' || !Number.isFinite(n)) return '—';
  if (n < 1024) return `${n} B`;
  if (n < 1048576) return `${(n / 1024).toFixed(1)} KiB`;
  return `${(n / 1048576).toFixed(1)} MiB`;
}

/** Seconds to `m:ss` / `h:mm:ss`. The player reports seconds as a decimal string. */
export function fmtClock(seconds) {
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
export function fmtSeconds(v) {
  return typeof v === 'number' && Number.isFinite(v) ? `${v.toFixed(1)} 秒` : '—';
}

/** Time of day for the event feed's stamp: seconds matter when two things race. */
export function fmtStamp(date = new Date()) {
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
export function serviceState(running, healthy) {
  if (!running) return { tone: 'warn', text: '已停止' };
  if (healthy) return { tone: 'ok', text: '在听' };
  return { tone: 'bad', text: '端口在听，工作线程已不在' };
}

/** The same three states for GENA, whose second question is "is the thread alive". */
export function threadState(running, alive) {
  if (!running) return { tone: 'warn', text: '未启动' };
  if (alive) return { tone: 'ok', text: '在跑' };
  return { tone: 'bad', text: '要它跑，线程已不在' };
}
