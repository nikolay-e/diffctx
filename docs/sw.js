const SHELL = "diffctx-shell-v2";
// The manifest's shortcuts are what an installed icon opens; a shortcut page
// missing here opened as the start page offline, silently.
const SHELL_FILES = [
  "./",
  "./index.html",
  "./manifest.webmanifest",
  "./icons/icon-192.png",
  "./product/cli.html",
  "./product/token-budget.html",
];

self.addEventListener("install", (e) => {
  e.waitUntil(caches.open(SHELL).then((c) => c.addAll(SHELL_FILES)));
});

self.addEventListener("activate", (e) => {
  e.waitUntil(
    caches
      .keys()
      .then((names) => Promise.all(names.filter((n) => n !== SHELL).map((n) => caches.delete(n))))
      .then(() => self.clients.claim()),
  );
});

// Pages has no build step, so nothing stamps a version into this file: every
// navigation goes to the network first and the cache is only the offline
// fallback, which is what keeps a live visit from ever seeing a stale page.
self.addEventListener("fetch", (e) => {
  const req = e.request;
  if (req.method !== "GET" || new URL(req.url).origin !== self.location.origin) return;
  e.respondWith(
    caches.open(SHELL).then(async (cache) => {
      try {
        const res = await fetch(req);
        if (res.ok) cache.put(req, res.clone());
        return res;
      } catch (err) {
        const hit = await cache.match(req, { ignoreSearch: true });
        if (hit) return hit;
        if (req.mode === "navigate") return cache.match("./index.html");
        throw err;
      }
    }),
  );
});
