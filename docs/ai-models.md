# AI models

PhotoCraft's AI features run **on the user's device**. No model is committed to this repository or
baked into the app: each one is downloaded on request from the pinned URL below, checked against
its SHA-256, and cached (IndexedDB in the browser, the config directory on desktop). The app asks
before the first download and shows the model's name, size and licence. Everything works without
any model: each feature then uses its classical algorithm.

Policy (owner decision, answering upstream's open decision #41 in `docs/gaps.md`): **local first,
no cloud calls by default, permissively licensed models only.** A model is listed here only when
both the weights' host and the original project state a permissive licence (MIT, Apache-2.0 or
BSD). A model may come as two files (its graph and its weights); each has its own pinned URL and
SHA-256. Models with non-commercial, research-only or unclear terms are not used; see
[Rejected](#rejected).

The table is the source of truth for `crates/ml/src/registry.rs`; a test there fails if the two
disagree. URLs pin a commit, so a file can't change under its hash.

## Models

Licences were read on the model card and on the original project's repository on **2026-10-12** (SCUNet on **2026-10-14**).

| Id | Model | Used for | Licence | Size | SHA-256 |
|---|---|---|---|---:|---|
| `u2netp` | U²-Netp (U-2-Net small) | Remove Background, Select Subject (default, fast) | Apache-2.0 | 4,574,861 B (4.4 MiB) | `309c8469258dda742793dce0ebea8e6dd393174f89934733ecc8b14c76f4ddd8` |
| `isnet` | IS-Net general use (DIS) | Remove Background, Select Subject (high quality) | Apache-2.0 | 178,648,008 B (170 MiB) | `60920e99c45464f2ba57bee2ad08c919a52bbf852739e96947fbb4358c0d964a` |
| `realesrgan` | Real-ESRGAN general x4 v3 (compact) | AI Upscale 2x / 4x | BSD-3-Clause | 42,443 B (graph) + 4,836,096 B (weights, 4.6 MiB) | `2d1810f8b75c6a899487decf933697241eafbe6327d7b723674ba6df73a08fda`, weights `512d0ec9940c2e9d85d27f2952f12a0b77b7841dc22df4ce9f3ea458bc98f37f` |
| `lama` | LaMa (big-lama, 512 px, fp32) | The Remove tool (inpainting) | Apache-2.0 | 208,044,816 B (198 MiB) | `1faef5301d78db7dda502fe59966957ec4b79dd64e16f03ed96913c7a4eb68d6` |
| `mobilesam` | MobileSAM: the image encoder, then the single-mask prompt decoder as its second file | Object Selection by clicking | Apache-2.0 | 28,157,093 B (encoder) + 16,501,323 B (decoder, 16 MiB) | `580f5fb648ea1062c0aabc26217aed56921985f03f0cbbd852bba81d760cc749`, decoder `93915fc7c993ab9d59ab8c9ccd3bce37f7509c81ab4150a74abd4d2abbd8570d` |
| `scunet` | SCUNet (Swin-Conv-UNet), blind real-image denoising, PSNR variant | AI Denoise | Apache-2.0 | 3,798,678 B (graph) + 73,138,176 B (weights, 70 MiB) | `231be201ab413dbc999d7951caa9844846b93a12a40a41e037d6b5888ed4e88c`, weights `98825ea1210b641c71e5f052f582c70c49fd44b35387ebe2c034268c17df3feb` |

Sources:

| Id | Download URL (pinned commit) | Weights host says | Original project |
|---|---|---|---|
| `u2netp` | `https://huggingface.co/BritishWerewolf/U-2-Netp/resolve/7112208dbac3a3642496c8d54e2f0f9bb3dc1dc8/onnx/model.onnx` | `license: apache-2.0` | [xuebinqin/U-2-Net](https://github.com/xuebinqin/U-2-Net/blob/master/LICENSE), Apache-2.0 |
| `isnet` | `https://huggingface.co/BritishWerewolf/IS-Net/resolve/9783722d9f964c0286a411e7e8e6fede947d5a53/onnx/model.onnx` | `license: apache-2.0` | [xuebinqin/DIS](https://github.com/xuebinqin/DIS/blob/main/LICENSE.md), Apache-2.0 |
| `realesrgan` | `https://huggingface.co/TheGuy444/Real-ESRGAN-ONNX/resolve/4c80f3da1a8a85f0757a9e7f79e08d0307220eb5/onnx/model.onnx` and, for the weights, `https://huggingface.co/TheGuy444/Real-ESRGAN-ONNX/resolve/4c80f3da1a8a85f0757a9e7f79e08d0307220eb5/onnx/model.data` | `license: bsd-3-clause`. The graph was exported with Qualcomm AI Hub (it says so) from the compact `realesr-general-x4v3` network | [xinntao/Real-ESRGAN](https://github.com/xinntao/Real-ESRGAN/blob/master/LICENSE), BSD-3-Clause |
| `lama` | `https://huggingface.co/Carve/LaMa-ONNX/resolve/c3c0c9e468934d62e79c329e35d82dd09ff8c444/lama_fp32.onnx` | `license: apache-2.0` | [advimman/lama](https://github.com/advimman/lama/blob/main/LICENSE), Apache-2.0 |
| `mobilesam` | `https://huggingface.co/Acly/MobileSAM/resolve/0d3b403339b4674a82493d5e97964dd78089ddc8/mobile_sam_image_encoder.onnx` and, for the decoder, `https://huggingface.co/Acly/MobileSAM/resolve/0d3b403339b4674a82493d5e97964dd78089ddc8/sam_mask_decoder_single.onnx` | `license: mit`. The encoder was exported with the repository's `export_image_encoder.py` with preprocessing (it takes `H × W × 3` pixels and normalises and pads itself) | [ChaoningZhang/MobileSAM](https://github.com/ChaoningZhang/MobileSAM/blob/master/LICENSE), Apache-2.0 |
| `scunet` | `https://huggingface.co/Heliosoph/scunet-onnx/resolve/6d11417ee2fbcc73783c502a238ac115097754fe/scunet_color_real_psnr.onnx` and, for the weights, `https://huggingface.co/Heliosoph/scunet-onnx/resolve/6d11417ee2fbcc73783c502a238ac115097754fe/scunet_color_real_psnr.onnx.data` | `license: apache-2.0` (its `LICENSE.txt` is KAIR's MIT). Exported with `torch.onnx.export` from upstream's `scunet_color_real_psnr.pth`, as the card describes | [cszn/SCUNet](https://github.com/cszn/SCUNet/blob/main/LICENSE), Apache-2.0 |

Both hosts answer cross-origin requests (`Access-Control-Allow-Origin` on the redirect and on the
file), which the browser build needs. A site with a Content Security Policy must allow
`https://huggingface.co` and `https://*.hf.co` in `connect-src` (see `packaging/web/README.md`).

## Candidates checked, not shipped yet

Verified the same way; they move into the table above when their feature ships and passes its
checks. `docs/better-roadmap.md` says where each one stands.

| Model | For | Licence | Size | Pinned source | SHA-256 |
|---|---|---|---:|---|---|
| MODNet | portrait matting | Apache-2.0 ([ZHKKKe/MODNet](https://github.com/ZHKKKe/MODNet/blob/master/LICENSE)) | 25,888,640 B | `Xenova/modnet@fa2fa546052fba4c08921230a26cc69a333fca12` `onnx/model.onnx` | `07c308cf0fc7e6e8b2065a12ed7fc07e1de8febb7dc7839d7b7f15dd66584df9` |
| BiRefNet lite (fp16) | highest-quality background removal | MIT ([ZhengPeng7/BiRefNet](https://github.com/ZhengPeng7/BiRefNet/blob/main/LICENSE)) | 114,538,221 B | `onnx-community/BiRefNet_lite-ONNX@de15b22ba131738a16dff04aab8bdf8dc32e3ac1` `onnx/model_fp16.onnx` | `d39b897ceb16ae654c1731f3dba0cf9b368d9cae74b5a57459b455cc8bfec402` |
| Real-ESRGAN x2 / x4 (the large RRDB network; not used: about 30 s per tile on the CPU) | AI Upscale | BSD-3-Clause ([xinntao/Real-ESRGAN](https://github.com/xinntao/Real-ESRGAN/blob/master/LICENSE)) | 67,073,434 B / 67,051,616 B | `SceneWorks/real-esrgan-onnx@09f741bac80a246b407da3ee902bf5f3291b602f` | `7115ba92e8a1bfa63d68558ef006ef3d91273a068d321b1439f8bb1c9179002c`, `5c586662929cbc686c1a5c38d9c060dbdb4ea5863a1f7672b8c0761e6b89c033` |

## Rejected

| Model | Why |
|---|---|
| Bria RMBG 1.4 / 2.0 | Non-commercial licence. |
| `onnx-community/ISNet-ONNX` (a different IS-Net conversion) | Its card says AGPL-3.0, which is not MIT-compatible; the Apache-2.0 conversion above is used instead. |
| Swin2SR conversions without a licence on the card | Unclear terms. |

## Adding a model

1. Check the licence on the weights' card **and** on the original project. Both must be permissive.
2. Pin the URL to a commit, record the size and SHA-256 here and in `crates/ml/src/registry.rs`.
3. Check the host sends `Access-Control-Allow-Origin` (`curl -I -H 'Origin: http://localhost:8780' <url>`), or the browser build can't fetch it.
4. Add a row to `ATTRIBUTION.md`. Never commit the file itself.
