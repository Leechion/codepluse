'use strict';
// App-shell cache for the installed desktop/PWA app. Static assets are
// network-first with a cache fallback (so updates land immediately and the
// shell still opens offline); /api/* and the SSE stream are never cached.
const CACHE = 'harness-hub-v1';
const CORE = ['/', '/styles.css', '/chat.js', '/app.js', '/missions.js', '/manifest.webmanifest',
  '/icon-192.png', '/icon-512.png', '/icon-512-maskable.png', '/core/engine.js'];

self.addEventListener('install', event => {
  event.waitUntil(caches.open(CACHE).then(cache => cache.addAll(CORE)).then(() => self.skipWaiting()));
});
self.addEventListener('activate', event => {
  event.waitUntil((async () => {
    const keys = await caches.keys();
    await Promise.all(keys.filter(k => k !== CACHE).map(k => caches.delete(k)));
    await self.clients.claim();
  })());
});
self.addEventListener('fetch', event => {
  const url = new URL(event.request.url);
  if (url.origin !== location.origin) return;
  if (url.pathname.startsWith('/api/')) return; // REST + SSE stay live-only
  if (event.request.method !== 'GET') return;
  event.respondWith((async () => {
    try {
      const fresh = await fetch(event.request);
      if (fresh.ok) {
        const cache = await caches.open(CACHE);
        cache.put(event.request, fresh.clone()).catch(() => {});
      }
      return fresh;
    } catch (err) {
      const hit = await caches.match(event.request, { ignoreSearch: event.request.mode !== 'navigate' });
      if (hit) return hit;
      throw err;
    }
  })());
});
