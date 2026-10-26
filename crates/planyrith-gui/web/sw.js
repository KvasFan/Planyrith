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

// The application's own files from the cache, everything else from the
// network.
self.addEventListener("fetch", (event) => {
  if (event.request.method !== "GET") {
    return;
  }
  event.respondWith(caches.match(event.request).then((hit) => hit || fetch(event.request)));
});
