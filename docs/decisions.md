# Implementation decisions

## 2026-10-10: Smart Object contents identity (#2163)

Smart Object instances carry a document-scoped contents ID separately from their layer ID. Normal duplicates preserve it; New Smart Object via Copy creates a new one. Contents edits update the matching instances together, retaining their individual placement and filters. Copies into another document remap contents IDs as a batch, preserving sharing within the copied layers without joining existing destination objects.

Native files persist this ID. Legacy embedded objects without an ID remain independent: identical file bytes do not prove that two objects were intended to share edits. Legacy nonempty linked paths and PSD source identifiers retain their explicit sharing relationship. PSD export keys embedded sources by contents identity as well as bytes, so independent equal-content objects stay independent.

## 2026-10-12: Better-Photocraft-Web base and ground rules

This edition is built on the official PhotoCraft release **0.7.0**: tag `v0.7.0`, commit
`79261615956ab21334e49ca1d384390272e647da`. That is the source of the official
`photocraft-web-0.7.0.zip`; a local build of the tag gives a 24,989,389-byte wasm against the
official zip's 24,996,331 (Rust 1.99.0, trunk 0.21.14, wasm-bindgen 0.2.129, binaryen 133).

**Rebasing later.** Upstream `main` was 418 commits ahead on this date (version 0.8.1, 755 files,
about +104,000/−8,500 lines), with heavy churn in the files this edition also edits:
`engine/src/commands.rs`, `ui-egui/src/menus.rs`, `state.rs`, `theme.rs`, `gpu_canvas.rs` and
`apps/photocraft-web/src/web.rs`. Moving to a newer tag means `git rebase --onto <tag> v0.7.0 main`
(or merging the tag), resolving conflicts in those shared files, dropping every backport listed in
`docs/better-roadmap.md` whose upstream commit is already in the new base, re-applying the licence
and branding commit by hand (upstream keeps its Apache licence file and Discord references), then
re-running the whole gate (`cargo xtask layers`, `wasm`, `parity`, `scorecard`, the web size gate)
and re-measuring the wasm budget below. Not done now, by the owner's decision.

**Backports instead of rewrites.** Where upstream already built a wanted feature after 0.7.0, this
edition cherry-picks upstream's own commit (same licence, same behaviour as the next official
release, nothing to throw away at the next rebase) and says so in `docs/better-roadmap.md`. That
is not a rebase: the base stays 0.7.0.

**Wasm size budget.** `packaging/web/package.sh` fails above 25,165,824 bytes (24 MiB). The 0.7.0
build leaves only **176,435 bytes** of room, so the first change made room: the browser build
embeds egui's four fallback fonts deflated (inflated once at startup) instead of raw, as upstream
already does for Inter and the translation catalogs. That took the wasm from 24,989,389 to
**24,391,258 bytes: 774,566 bytes of room**. Every feature commit re-measures it. No ML runtime
is linked into the main wasm (see the runtime decision below).

**Publishing is off.** The only remote is `upstream`, its push URL is `DISABLED`, a `pre-push` hook
refuses every push, and there is no `origin`. `PUBLISHING.md` says how to undo that.

**Commit attribution.** Commits end with the `Co-Authored-By` line Claude Code is configured to
add. No `Claude-Session` line is configured in this installation, so none is written.

## 2026-10-12: ML runtime: `tract`, in a separate worker wasm

**Constraints found.** (1) The main wasm has 176 KiB of room under the size gate, so no inference
runtime can be linked into it. (2) On the web, engine jobs run inline on the UI thread
(`crates/engine/src/jobs.rs`), and upstream uses no threads or `SharedArrayBuffer`, so inference
inside the app would freeze it. Both are solved by one design: inference runs in a **second wasm,
loaded on demand in a Web Worker** (`apps/photocraft-ml-worker`). The per-file host cap applies to
each file separately. On desktop and in the CLI the same code runs on a job thread through
`jobs::run`.

**Trunk can build it with generated glue only.** `<link data-trunk rel="rust" data-bin="…"
data-type="worker" data-loader-shim />` (trunk 0.21.14) emits `<bin>.js`, `<bin>_bg.wasm` and a
62-byte generated `<bin>_loader.js`. No hand-written JavaScript.

**Measured** (Apple M4, 16 GB; one thread; U²-Netp, 4.4 MiB ONNX; `tract-onnx` 0.23.8; wasm built
with `+simd128`, fat LTO, opt-level 3; headless Chromium via Playwright):

| | Load model | Inference 320 px | Inference 1024 px |
|---|---:|---:|---:|
| Native (release) | 96 ms | 307 ms | 3.4 s |
| Wasm in a Web Worker | 221 ms | 1.06 s | 10.7 s |

Added wasm: **14,631,450 bytes raw, 3,754,048 gzipped, in its own file** (0 bytes in the main
wasm). IS-Net (170 MiB, 1024 px input) runs natively in 1.3 s, with 1.5 GB peak memory. Masks were
checked by eye on public-domain paintings (Vermeer, van Gogh, Leonardo).

**Candidates.**

| Runtime | Verdict | Why |
|---|---|---|
| `tract` 0.23 | **chosen** | Pure Rust, loads ONNX at run time (models are never built in), compiles to `wasm32-unknown-unknown` unchanged, ran both test models correctly. CPU only. |
| `burn` 0.22 (wgpu) | not now | Its WebGPU backend uses wgpu 30, the workspace's version, which makes it the natural GPU path later. But its ONNX import (`burn-onnx`) is a build-time code generator: each architecture would be compiled in, and the weights would need converting, which conflicts with fetching pinned third-party files at run time. It needs compute shaders, so no WebGL2. Not benchmarked. |
| `candle` 0.11 | rejected | No GPU backend for the web, and `candle-onnx` is a partial ONNX evaluator; CPU-only like tract with less operator coverage. Not benchmarked. |
| onnxruntime-web or any JS runtime | not considered | AGENTS.md: Rust only. A Rust option works. |

**Consequences.** WebGPU, WebGL2 (`?webgl`) and `?cpu` all run inference the same way, on the
CPU in the worker, so there is one path to test. The default model runs at its native 320 px
(about 1 s in the browser). Larger models are opt-in and show progress. If a GPU path is wanted
later, `burn` on wgpu 30 inside the same worker is the route; nothing in the engine would change.

**The seam.** `photocraft-ml` (L4) holds the model registry and the pre/post-processing, with the
tract runtime behind its `runtime` cargo feature. The main web wasm builds it without that
feature; the worker, the desktop app and the CLI build it with. Existing commands keep their ids
(`layer.removeBackground`, `select.subject`, `paint.remove`) and gain a model path, falling back
to today's classical algorithm when no model is installed. New commands live in
`crates/engine/src/ai_cmds.rs`.

## 2026-10-12: one hand-written JavaScript file (the service worker)

AGENTS.md forbids hand-written JS. A service worker cannot be WebAssembly: the browser only
accepts a script URL. `apps/photocraft-web/sw.js` is therefore a small static script with no
dependencies and no UI, registered from Rust (`web_sys::ServiceWorkerContainer`); `index.html`
still has no `<script>` of its own (the test in `apps/photocraft-web/src/main.rs` keeps that true).
The owner approved this one exception.
## 2026-10-17: keeping a replaced file's ACL and named streams (#7073, #7094, #7096)

Saving replaces a file by renaming a new one over it, which drops everything the old file
carried. Carrying the security-relevant parts across needs platform calls that have no safe API
in `std`, and the workspace forbids `unsafe`, so each platform took a dependency rather than an
exception.

**macOS ACLs: `exacl`, not `fclonefileat`.** The two routes that avoid `unsafe` are a macOS-only
`exacl` dependency (a safe wrapper over `acl_get_file`/`acl_set_file`) and `fclonefileat` with
the undocumented-in-`rustix` `CLONE_ACL` bit. The clone keeps the ACL, but only on APFS, and it
means writing every save as clone-then-truncate-then-write with a fallback for every other file
system — restructuring the one write path the whole app's durability rests on, to preserve an
ACL. `exacl` is a best-effort step beside the existing permission copy, it works wherever ACLs
do, and a failure cannot affect the bytes.

**Windows: `ReplaceFileW` via `winsafe`, with a backup name.** Copying the `Zone.Identifier`
stream by hand with `std` would cover the Mark of the Web but not the DACL, and enumerating every
stream needs `FindFirstStreamW`. `ReplaceFileW` is the call Microsoft documents for the
save-through-a-temporary-file pattern and preserves the DACL, named streams, creation time and
object identifier in one step; `winsafe`'s `kernel` feature wraps it safely and pulls in nothing.

The backup-file argument is a correctness requirement, not an option: `ReplaceFileW` without one
documents `ERROR_UNABLE_TO_MOVE_REPLACEMENT` as leaving the replaced file deleted and the
replacement under its temporary name — a failed save that loses the user's document. With a
backup name the same failure leaves both files alone, and the one failure that does move things
(`ERROR_UNABLE_TO_MOVE_REPLACEMENT_2`) is unwound in `atomic/meta.rs`.
