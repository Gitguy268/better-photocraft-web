// PhotoCraft service worker: lets the installed app start offline.
// The one hand-written script in this repository (docs/decisions.md), registered from Rust
// (apps/photocraft-web/src/pwa.rs). No dependencies, no UI, nothing precached by name: the
// build's file names carry content hashes, so files are cached as the page asks for them.
// Bump CACHE when the caching rules change; this worker's older caches are deleted on activate.
// Caches the app keeps itself (downloaded AI models) are not this worker's to touch.
const CACHE = 'photocraft-v2';
const OWN = 'photocraft-v';

// Files whose names do not change with their contents: network first, cache as the fallback.
// The packaged site's app/ loader and parts list keep their names from release to release.
const FRESH = /\/(index\.html|manifest\.webmanifest|fonts\/manifest\.txt|ai-worker\.txt|app\/photocraft-web\.js|app\/parts\.txt)?$/;
// Content-hashed files and checksum-named folders never change: cache first.
const IMMUTABLE = /\.(wasm|js)$|\.part-[a-z]+$|\/ai-worker-[^/]+\//;
// A new build's file replaces the old build's: the same name with another hash.
const family = (path) => path.replace(/-[0-9a-f]{8,}/, '-*');

self.addEventListener('install', () => self.skipWaiting());

self.addEventListener('activate', (event) => {
  event.waitUntil((async () => {
    for (const name of await caches.keys()) {
      if (name !== CACHE && name.startsWith(OWN)) await caches.delete(name);
    }
    await self.clients.claim();
  })());
});

self.addEventListener('fetch', (event) => {
  const request = event.request;
  const url = new URL(request.url);
  // Other hosts (AI models, for one) are fetched and cached by the app itself.
  if (request.method !== 'GET' || url.origin !== self.location.origin) return;
  event.respondWith(respond(request, url.pathname));
});

async function respond(request, path) {
  const cache = await caches.open(CACHE);
  const cacheFirst = IMMUTABLE.test(path) && !FRESH.test(path);
  if (cacheFirst) {
    const hit = await cache.match(request);
    if (hit) return hit;
  }
  try {
    const response = await fetch(request);
    if (response.ok && response.type === 'basic') await store(cache, request, path, response.clone());
    return response;
  } catch (offline) {
    // URL flags (?webgl, ?cpu) share the page's cached copy.
    const hit = await cache.match(request, { ignoreSearch: true });
    if (hit) return hit;
    throw offline;
  }
}

async function store(cache, request, path, response) {
  if (IMMUTABLE.test(path)) {
    for (const old of await cache.keys()) {
      const other = new URL(old.url).pathname;
      if (other !== path && family(other) === family(path)) await cache.delete(old);
    }
  }
  try {
    await cache.put(request, response);
  } catch (full) {
    // Out of quota: the app still runs from the network.
  }
}
