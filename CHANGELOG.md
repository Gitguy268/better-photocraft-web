# Changelog

Changes of the Better-Photocraft-Web edition. Upstream's own history is in its release notes
(<https://github.com/storytold/photocraft/releases>). `#n` are upstream issue numbers.

## 0.7.0-better.1 (unreleased)

Built on official PhotoCraft 0.7.0 (`v0.7.0`, `79261615`). Where each item stands, and what was
not finished, is in [`docs/better-roadmap.md`](docs/better-roadmap.md).

### Added

- **Liquid Glass theme**, dark and light, **the default**: translucent panels over a blurred view
  of the canvas, and glass panes for menus, dialogs, cards and floating panels whose rims work as
  a lens. The rim bends the canvas behind it (Snell refraction over its depth), splits colours
  slightly, and catches a Fresnel sheen and glare, after
  [liquid-glass-studio](https://github.com/iyinchao/liquid-glass-studio) (MIT). Panel bodies stay
  frosted enough that all text keeps 4.5:1 contrast on any image. Preferences saved by an earlier
  build move from the old defaults (Pro Medium, Studio Light) to Liquid Glass once. A *Reduce
  transparency* preference gives opaque surfaces; it is forced on the CPU canvas path and when the
  browser asks for reduced transparency or more contrast. Works on WebGPU and WebGL2.
- **Local AI for Remove Background, Select Subject and Object Selection.** A model that runs on
  the device (U²-Netp by default, 4.4 MB; IS-Net as a larger option) finds the subject. The app asks
  before the first download and shows the model's name, size and licence; nothing is uploaded, and
  without a model the classical method runs as before. In the browser the model runs in a Web
  Worker from a second wasm, so the page never freezes. (#4098, #2788, #2804, #2313)
- **AI Upscale** (Filter › AI › AI Upscale): enlarges the document 2x or 4x with a local
  Real-ESRGAN model (4.7 MB, BSD-3-Clause), tile by tile so memory stays bounded. Masks, type,
  shapes and effects are resized as Image Size resizes them; pixel layers are rendered by the
  model. Without a model it resamples with Preserve Details. (#2942)
- **AI Denoise** (Filter › AI › AI Denoise): removes noise and grain from the active pixel layer
  with a local SCUNet model (74 MB, Apache-2.0), tile by tile, in one undo step. Without a model it
  runs Reduce Noise with its defaults. RGB and grayscale documents; the selection is not used yet.
- **Object Selection by clicking:** with MobileSAM installed from Filter › AI › AI Models (43 MB,
  Apache-2.0), a click selects the object under the pointer (⇧ adds, ⌥ subtracts); further clicks
  on the same image reuse its embedding. `select.object` takes `"point": [x, y]`.
- **Plain-language commands in Edit › Search (⌘F):** "blur 5", "make it brighter", "remove the
  background", "upscale 4x", "rotate left" (English and Dutch phrases) become the first row and run
  with their numbers, without a dialog. Keyword matching on the device, no language model.
- **Remove tool with a local inpainting model.** With LaMa installed from Filter › AI › AI Models
  (198 MB, Apache-2.0) the Remove tool fills what you paint over with the model; without it the
  patch-based fill runs as before. `paint.remove` takes `"model": "auto" | "classical" | "lama"`.
  RGB and grayscale documents, on opaque pixels; a stroke takes 10 to 25 s in the browser.
- **Remove Background options:** output as a layer mask (default), a new layer, a new layer with a
  mask, or a cut-out; edge refine, feather and colour decontamination. One undo step.
- **AI model manager** (Filter › AI › AI Models) and the commands `ai.models`, `ai.model.install`,
  `ai.model.remove` for the CLI and MCP.
- **Contextual Task Bar** under the canvas with Remove Background, Select Subject, AI Upscale and
  AI Denoise, with every tool; it steps aside during a drag on the canvas (Window › Contextual
  Task Bar).
- **Web: autosave and crash recovery** in IndexedDB, with a restore prompt at start-up.
- **Web: installable and offline** (web app manifest and a small service worker; `?nosw` turns it off).
- **Web: fonts from the device** (Type › Use Fonts from This Device), where the browser has the
  Local Font Access API. The browser asks first; a family's file is read only when it is used.
  (#3336, #3377 on the web)
- **Web: File System Access**: where the browser has it, Open keeps the file handle so Save writes
  back to the same file, and Save As shows the native picker; elsewhere downloads as before.
- **Color Replacement tool** (#1045), **Add / Delete Anchor Point tools** (#1044, #4144) and the
  **Free Transform right-click menu** (#2832): backports of upstream's own later commits.

### Changed

- Licence: MIT only (upstream is MIT OR Apache-2.0).
- The ArtCraft logos and the Discord promotion are removed, as upstream's brand terms require of
  modified versions. The product is still PhotoCraft; About and the README credit the official
  project. Help links to upstream and to this edition's repository.
- The browser build stores egui's fallback fonts deflated: 598 KB less wasm, which is what made
  room for the features above under the 24 MiB size gate.
- The web release zip is named `better-photocraft-web-<version>.zip`.
