# Hosting PhotoCraft for the web

`better-photocraft-web-<version>.zip` (built by `packaging/web/package.sh`) holds a
static site in `better-photocraft-web-<version>/`:

| File | What it is |
|---|---|
| `index.html` | The page. It loads everything through relative URLs. |
| `photocraft-web-boot-<hash>.js`, `photocraft-web-boot-<hash>_bg.wasm` | A small module (about 120 KB) that starts the app from its parts, with its wasm-bindgen glue |
| `app/photocraft-web-<hash>.part-aa`, `-ab`, … | The app's WebAssembly, gzipped and cut into parts of at most 20 MiB (see [Sizes](#sizes)) |
| `app/parts.txt` | The parts, in order |
| `app/photocraft-web.js` | The app's wasm-bindgen glue (generated, ES module) |
| `sw.js`, `manifest.webmanifest`, `icons/` | The service worker, web app manifest and icons that make the app installable and able to start offline (see [Installable and offline](#installable-and-offline)) |
| `ai-worker.txt`, `ai-worker-<checksum>/` | The AI worker, a second wasm loaded on demand (see [AI models and the AI worker](#ai-models-and-the-ai-worker)) |
| `_headers`, `.htaccess` | Sample header rules for Netlify/Cloudflare Pages and Apache |
| `fonts/` (optional) | Fonts loaded on demand, listed in `fonts/manifest.txt` (see [Fonts](#fonts)) |

There is no server-side code. Upload the folder's contents anywhere that serves static files.

## Docker

> The Docker recipe (`Dockerfile`, `.dockerignore`, `packaging/web/nginx.conf`) is
> **community-maintained**: it is not built in CI or used for releases, so it can lag behind the
> official web build above. Fixes are welcome.

From the repository root (Docker is the only build prerequisite):

```sh
docker build --load -t photocraft-web:local .
docker run -d --name photocraft-web --restart unless-stopped \
  -p 8080:8080 photocraft-web:local
```

Open **http://localhost:8080/**. The multi-stage `Dockerfile` builds the existing Rust/Wasm
app with Trunk, including HEIF support, then copies only the static site into NGINX. The
runtime runs as the `nginx` user on port **8080**, with a `/healthz` endpoint and Docker
health check. It serves precompressed gzip assets, the Wasm MIME type, immutable hashed
assets, and an HTML page that revalidates after deployments. No Rust installation, Node.js,
database, GPU passthrough, or document volume is needed on the host.

The first build downloads Rust dependencies and Trunk's Wasm tools and can take several
minutes. Subsequent builds reuse Cargo caches. The builder supports Linux amd64 and arm64;
the default Rust version is the stable release CI used when the `Dockerfile` was last updated
(CI and releases always use the latest stable), and Trunk is pinned to the release workflow's
`TRUNK_VERSION`. `RUST_VERSION`, `TRUNK_VERSION`, and `NGINX_VERSION` can be overridden with
`--build-arg`. To stamp the About dialog with the commit and date instead of "dev build", pass
`--build-arg PHOTOCRAFT_BUILD_SHA=$(git rev-parse HEAD) --build-arg PHOTOCRAFT_BUILD_DATE=$(date -u +%F)`.
The web build does not embed the optional `craft-fonts` checkout.

The Docker build uses thin LTO and one Cargo build job to reduce peak memory, while keeping
the web profile's size optimizations and `wasm-opt -Oz`. Its Wasm size may differ from the
release zip figures below; NGINX has no 25 MiB file limit. To use the release zip's fat LTO on
a builder with more memory, pass `--build-arg CARGO_PROFILE_WASM_RELEASE_LTO=fat`.
`--build-arg CARGO_BUILD_JOBS=4` enables more concurrent compilation on larger builders. A compiler killed with
`SIGKILL`/`cannot allocate memory` means the Docker/Podman VM needs more memory or swap.

For a public deployment, put an HTTPS reverse proxy or your hosting platform's TLS endpoint
in front of container port 8080. With a reverse proxy on the same host, bind only loopback
(`-p 127.0.0.1:8080:8080`); with a proxy container, connect both through a Docker network.
HTTPS enables WebGPU and browser clipboard APIs. If hosting under `/photocraft/`, redirect
`/photocraft` to `/photocraft/` and strip that prefix when proxying to the container. Relative
asset URLs then work without rebuilding. For example, in an existing HTTPS NGINX server:

```nginx
location = /photocraft { return 301 /photocraft/; }
location /photocraft/ {
    proxy_pass http://127.0.0.1:8080/;
}
```

The editor and image processing run on the visitor's device. Open uses the browser file
picker; Save and Export download files. Preferences use browser localStorage. There is no
server-side document storage or desktop TCP control server. Documents with unsaved changes are
autosaved into the browser's IndexedDB (see [Autosave and crash recovery](#autosave-and-crash-recovery));
that is a safety net on one device, not storage: save/download work before closing the page. A
browser with WebGPU or WebGL2 is required; `?webgl` forces the fallback when troubleshooting.
In a browser that can't inflate a stream itself (no `DecompressionStream`: before Safari 16.4 and
Firefox 113), the release zip's site inflates the app's parts in its own code, a little slower.
A server that recognises the gzip in a part and sends it with `Content-Encoding: gzip` or
`x-gzip` (Apache with `mod_mime_magic` does) works too: the browser inflates it on the way, and
the page then uses it as it is. (That holds while the app ships as one part, as it does today:
such a server labels only the first part, the one that starts with gzip's header.)

To verify the browser deployment, open the page, open an image and save/export it.

## Sizes

Measured on 2026-10-13 (0.8.2, `packaging/web/package.sh`):

| File | Size |
|---|---|
| `app/photocraft-web-<hash>.part-aa` (the only part today) | 11,401,527 bytes (10.9 MiB) |
| `app/photocraft-web.js` | about 170 KB (26 KB with gzip) |
| `photocraft-web-boot-<hash>_bg.wasm` and `.js` | about 120 KB together |

The app's WebAssembly is 25,019,043 bytes (23.9 MiB) before gzip. This edition's AI worker
(`ai-worker-<checksum>/photocraft-ml-worker_bg.wasm`, 14,641,656 bytes at 0.7.0-better.1) is a
separate file under the same limit.

Per-file upload limits: Cloudflare Pages and Workers static assets reject any file over
**25 MiB** (26,214,400 bytes), and the app's WebAssembly is at that size. (Releases up to 0.2.0
shipped a 25.8 MiB wasm, which Cloudflare rejected; issue #198.) So the site doesn't hold it as
one file: packaging gzips it and cuts the result into parts of at most 20 MiB, and the page puts
them back together while it downloads them. `packaging/web/package.sh` fails if any file in the
site is over 24 MiB, so a release can't ship a file a common host refuses, and the app can grow
without reaching that limit (issue #5302). A build straight from Trunk (`trunk build --release`
in `apps/photocraft-web`, or the Docker image above) still has the app in one `.wasm`, for hosts
without such a cap.

The app is built with the `wasm-release` Cargo profile (fat LTO, size-optimized code with the
pixel and file-decoding crates kept at speed) plus `wasm-opt -Oz`, and stores the bundled fonts
(PhotoCraft's and egui's) and the translation catalogs compressed, unpacked on first use.

## Any path works

All URLs in `index.html` are relative (`public_url = "./"` in `apps/photocraft-web/Trunk.toml`),
so the site works at a domain root (`https://example.com/`), under a prefix
(`https://example.com/tools/photocraft/`) and from a CDN bucket. The asset names carry a content
hash, so they can be cached forever. `index.html`, `app/photocraft-web.js` and `app/parts.txt` keep
their names and need revalidation.

## Required server settings

- **MIME type:** serve `.wasm` as `application/wasm`. Browsers refuse to stream-compile it under
  any other type, and the app then loads slowly or not at all. Serve `.js` as `text/javascript`.
  Most hosts already do both. For nginx, check that `mime.types` has `application/wasm wasm;`.
- **The parts as they are:** serve `app/*.part-*` byte for byte, with no `Content-Encoding` (they
  are gzip already, and the page inflates them). Their extension is one servers don't
  know, so the usual result, `application/octet-stream` and no compression, is right.
- **Missing files are 404s:** a host that answers every unknown path with `index.html` (a
  single-page-app fallback) hides a failed upload. PhotoCraft notices and says the download
  failed, but a 404 is clearer in the server's logs.
- **Compression:** turn on gzip or Brotli for `.wasm`, `.js` and `.html`. It matters little for
  the download, since the parts are compressed already; it shrinks the two `.js` files and the
  small boot module. You can also precompress and let the server send `Content-Encoding: br`.
- **Caching:** `Cache-Control: public, max-age=31536000, immutable` on the files whose names
  carry a hash (the parts and `photocraft-web-boot-<hash>.*`), and `no-cache` on `index.html`,
  `app/photocraft-web.js`, `app/parts.txt`, `sw.js`, `manifest.webmanifest` and `ai-worker.txt`
  (the AI worker's `ai-worker-<checksum>/` folder can be cached forever). The sample `_headers` and `.htaccess` do this.
- **HTTPS:** WebGPU (and the clipboard) only work in a secure context, which means `https://`
  or `http://localhost`. Over plain HTTP elsewhere, the app falls back to WebGL2.
- **No special isolation headers:** PhotoCraft doesn't use `SharedArrayBuffer`, so it doesn't
  need `Cross-Origin-Opener-Policy` or `Cross-Origin-Embedder-Policy`. If your site already sends
  COEP `require-corp`, also send `Cross-Origin-Resource-Policy: same-origin` (or `cross-origin`
  when the files live on a CDN) on the app's files.

nginx example:

```nginx
location /photocraft/ {
    types { application/wasm wasm; text/javascript js; text/html html; }
    gzip on;
    gzip_types application/wasm text/javascript text/html;
    # The first matching line wins, so the files that keep their names come first.
    location ~* (index\.html|app/photocraft-web\.js|app/parts\.txt)$ { add_header Cache-Control "no-cache"; }
    location ~* (\.(wasm|js)|\.part-[a-z]+)$ { add_header Cache-Control "public, max-age=31536000, immutable"; }
}
```

Local test: `python3 -m http.server 8765` inside the folder, then open http://localhost:8765/.

## Fonts

The app has two fonts of its own, Inter and JetBrains Mono: a browser offers no system fonts, and
embedding more would push the wasm over the size gate. A host can serve more fonts next to the
app. They show in the font menus straight away, and a family's files are downloaded the first time
it is picked or a document's text needs it, so starting the app costs one small text file.

1. Put the font files (`.ttf`, `.otf`, `.ttc`) anywhere under `fonts/` next to `index.html`.
2. List them in `fonts/manifest.txt`, one file per line, in the format of
   [craft-fonts](https://github.com/storytold/craft-fonts)' own `fonts/manifest.txt`:

   ```text
   # family | style | file (relative to the site root) | scripts | licence | licence file | sha256 | source
   Open Sans | Regular | fonts/open-sans/OpenSans[wdth,wght].ttf
   Open Sans | Italic | fonts/open-sans/OpenSans-Italic[wdth,wght].ttf
   Lobster | Regular | fonts/lobster/Lobster-Regular.ttf | Latn,Cyrl | OFL-1.1 | fonts/lobster/OFL.txt
   ```

   The app reads the first three fields; the others are optional here. `family` must be the font's
   own family name (the typographic family in its `name` table, else the family), which is what the
   font menu shows once the file is in. The path is relative to the site root and can't leave it
   (no `..`, no `/` at the start, no other host). A variable font is one line: the style menu offers
   the weights of its `wght` axis.
3. Put each font's licence next to it; the SIL Open Font License, for one, requires that.

A craft-fonts checkout is already laid out this way, and so is any folder with a
`fonts/manifest.txt` and the files it lists: `PHOTOCRAFT_WEB_FONTS_DIR=<that folder>
packaging/web/package.sh` copies the manifest, the fonts and their licences into the zip, keeping
their paths. Without `fonts/manifest.txt` (a 404) nothing changes.

Serving: let `fonts/manifest.txt` revalidate (`Cache-Control: no-cache`) so edits show up; cache
the font files for long only if their names change with their contents. Serve `.ttf` as `font/ttf`
and `.otf` as `font/otf`, and compress them (Brotli roughly halves a TrueType file). The app
fetches them with `fetch()`, so a Content Security Policy needs them allowed by `connect-src`.

**Fonts from the visitor's device.** In browsers with the Local Font Access API (Chromium on
desktop), Type › Use Fonts from This Device asks the browser for the installed fonts; the browser
shows its own permission prompt. The families then appear in the font menus, and a family's file
is read from the device only when it is picked or a document's text needs it. Nothing is uploaded
and the host needs to do nothing. A site that sends a `Permissions-Policy` header must not deny
`local-fonts`. Elsewhere the menu item is greyed and the served fonts above are the way.

## Autosave and crash recovery

The web build autosaves like the desktop app (Preferences › File Handling: on by default, every
10 minutes, for documents with unsaved changes), into the IndexedDB database `photocraft.recovery`
of the site's origin. Nothing leaves the device.

- One snapshot per document (a `.pcraft` bundle), replaced by each autosave and deleted when the
  document is saved or closed. At most 8 snapshots are kept; the oldest leftovers of earlier
  sessions go first, never an open document's.
- After a crash, a reload or a closed tab, the next start shows a bar above the app: "Unsaved work
  from your last session: …" with **Restore** and **Discard**. Restored documents open unsaved.
  With the preference `fileHandling.recoverOnLaunch` off (`prefs.set`), the bar is not shown.
- When the browser's storage quota is full, the status bar says "Autosave failed: the browser's
  storage for this site is full…" and the document stays open and unsaved.
- Without IndexedDB (private windows in some browsers, blocked site data, some cross-origin
  iframes) the app runs without autosave and says so in a notice ("Autosave is off in this tab");
  once the notice is closed it stays away.
- A snapshot is encoded on the page's main thread, so a very large document pauses the app for a
  moment at each autosave.

Clearing the site's data in the browser deletes the snapshots.
## Installable and offline

The page links a web app manifest and registers a small service worker (`sw.js`), so browsers
offer to install PhotoCraft, and after the first visit it starts without a network.

- The worker caches the site's own files **as the page loads them** (nothing is listed by name).
  Hashed files and the AI worker's folder are served cache-first; `index.html`,
  `manifest.webmanifest`, `ai-worker.txt` and `fonts/manifest.txt` are fetched from the network
  first, with the cached copy as the offline fallback. It never touches requests to other hosts,
  and never the caches the app keeps itself (downloaded AI models, autosaves).
- **Serve `sw.js`, `manifest.webmanifest` and `ai-worker.txt` with `Cache-Control: no-cache`.**
  The sample `_headers`, `.htaccess` and `nginx.conf` do; a blanket "cache every `.js` forever"
  rule would also catch `sw.js`.
- Service workers need a **secure context** (https, or `http://localhost`). Elsewhere the app
  runs as before, just not offline.
- The worker's scope is the folder the site is served from, so it works under a sub-path and
  does not affect other pages on the same host. Serve `sw.js` from that folder (the default).
- `?nosw` in the URL runs the app without the worker and removes one registered earlier. Use it
  when troubleshooting a stale copy. `sw.js` is the one hand-written script in the site.

## AI models and the AI worker

The AI features (Remove Background, Select Subject, Object Selection, AI Upscale and the Remove tool with a model) run **on the
visitor's device**. Nothing is uploaded, and the site ships no model.

| File | What it is |
|---|---|
| `ai-worker.txt` | One line: the name of the folder below. Serve it `no-cache` (the app also asks for it uncached). |
| `ai-worker-<checksum>/photocraft-ml-worker_bg.wasm` | The model runtime, a second wasm of about 14 MiB (3.6 MiB gzipped). Loaded only when a model is first used. |
| `ai-worker-<checksum>/photocraft-ml-worker.js`, `…_loader.js` | Its generated glue and loader. |

The folder is named after the wasm's checksum, so everything in it can be cached forever like the
app's own hashed files; the rules for `.wasm` and `.js` above already cover it. Each wasm has the
host's per-file cap to itself, and `packaging/web/package.sh` checks both.

- **Models come from a third-party host.** When a visitor asks for a model, the browser downloads
  it from `huggingface.co` (which redirects to its CDN), checks its SHA-256 and keeps it in Cache
  Storage. A site with a Content Security Policy must allow that in `connect-src`:
  `connect-src 'self' https://huggingface.co https://*.hf.co`. The worker needs
  `worker-src 'self'` and `script-src 'self' 'wasm-unsafe-eval'` (as the app itself does).
  The models, their licences and pinned URLs: `docs/ai-models.md` in the repository.
- **A secure context is required** (https, or `http://localhost`): Cache Storage and the checksum
  need it. Over plain http the app says so, and the built-in (non-AI) methods keep working.
- **Without the worker files** (deleted, or blocked by the host) the app works as before; asking
  for a model then reports that the site has no AI worker.
- The worker uses WebAssembly SIMD (every current browser; Safari from 16.4). The app itself does
  not need it.

## Embedding in a page (iframe)

```html
<iframe
  src="https://example.com/photocraft/"
  title="PhotoCraft image editor"
  style="width: 100%; height: 720px; border: 0;"
  allow="fullscreen; clipboard-read; clipboard-write"
  allowfullscreen>
</iframe>
```

- The app fills the iframe and follows its size, so size the iframe and not the app.
- Keyboard shortcuts go to the iframe after the user clicks into it, as with any embedded app.
- **Cross-origin embeds** work. Preferences are kept in the iframe's `localStorage`. Browsers
  that partition or block third-party storage may forget them between visits, and the app
  then starts with defaults.
- **Sandboxed iframes** need at least
  `sandbox="allow-scripts allow-same-origin allow-downloads allow-popups"`. Without
  `allow-same-origin` there's no storage. Without `allow-downloads`, Save and Export (browser
  downloads) are blocked.
- Don't send `X-Frame-Options: DENY` or a `frame-ancestors` CSP that excludes the embedding page.

## Renderer selection and fallback flags

PhotoCraft renders with wgpu. It uses **WebGPU** when the browser has it and falls back to
**WebGL2** on its own. URL query flags override this, and they work on the iframe `src` too:

| Flag | Effect |
|---|---|
| *(none)* | WebGPU if available, otherwise WebGL2 |
| `?webgl` | Force the WebGL2 backend (useful when a WebGPU driver misbehaves) |
| `?cpu` | Force the CPU canvas path (slowest, most compatible) |

For example: `<iframe src="https://example.com/photocraft/?webgl" ...>`.

A browser with neither WebGPU nor WebGL2 gets a message in place of the app.
