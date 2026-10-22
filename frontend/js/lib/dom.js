/*
 * DOM helpers. No framework: this is one window with two views, and a build step would
 * cost more than the framework saves.
 *
 * The rule enforced here is the one that matters for security and for correctness alike:
 *
 * **text goes in through `textContent`, never through markup.** Every string this window
 * displays except its own copy comes from somewhere else — a DIDL `dc:title` a phone sent,
 * a URL from a page we scraped, a SID a control point invented, a log line an engine
 * wrote. `AGENTS.md` §4.10 records what happens when a title with an `<img onerror=…>`
 * reaches `v-html`: the settings page executes it. So there is no `html()` helper in this
 * file, deliberately — the capability is absent, not merely unused.
 */

/**
 * Build one element.
 *
 * `props` sets properties (not attributes) — `className`, `textContent`, `value`, `id` —
 * because those are the ones that behave identically for every element type.
 */
export function el(tag, props = {}, children = []) {
  const node = document.createElement(tag);
  for (const [key, value] of Object.entries(props)) {
    if (value === undefined || value === null) continue;
    if (key === 'dataset') {
      Object.assign(node.dataset, value);
    } else if (key === 'style' && typeof value === 'object') {
      Object.assign(node.style, value);
    } else if (key.startsWith('on') && typeof value === 'function') {
      node.addEventListener(key.slice(2).toLowerCase(), value);
    } else {
      node[key] = value;
    }
  }
  for (const child of [].concat(children)) {
    if (child === null || child === undefined || child === false) continue;
    node.append(child instanceof Node ? child : document.createTextNode(String(child)));
  }
  return node;
}

/** An element found by id, with a message that names the id when it is missing. */
export function byId(id) {
  const node = document.getElementById(id);
  if (!node) throw new Error(`页面缺少 #${id} —— 标记与脚本对不上了`);
  return node;
}

/** Write text into the element with this id, if it exists. */
export function setText(id, text) {
  const node = document.getElementById(id);
  if (node) node.textContent = text ?? '';
}

/**
 * Set a state chip: its `data-tone` and its words.
 *
 * One function for both halves, because a chip whose colour and text disagree is worse
 * than no chip — `data-tone` is the only variant axis the stylesheet knows.
 */
export function setChip(id, state) {
  const node = document.getElementById(id);
  if (!node) return;
  node.dataset.tone = state.tone;
  node.textContent = state.text;
}

/**
 * Replace a container's children, keeping the scroll position of the container itself.
 *
 * Lists on this page are rebuilt on a 1.5 s poll. Without this, a pane a user has scrolled
 * would jump back to the top on every tick — and a list rebuilt while the pointer is over
 * a row swallows the click that was about to land.
 */
export function replaceChildren(container, children) {
  const { scrollTop } = container;
  container.replaceChildren(...[].concat(children).filter(Boolean));
  container.scrollTop = scrollTop;
}

/**
 * Render only when a signature changes.
 *
 * Every polled list on this page is compared by signature first. The reason is not
 * performance: rebuilding a list mid-interaction replaces the element the pointer is
 * pressing, which is exactly how "I had to click twice" bugs appear.
 */
export function onceChanged(slot, signature, render) {
  if (slot.value === signature) return false;
  slot.value = signature;
  render();
  return true;
}

/** An empty-state block. Says what would fill it; a bare dash is a dead end. */
export function emptyState(text) {
  return el('p', { className: 'empty', textContent: text });
}

/** An inline notice with a tone, for the result of one action. */
export function notice(text, tone = '') {
  return el('p', { className: 'notice', dataset: tone ? { tone } : {}, textContent: text });
}
