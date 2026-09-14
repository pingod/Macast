/* Macast service worker: caches the app shell so the control panel can
 * open instantly / work offline (on localhost or HTTPS contexts).
 * NOTE: service workers require a secure context (https or localhost).
 * On a plain http LAN address the SW simply never activates — the page
 * still works normally; only the offline shell is unavailable there.
 */
const CACHE = 'macast-shell-v1';
const SHELL = [
  '/',
  '/assets/vue@2.6.14.js',
  '/assets/vue-resource@1.5.3.js',
  '/assets/element-ui@2.15.7.js',
  '/assets/element-ui@2.15.7.css',
  '/assets/icon.png'
];

self.addEventListener('install', event => {
  event.waitUntil(
    caches.open(CACHE).then(c => c.addAll(SHELL)).then(() => self.skipWaiting())
  );
});

self.addEventListener('activate', event => {
  event.waitUntil(
    caches.keys()
      .then(keys => Promise.all(keys.filter(k => k !== CACHE).map(k => caches.delete(k))))
      .then(() => self.clients.claim())
  );
});

self.addEventListener('fetch', event => {
  const req = event.request;
  if (req.method !== 'GET') return; // uploads (POST) go straight to network

  const url = new URL(req.url);
  // Dynamic data and streamed media must never be cached or answered stale.
  if (url.pathname.startsWith('/api') ||
      url.searchParams.has('local') ||
      url.pathname.startsWith('/local_files')) {
    return;
  }

  // Navigation requests: network-first, fall back to cached shell when offline.
  if (req.mode === 'navigate') {
    event.respondWith(
      fetch(req).catch(() => caches.match('/'))
    );
    return;
  }

  // Static assets: cache-first, then network (and populate cache).
  event.respondWith(
    caches.match(req).then(cached => {
      if (cached) return cached;
      return fetch(req).then(resp => {
        const copy = resp.clone();
        caches.open(CACHE).then(c => c.put(req, copy));
        return resp;
      });
    })
  );
});
