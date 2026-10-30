# Better-Photocraft-Web roadmap

What this edition adds on top of official PhotoCraft 0.7.0, why each item was picked, and where it
stands. Statuses are only `done` when the feature was run and seen working in the web build;
anything not finished or not verified says so.

- Base: `v0.7.0` (`79261615`). Decisions and measurements: [`decisions.md`](decisions.md).
- Models: [`ai-models.md`](ai-models.md).
- `#n` are upstream issues (storytold/photocraft); `G…` are entries in [`gaps.md`](gaps.md);
  `TOOL-…`, `UI-…`, `AUTO-…` are rows in [`scorecard.md`](scorecard.md).

## How the list was made (2026-10-12)

Sources, read-only: `docs/gaps.md`, `docs/scorecard.md` (114 open rows), the parity docs,
`packaging/web/README.md`, and upstream's open issues (850 open; ranked by reactions ×2 + comments
through the search API). Each candidate was then checked against the 0.7.0 source, and against
upstream `main` (418 commits ahead) to see whether upstream had already built it.

What that showed:

- The top-voted issues are mostly outside this edition's scope (Homebrew, Flathub, winget, the
  Windows taskbar bug, DDS, JPEG XL, GIMP files). The web build's own gaps come from
  `packaging/web/README.md` rather than from votes, because few people run the web build yet.
- **AI is asked for repeatedly:** #2313 (downloadable models), #4098 and #2788 (local background
  removal and Select Subject), #2804 (BiRefNet/rembg), #2942 (AI upscale), #2943 (AI denoise),
  #2682, #41 (generative). `gaps.md` G15 and scorecard AUTO-219-7 say the same: no ML runtime.
- **Already in 0.7.0, so not on the list:** Commit and Cancel buttons in the Free Transform
  options bar (#2442 is fixed: `transform_tool.rs`, "cancel and commit cluster").
- **Already built upstream after 0.7.0:** Color Replacement tool, Add/Delete Anchor Point tools,
  the Free Transform right-click menu, and most of the Select and Mask workspace. These are
  **backported** (cherry-picked from upstream's own commits) instead of rewritten.

## The list

| # | Item | Answers | Kind | Status |
|---|---|---|---|---|
| 1 | **Liquid Glass theme** (GPU blur-behind, lens rims, Reduce Transparency), **the default** | UI-217-11, #2661 | new | **done.** 2026-10-14: the first version was only a frosted blur, so it read as a plain dark theme, and the web build never showed it, because the web shell forced Pro on every start (and saved that as the user's choice). Now the rims of every pane are a lens after [liquid-glass-studio](https://github.com/iyinchao/liquid-glass-studio) (MIT): Snell refraction of a sharp copy of the scene over the rim's depth, colour dispersion, a Fresnel sheen and glare. Panel bodies stay frosted (about 80 % tint) so all text keeps 4.5:1 on any image, so the lens shows in the outer 5 pt and under the tint. Liquid Glass is the default; preferences saved before version 2 with the old defaults (Pro, Pro Medium, Studio Light) move to it once. Seen in offscreen snapshots (dark, light) and in headless Chromium on a fresh profile, on WebGPU, WebGL2 (`?webgl`) and the opaque CPU fallback (`?cpu`); a version 1 file holding `pro` opens in Liquid Glass |
| 2 | **Remove Background with a local model** (U²-Netp default, IS-Net optional) | #4098, #2788, #2804, #2313, AUTO-219-7, G15 | new | **done** with U²-Netp: prompt, download, checksum, worker run and mask seen in the browser; in-process run tested natively. **IS-Net** (170 MB): downloaded with the progress bar and run in the browser too; the whole command finished in under 15 s |
| 3 | **ML Select Subject / Object Selection** | #2788, G15, P29 | new | **done** in the engine (tests with model outputs, both commands). **Click-to-segment (MobileSAM): done** 2026-10-14. With MobileSAM installed (Filter › AI › AI Models; 43 MB in two files, Apache-2.0), a click with the Object Selection tool selects the object under it (⇧ adds, ⌥ subtracts); a drag still runs the rectangle. `select.object` takes `"point": [x, y]`. The encoder sees the whole image scaled to 1024 px and padded with the pixel mean; its embedding is cached, so further clicks on the same image run only the decoder. Natively 1.3 s for the first click and 65 ms after; in headless Chromium about 3 to 4.5 s for the first click (the worker loads the model) and under 3 s for the next. Seen in the browser on *The Great Wave*: one click selected the wave, a ⇧-click added the sky. **Limits:** the tool never offers the download (it is installed from AI Models, as for LaMa); without the model a click is a Quick Selection dab as before; only the decoder's 256 px mask is used, so edges come from the edge refinement |
| 4 | **AI Upscale 2x / 4x**, tiled | #2942, G15 | new | **done.** Filter › AI › AI Upscale with the compact Real-ESRGAN model (4.7 MB in two files, BSD-3-Clause). Seen in the browser: dialog, prompt, both files downloaded and checked, tiles run in the worker behind a progress bar with Cancel, a 304 × 360 image enlarged to 608 × 720 in about 13 s with no visible seams; in process the real model is covered by an engine test. **It is slow on large images:** about 1.1 s per 128 px tile in the browser and 0.4 s natively, so a 12-megapixel layer is roughly 950 tiles: about 17 minutes in the browser, 6 natively. Cancel works at any tile. RGB and grayscale documents only |
| 5 | **AI Remove (inpainting) behind the Remove tool** | G15, TOOL-213-19 | new | **done.** With the LaMa model installed (Filter › AI › AI Models; 198 MB, Apache-2.0) the Remove tool fills its stroke with it; without it, or with `"model": "classical"`, the patch-based fill runs as before. Only the stroke's pixels change, in one undo step. Seen in the browser on a 1280 px photo: a ring drawn around the Earth in *Earthrise* left clean black sky, and a ring around a lunar crater left plausible ground with no visible seam; the first stroke took between 15 and 25 s (it loads the model), the next one 10 to 12 s, behind a progress window with Cancel; Undo brought the object back. In process the real model is covered by an engine test (a separate native probe on an Apple M4 measured 3.7 s to load the model and 3.5 s per run). **Limits:** the model sees the stroke's surroundings scaled to 512 × 512, so a fill wider than about 500 px comes out soft; RGB and grayscale documents only, and only where the pixels around the stroke are fully opaque (elsewhere the built-in fill runs, which also completes transparency); once the model is installed every such stroke uses it, with no switch in the tool's options (scripts pass `"model": "classical"`); a second stroke while one is being filled is refused; it needs about 2 GB of memory; the tool never offers the download itself (a dialog would interrupt painting). In this check the model file was served from a local copy of the pinned file, because this machine's name lookups were failing at the time; the browser's download from the same host was seen for the three other models |
| 6 | **AI Denoise** | #2943 | new | **done.** Filter › AI › AI Denoise and the Contextual Task Bar: SCUNet (blind real-image, PSNR variant; 74 MB in two files, Apache-2.0) on the active pixel layer, tile by tile through the same job as AI Upscale at 1x, one undo step; without a model, Reduce Noise with its defaults. Seen in headless Chromium: the prompt (name, size, licence), both files downloaded and checked, the worker run, and History's "AI Denoise" step; a 640 × 430 image took about 27 s, **about 1.1 s per 128 px tile**, so a 12-megapixel layer is roughly 17 minutes in the browser. In process, an engine test with the real model takes noise from 0.058 to 0.034 (standard deviation); the PSNR variant is conservative on fine grain. **Limits:** the selection is ignored; RGB and grayscale only |
| 7 | **Web autosave and crash recovery** (IndexedDB, restore prompt) | `packaging/web/README.md`, #3163 | new | **done.** Seen in the browser: two brush strokes, a one-minute autosave, reload, the "Unsaved work from your last session" bar, Restore, and the strokes are back |
| 8 | **Installable, offline PWA** | web gap | new | **done.** Service worker activates; the app starts and opens an image with the server stopped |
| 9 | **File System Access open and save** with a download fallback | web gap, #2223 on the web | new | **done.** Checked in the browser with the picker replaced by a real file handle (the origin's private file system): Save As writes a valid PSD or PNG into the handle; after an edit, Save rewrites the same PSD without asking again. For flat formats (PNG, JPEG) Save still asks each time, as official 0.7.0 does on desktop (#2223). The native picker dialogs themselves can't be driven headlessly |
| 10 | **Web fonts:** Local Font Access, documented manifest flow | web gap, #3336, #3377 | new | **done.** Type › Use Fonts from This Device lists the device's families; a family's file is read when it is picked. Seen in the browser with the font list supplied by a script holding a real font file (the permission prompt can't be driven headlessly): the family appears in the font menu and text is set in it. The `fonts/manifest.txt` flow was already documented by upstream and is unchanged |
| 11 | **Color Replacement tool** | #1045, G6 | backport `073e1cc1` | **done** |
| 12 | **Add / Delete Anchor Point tools** | #1044, #4144, G6 | backport `d96bb501` | **done** |
| 13 | **Free Transform right-click menu** | #2832, TOOL-214-5, G5 | backport `d79fc5e8` | **done** |
| 14 | **Select and Mask workspace** (live view modes, Refine Edge Brush, output to new document) | #1754, #2972, #2998, #2671, TOOL-214-1, G7 | backport | not started (the three upstream commits conflict in 16 catalogs and three source files) |
| 15 | **Art History Brush** | G6 | new | not started |
| 16 | **Large-brush lag** | #2619, #1994, G4 | new | not started |
| 17 | Natural-language command bar | #2682 (in part) | new | **done, small.** Edit › Search (⌘F) turns a phrase with a number or two words ("blur 5", "make it brighter", "remove the background", "upscale 4x", "rotate left"; English and Dutch) into one command with its parameters as the first row, run without a dialog. Keyword and synonym matching on the device, no language model; seen in headless Chromium ("blur 5" applied Gaussian Blur 5) |
| + | **Contextual Task Bar** (minimal) | G9 | new | **done** (Remove Background, Select Subject, AI Upscale, AI Denoise, options, hide). Since 2026-10-14 it shows with every tool, the default Brush included (before, the AI was invisible on a first start), and steps aside while a drag that began on the canvas is under way |

Cut order if time or the wasm size budget runs out: 14, 15 (the others are done). Items 1, 2 and 7 to 13 are not cuttable, and all of them are done.

## Remove Background on the test set

Run in the web build (headless Chromium, the model in the worker), on public-domain paintings,
one click with the default options. Judged by eye.

| Kind | Image | U²-Netp (4.4 MB, about 1 s) | Notes |
|---|---|---|---|
| Portrait | Vermeer, *Girl with a Pearl Earring* | partly | Head and headscarf kept; the dark jacket against the dark ground is lost |
| Portrait | Leonardo, *Mona Lisa* (with **IS-Net**) | good | Clean silhouette including the veil; under 15 s with the 170 MB model |
| Still life | van Gogh, *Sunflowers* | good | Some pale background remains between the flowers |
| Hair, several figures | Botticelli, *The Birth of Venus* | good | Figures, flowing hair and the shell kept; sea and trees removed |
| Glass | Heda, *Still Life with Gilt Goblet* | acceptable | The table setting is kept as one subject. Glass is treated as solid, not as transparent; the pewter jug and a small glass fade |
| Product-like | van Gogh, *A Pair of Shoes* | good | The shoes are isolated; a soft shadow stays half-transparent |
| Low contrast | Whistler, *Nocturne in Black and Gold* | **nothing found** | The command reports that no subject was found and leaves the document unchanged |

## Constraints that shape the work

- **Wasm size.** 0.7.0 leaves 176,435 bytes under the 24 MiB gate; deflating egui's fallback
  fonts raised that to 774,566 bytes. Every item reports its cost;
  the ML runtime lives in a second, lazily loaded wasm (`decisions.md`).
- **Models** are downloaded on request, never committed or built in.
- **Backports** keep upstream's code and behaviour, so a later rebase drops them cleanly.

## Log

| Step | Main wasm (bytes) | Under the 25,165,824 gate |
|---|---:|---:|
| Official 0.7.0, built locally | 24,989,389 | 176,435 |
| egui fallback fonts deflated | 24,391,258 | 774,566 |
| AI commands, download prompt, browser bridge, task bar | 24,487,564 | 678,260 |
| + Liquid Glass, backports, autosave, File System Access, PWA | 24,622,990 | 542,834 |
| + fonts from the device | 24,637,747 | 528,077 |
| + AI Upscale | 24,654,491 | 511,333 |
| + LaMa behind the Remove tool | 24,664,985 | 500,839 |
| + lens glass, glass default, AI Denoise, plain-language commands, task bar with every tool | 24,685,585 | 480,239 |
| + MobileSAM click-to-segment, click hint, stricter phrases (final) | 24,694,851 | 470,973 |

The AI worker is a separate file: 14,653,349 bytes (its own allowance).

`cargo xtask perf --quick`, before and after, on the same machine (Apple M4): the median p50 change
over 41 scenarios is +1.4 %; single rows move between −12 % and +26 % on scenarios of 1 to 40 ms
with three samples each, which is the noise of a quick run. The benches use the default theme, so
they show that the other themes pay nothing, not what Liquid Glass costs. The bevel rows that moved
most (+19 to +26 % in that single run) were re-run three times on the final build: the GPU ellipse
row is back at its baseline (13.6 to 13.9 ms against 13.9), and the text and CPU rows sit 0 to 14 %
above their single baseline sample. No change in this edition touches that path; the difference is
not explained and is under the tool's 15 % regression threshold.

**A limit on the web with large images.** Only the model itself runs in the worker. Reading the
image for the model and refining the mask edge run on the page's thread, because the engine's jobs
are inline on wasm (as in official 0.7.0). On a 9.9-megapixel image (3840 × 2581) the longest
stall measured during Remove Background was 1.5 s, at the end; it grows with the image.

Why work stopped where it did: the build used three parallel workstreams, and the account's usage
limits ended them twice mid-task and then for the week. What was committed was merged and
re-verified; the items marked "not started" were never begun, not abandoned half-way.
