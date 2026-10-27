// The web build's service worker (docs/11 §4.4): keeps the application for
// offline use. `tools/build-web.sh` stamps the cache name with the build, so
// a new build replaces the old files.
const CACHE = "planyrith-@BUILD@";
const FILES = [
  "./",
  "index.html",
  "planyrith-gui.js",
  "planyrith-gui_bg.wasm",
  "manifest.webmanifest",
  "icon.svg",
];

self.addEventListener("install", (event) => {
  event.waitUntil(caches.open(CACHE).then((cache) => cache.addAll(FILES)));
  self.skipWaiting();
});

self.addEventListener("activate", (event) => {
  event.waitUntil(
    caches
      .keys()
      .then((keys) => Promise.all(keys.filter((k) => k !== CACHE).map((k) => caches.delete(k))))
      .then(() => self.clients.claim()),
  );
});

// Network first, revalidating with the server, so a new build is used as
// soon as it is published; the cache serves only when the network fails
// (offline). Cache-first kept running the previous build until a second
// reload.
self.addEventListener("fetch", (event) => {
  const request = event.request;
  if (request.method !== "GET" || new URL(request.url).origin !== self.location.origin) {
    return;
  }
  event.respondWith(
    fetch(request, { cache: "no-cache" })
      .then((response) => {
        if (response.ok) {
          const copy = response.clone();
          caches.open(CACHE).then((cache) => cache.put(request, copy));
        }
        return response;
      })
      .catch(() => caches.match(request).then((hit) => hit || Response.error())),
  );
});
