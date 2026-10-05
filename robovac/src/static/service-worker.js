// App-shell caching only.
//
// The map and state are live data. Serving a stale map because it was cached
// would misrepresent where a robot actually is, which is worse than showing
// nothing, so nothing under /api or /map.svg is ever cached.

const SHELL_CACHE = 'robovac-shell-v1';
const SHELL = ['/', '/static/style.css', '/static/app.css', '/static/app.js'];

self.addEventListener('install', (event) => {
  event.waitUntil(
    caches
      .open(SHELL_CACHE)
      .then((cache) => cache.addAll(SHELL))
      .then(() => self.skipWaiting()),
  );
});

self.addEventListener('activate', (event) => {
  event.waitUntil(
    caches
      .keys()
      .then((keys) =>
        Promise.all(keys.filter((k) => k !== SHELL_CACHE).map((k) => caches.delete(k))),
      )
      .then(() => self.clients.claim()),
  );
});

self.addEventListener('fetch', (event) => {
  const { request } = event;
  if (request.method !== 'GET') return;

  const url = new URL(request.url);
  if (url.origin !== self.location.origin) return;

  // Live data and commands stay uncached.
  const isApi =
    url.pathname.startsWith('/api/') ||
    url.pathname === '/map.svg' ||
    url.pathname === '/events' ||
    url.pathname === '/healthz' ||
    url.pathname === '/readyz';
  if (isApi) return;

  // Network first for the shell, so a new release is picked up immediately, and
  // cache only as a fallback when the network is unavailable.
  event.respondWith(
    fetch(request)
      .then((response) => {
        if (response.ok && SHELL.includes(url.pathname)) {
          const copy = response.clone();
          caches.open(SHELL_CACHE).then((cache) => cache.put(request, copy));
        }
        return response;
      })
      .catch(() =>
        caches.match(request).then((cached) => cached ?? caches.match('/static/app.css')),
      ),
  );
});
