const CACHE = 'inkstone-shell-v1';
const SHELL = ['/', '/styles.css', '/manifest.webmanifest', '/icon.svg', '/icon-192.png'];
const IMMUTABLE = /^\/pkg\/[0-9a-f]{64}\//;

self.addEventListener('install', event => {
  event.waitUntil(caches.open(CACHE).then(cache => cache.addAll(SHELL)).catch(() => {}));
  self.skipWaiting();
});

self.addEventListener('activate', event => {
  event.waitUntil(
    caches.keys()
      .then(keys => Promise.all(keys.filter(key => key !== CACHE).map(key => caches.delete(key))))
      .then(() => self.clients.claim())
  );
});

self.addEventListener('fetch', event => {
  const request = event.request;
  const url = new URL(request.url);
  if (request.method !== 'GET' || url.origin !== self.location.origin || url.pathname.startsWith('/api/')) {
    return;
  }
  if (IMMUTABLE.test(url.pathname)) {
    event.respondWith(
      caches.match(request).then(hit => hit || fetch(request).then(response => {
        if (response.ok) {
          const copy = response.clone();
          caches.open(CACHE).then(cache => cache.put(request, copy));
        }
        return response;
      }))
    );
    return;
  }
  const key = request.mode === 'navigate' ? '/' : request;
  event.respondWith(
    fetch(request).then(response => {
      if (response.ok && response.type === 'basic') {
        const copy = response.clone();
        caches.open(CACHE).then(cache => cache.put(key, copy));
      }
      return response;
    }).catch(() => caches.match(key).then(hit => hit || Response.error()))
  );
});
