//! Running models: pure-Rust ONNX inference on the CPU (tract). One thread; callers put it on a
//! job thread (desktop, CLI) or in a Web Worker (browser).

use std::collections::HashMap;

use tract_onnx::prelude::*;

use crate::{MlError, Model, Tensor};

type Plan = std::sync::Arc<TypedSimplePlan>;

/// Loaded models, ready to run.
#[derive(Default)]
pub struct Runtime {
    plans: HashMap<&'static str, Plan>,
    /// A segmentation model's prompt decoder (its encoder is in `plans`).
    decoders: HashMap<&'static str, Plan>,
    /// The last image a segmentation encoder saw (a hash of its input) and its embedding, so
    /// further clicks on the same image run only the decoder.
    embedding: std::cell::RefCell<Option<(&'static str, u64, TValue)>>,
}

fn check(name: &'static str, sha256: &str, bytes: &[u8]) -> Result<(), MlError> {
    use sha2::Digest as _;
    let hex: String = sha2::Sha256::digest(bytes).iter().map(|b| format!("{b:02x}")).collect();
    if hex == sha256 { Ok(()) } else { Err(MlError::Checksum { name }) }
}

/// Check a downloaded model file against the registry's SHA-256.
pub fn verify(model: &Model, bytes: &[u8]) -> Result<(), MlError> {
    check(model.name, model.sha256, bytes)
}

/// Check a model's separate weights file against the registry's SHA-256.
pub fn verify_data(model: &Model, bytes: &[u8]) -> Result<(), MlError> {
    let data = model.data.ok_or(MlError::Load { name: model.name, reason: "the model has no separate weights file".into() })?;
    check(model.name, data.sha256, bytes)
}

/// Serves a model's "external data" from memory: models are handed over as bytes, never as files
/// (there is no file system in the browser).
struct InMemory(Vec<u8>);

impl tract_onnx::data_resolver::ModelDataResolver for InMemory {
    fn read_bytes_from_path(&self, buf: &mut Vec<u8>, _: &std::path::Path, offset: usize, length: Option<usize>) -> TractResult<()> {
        let end = offset.checked_add(length.unwrap_or(self.0.len().saturating_sub(offset)));
        let slice = end.and_then(|end| self.0.get(offset..end)).ok_or_else(|| TractError::msg("the weights file is shorter than the model expects"))?;
        buf.extend_from_slice(slice);
        Ok(())
    }
}

impl Runtime {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn is_loaded(&self, model: &Model) -> bool {
        self.plans.contains_key(model.id)
    }

    /// Parse and optimise a model file (and its weights file, for a model that has one) for its
    /// fixed input size. The bytes are not kept.
    pub fn load(&mut self, model: &'static Model, bytes: &[u8], data: Option<&[u8]>) -> Result<(), MlError> {
        let load = |e: TractError| MlError::Load { name: model.name, reason: e.to_string() };
        let mut onnx = tract_onnx::onnx();
        // SCUNet's shape annotations keep a symbolic batch that clashes with the fixed input.
        if model.purpose == crate::Purpose::Denoise {
            onnx = onnx.with_ignore_value_info(true).with_ignore_output_shapes(true);
        }
        match (model.data, data) {
            (Some(_), Some(data)) => onnx.provider = std::sync::Arc::new(InMemory(data.to_vec())),
            (Some(_), None) => return Err(MlError::Load { name: model.name, reason: "its weights file is missing".into() }),
            (None, _) => {}
        }
        if model.purpose == crate::Purpose::Segment {
            return self.load_segment(model, bytes, data);
        }
        let proto = onnx.proto_model_for_read(&mut std::io::Cursor::new(bytes)).map_err(load)?;
        let plan = onnx
            .parse(&proto, Some(""))
            .map_err(load)?
            .model
            .with_input_fact(0, f32::fact([1, 3, model.side, model.side]).into())
            .and_then(|m| if model.planes() == 4 { m.with_input_fact(1, f32::fact([1, 1, model.side, model.side]).into()) } else { Ok(m) })
            .map_err(load)?
            .into_optimized()
            .map_err(load)?
            .into_runnable()
            .map_err(load)?;
        self.plans.insert(model.id, plan);
        Ok(())
    }

    /// A segmentation model: the image encoder (`side × side × 3`, `0..=255`; it normalises and pads
    /// itself) and the prompt decoder, which is the model's second file. The decoder's last output
    /// (masks resized to the original image) does not translate, so only the IoU and the
    /// low-resolution masks are kept; the app scales the mask itself.
    fn load_segment(&mut self, model: &'static Model, encoder: &[u8], decoder: Option<&[u8]>) -> Result<(), MlError> {
        let load = |e: TractError| MlError::Load { name: model.name, reason: e.to_string() };
        let decoder = decoder.ok_or(MlError::Load { name: model.name, reason: "its decoder file is missing".into() })?;
        let side = model.side;
        let enc = tract_onnx::onnx()
            .model_for_read(&mut std::io::Cursor::new(encoder))
            .map_err(load)?
            .with_input_fact(0, f32::fact([side, side, 3]).into())
            .map_err(load)?
            .into_optimized()
            .map_err(load)?
            .into_runnable()
            .map_err(load)?;
        let mut dec = tract_onnx::onnx().model_for_read(&mut std::io::Cursor::new(decoder)).map_err(load)?;
        let outs = dec.output_outlets().map_err(load)?.get(1..).map(<[_]>::to_vec).unwrap_or_default();
        dec.select_output_outlets(&outs).map_err(load)?;
        let low = side / 4;
        let dec = dec
            .with_input_fact(0, f32::fact([1, 256, side / 16, side / 16]).into())
            .and_then(|m| m.with_input_fact(1, f32::fact([1, 2, 2]).into()))
            .and_then(|m| m.with_input_fact(2, f32::fact([1, 2]).into()))
            .and_then(|m| m.with_input_fact(3, f32::fact([1, 1, low, low]).into()))
            .and_then(|m| m.with_input_fact(4, f32::fact([1]).into()))
            .and_then(|m| m.with_input_fact(5, f32::fact([2]).into()))
            .map_err(load)?
            .into_optimized()
            .map_err(load)?
            .into_runnable()
            .map_err(load)?;
        self.plans.insert(model.id, enc);
        self.decoders.insert(model.id, dec);
        Ok(())
    }

    /// One click: the image's embedding (from the cache when the image is the same), then the
    /// decoder with the point and a padding point. Returns the low-resolution mask logits.
    fn run_segment(&self, model: &'static Model, input: &[f32]) -> Result<Tensor, MlError> {
        let run = |e: TractError| MlError::Run { name: model.name, reason: e.to_string() };
        let (enc, dec) = match (self.plans.get(model.id), self.decoders.get(model.id)) {
            (Some(e), Some(d)) => (e, d),
            _ => return Err(MlError::NotInstalled(model.name)),
        };
        let side = model.side;
        let (image, point) = input.split_at(3 * side * side);
        let hash = {
            use std::hash::{Hash, Hasher};
            let mut h = std::collections::hash_map::DefaultHasher::new();
            image.iter().for_each(|v| v.to_bits().hash(&mut h));
            h.finish()
        };
        let cached = self.embedding.borrow().as_ref().filter(|(id, h, _)| *id == model.id && *h == hash).map(|(.., e)| e.clone());
        let embedding = match cached {
            Some(e) => e,
            None => {
                let out = enc.run(tvec!(tract_onnx::prelude::Tensor::from_shape(&[side, side, 3], image).map_err(run)?.into())).map_err(run)?;
                let e = out.into_iter().next().ok_or_else(|| MlError::Run { name: model.name, reason: "the encoder has no output".into() })?;
                *self.embedding.borrow_mut() = Some((model.id, hash, e.clone()));
                e
            }
        };
        let (x, y) = (point.first().copied().unwrap_or(0.0), point.get(1).copied().unwrap_or(0.0));
        let low = side / 4;
        let t = |shape: &[usize], data: &[f32]| tract_onnx::prelude::Tensor::from_shape(shape, data).map(TValue::from).map_err(run);
        let outputs = dec
            .run(tvec!(
                embedding,
                t(&[1, 2, 2], &[x, y, 0.0, 0.0])?,
                // The clicked point is foreground; the second is SAM's "no point" padding.
                t(&[1, 2], &[1.0, -1.0])?,
                t(&[1, 1, low, low], &vec![0.0; low * low])?,
                t(&[1], &[0.0])?,
                t(&[2], &[side as f32, side as f32])?,
            ))
            .map_err(run)?;
        let masks = outputs.get(1).ok_or_else(|| MlError::Run { name: model.name, reason: "the decoder has no mask output".into() })?;
        let view = masks.to_plain_array_view::<f32>().map_err(run)?;
        Ok(Tensor { shape: view.shape().to_vec(), data: view.iter().copied().collect() })
    }

    /// Free a loaded model's memory.
    pub fn unload(&mut self, model: &Model) {
        self.plans.remove(model.id);
        self.decoders.remove(model.id);
        self.embedding.borrow_mut().take();
    }

    /// Run a loaded model on one input and return its first output.
    pub fn run(&self, model: &'static Model, input: Tensor) -> Result<Tensor, MlError> {
        let plan = self.plans.get(model.id).ok_or(MlError::NotInstalled(model.name))?;
        let want: usize = model.input_shape().iter().product();
        let (side, planes) = (model.side, model.planes());
        if input.data.len() != want || input.shape != model.input_shape() {
            return Err(MlError::Shape { name: model.name, got: input.data.len(), want });
        }
        if model.purpose == crate::Purpose::Segment {
            return self.run_segment(model, &input.data);
        }
        let run = |e: TractError| MlError::Run { name: model.name, reason: e.to_string() };
        // The image is the first three planes; an inpainting model takes the fourth as its mask.
        let (image, mask) = input.data.split_at(3 * side * side);
        let mut inputs = tvec!(tract_onnx::prelude::Tensor::from_shape(&[1, 3, side, side], image).map_err(run)?.into());
        if planes == 4 {
            inputs.push(tract_onnx::prelude::Tensor::from_shape(&[1, 1, side, side], mask).map_err(run)?.into());
        }
        let outputs = plan.run(inputs).map_err(run)?;
        let first = outputs.first().ok_or_else(|| MlError::Run { name: model.name, reason: "the model has no output".into() })?;
        let view = first.to_plain_array_view::<f32>().map_err(run)?;
        Ok(Tensor { shape: view.shape().to_vec(), data: view.iter().copied().collect() })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::MODELS;

    #[test]
    fn a_wrong_file_fails_its_checksum_and_garbage_does_not_load() {
        let m = &MODELS[0];
        assert_eq!(verify(m, b"not the model"), Err(MlError::Checksum { name: m.name }));
        let mut rt = Runtime::new();
        assert!(matches!(rt.load(m, b"not an onnx file", None), Err(MlError::Load { .. })));
        assert!(matches!(rt.load(m, &[], None), Err(MlError::Load { .. })));
        // A model with a weights file can't load without it, and that file has its own checksum.
        let up = &MODELS[2];
        assert!(matches!(rt.load(up, b"graph", None), Err(MlError::Load { .. })));
        assert_eq!(verify_data(up, b"not the weights"), Err(MlError::Checksum { name: up.name }));
        assert!(matches!(verify_data(m, b""), Err(MlError::Load { .. })));
        assert!(!rt.is_loaded(m));
    }

    #[test]
    fn running_a_model_that_is_not_loaded_is_an_error() {
        let m = &MODELS[0];
        let input = Tensor { shape: vec![1, 3, m.side, m.side], data: vec![0.0; 3 * m.side * m.side] };
        assert_eq!(Runtime::new().run(m, input), Err(MlError::NotInstalled(m.name)));
    }

    /// With the real upscale model (never committed; `PHOTOCRAFT_MODEL_DIR/realesrgan.onnx` and
    /// `realesrgan.data` from their pinned URLs): both files pass their checksums, the weights load
    /// from memory, and a tile comes out four times its size, still showing its edge.
    #[test]
    fn realesrgan_enlarges_a_tile_when_the_model_is_available() {
        let m = &MODELS[2];
        let dir = std::env::var_os("PHOTOCRAFT_MODEL_DIR").map(std::path::PathBuf::from);
        let (Some(graph), Some(data)) = (
            dir.as_ref().and_then(|d| std::fs::read(d.join("realesrgan.onnx")).ok()),
            dir.as_ref().and_then(|d| std::fs::read(d.join("realesrgan.data")).ok()),
        ) else {
            eprintln!("skipped: set PHOTOCRAFT_MODEL_DIR to a folder with realesrgan.onnx and realesrgan.data");
            return;
        };
        verify(m, &graph).unwrap();
        verify_data(m, &data).unwrap();
        let mut rt = Runtime::new();
        assert!(matches!(rt.load(m, &graph, Some(&data[..1000])), Err(MlError::Load { .. })), "truncated weights are an error");
        rt.load(m, &graph, Some(&data)).unwrap();
        // Left half dark, right half light.
        let tile = crate::tensor::Tile { x: 0, y: 0, keep: [0, 0, m.side, m.side] };
        let input = crate::tensor::tile_input(|x, _| if x < 64 { [0.1; 3] } else { [0.9; 3] }, &tile, m.side);
        let out = rt.run(m, input).unwrap();
        assert_eq!(out.shape, [1, 3, 512, 512]);
        let (w, h, px) = crate::tensor::tile_output(&out.data, m, 4, 1, &tile).unwrap();
        assert_eq!((w, h), (512, 512));
        assert!(px[256 * 512 + 40][0] < 0.25 && px[256 * 512 + 470][0] > 0.75, "{:?} {:?}", px[256 * 512 + 40], px[256 * 512 + 470]);
    }

    /// With the real denoise model (never committed; `PHOTOCRAFT_MODEL_DIR/scunet.onnx` and
    /// `scunet.data` from their pinned URLs): it loads, keeps a tile's size and edge, and takes
    /// most of the noise out of a flat grey.
    #[test]
    fn scunet_denoises_a_tile_when_the_model_is_available() {
        let m = crate::find("scunet").unwrap();
        let dir = std::env::var_os("PHOTOCRAFT_MODEL_DIR").map(std::path::PathBuf::from);
        let (Some(graph), Some(data)) =
            (dir.as_ref().and_then(|d| std::fs::read(d.join("scunet.onnx")).ok()), dir.as_ref().and_then(|d| std::fs::read(d.join("scunet.data")).ok()))
        else {
            eprintln!("skipped: set PHOTOCRAFT_MODEL_DIR to a folder with scunet.onnx and scunet.data");
            return;
        };
        verify(m, &graph).unwrap();
        verify_data(m, &data).unwrap();
        let mut rt = Runtime::new();
        rt.load(m, &graph, Some(&data)).unwrap();
        // Left half dark, right half light, both with strong deterministic noise.
        let noise = |x: usize, y: usize| {
            // A hash per pixel: noise without a pattern the model could take for texture.
            let mut h = (x as u32).wrapping_mul(0x9E37_79B9) ^ (y as u32).wrapping_mul(0x85EB_CA6B);
            h ^= h >> 15;
            h = h.wrapping_mul(0x2C1B_3C6D);
            h ^= h >> 12;
            (h % 1000) as f32 / 1000.0 - 0.5
        };
        let tile = crate::tensor::Tile { x: 0, y: 0, keep: [0, 0, m.side, m.side] };
        let input = crate::tensor::tile_input(|x, y| [if x < 64 { 0.25 } else { 0.75 } + 0.2 * noise(x, y); 3], &tile, m.side);
        let out = rt.run(m, input).unwrap();
        assert_eq!(out.shape, [1, 3, m.side, m.side]);
        let (w, h, px) = crate::tensor::tile_output(&out.data, m, 1, 1, &tile).unwrap();
        assert_eq!((w, h), (m.side, m.side));
        let spread = |x0: usize| {
            let v: Vec<f32> = (20..100).flat_map(|y| (x0..x0 + 30).map(move |x| (x, y))).map(|(x, y)| px[y * w + x][1]).collect();
            let mean = v.iter().sum::<f32>() / v.len() as f32;
            (mean, (v.iter().map(|a| (a - mean).powi(2)).sum::<f32>() / v.len() as f32).sqrt())
        };
        let ((dark, dark_sd), (light, light_sd)) = (spread(16), spread(80));
        assert!((dark - 0.25).abs() < 0.08 && (light - 0.75).abs() < 0.08, "levels kept: {dark} {light}");
        // The input's noise has a standard deviation of about 0.058.
        assert!(dark_sd < 0.03 && light_sd < 0.03, "noise left: {dark_sd} {light_sd}");
    }

    /// With the real MobileSAM files (never committed; `PHOTOCRAFT_MODEL_DIR/mobilesam.onnx`, the
    /// encoder, and `mobilesam.data`, the decoder): a click on a bright disc selects the disc,
    /// and a second click on the same image reuses its embedding.
    #[test]
    fn mobilesam_selects_the_clicked_disc_when_the_model_is_available() {
        let m = crate::find("mobilesam").unwrap();
        let dir = std::env::var_os("PHOTOCRAFT_MODEL_DIR").map(std::path::PathBuf::from);
        let (Some(enc), Some(dec)) =
            (dir.as_ref().and_then(|d| std::fs::read(d.join("mobilesam.onnx")).ok()), dir.as_ref().and_then(|d| std::fs::read(d.join("mobilesam.data")).ok()))
        else {
            eprintln!("skipped: set PHOTOCRAFT_MODEL_DIR to a folder with mobilesam.onnx and mobilesam.data");
            return;
        };
        verify(m, &enc).unwrap();
        verify_data(m, &dec).unwrap();
        let mut rt = Runtime::new();
        rt.load(m, &enc, Some(&dec)).unwrap();
        let (w, h) = (300usize, 200usize);
        let rgb: Vec<[f32; 3]> = (0..w * h)
            .map(|i| {
                let (x, y) = ((i % w) as f32 - 90.0, (i / w) as f32 - 100.0);
                if x * x + y * y < 50.0 * 50.0 { [0.95, 0.8, 0.1] } else { [0.1, 0.2, 0.45] }
            })
            .collect();
        let click = |rt: &Runtime, at: [f32; 2]| {
            let out = rt.run(m, crate::tensor::segment_input(&rgb, w, h, at, m).unwrap()).unwrap();
            crate::tensor::segment_mask(&out.data, m, w, h).unwrap()
        };
        let t = std::time::Instant::now();
        let mask = click(&rt, [90.0, 100.0]);
        let first = t.elapsed();
        assert!(mask[100 * w + 90] > 200 && mask[100 * w + 120] > 200, "inside the disc");
        assert!(mask[100 * w + 250] < 50 && mask[10 * w + 10] < 50, "the background is out");
        let t = std::time::Instant::now();
        click(&rt, [100.0, 90.0]);
        assert!(t.elapsed() < first / 3, "the second click reuses the embedding: {:?} vs {first:?}", t.elapsed());
    }

    /// With the real model file (never committed; fetch it to `PHOTOCRAFT_MODEL_DIR/u2netp.onnx`):
    /// it passes its checksum, loads, and separates a bright disc from a dark field.
    #[test]
    fn u2netp_segments_a_disc_when_the_model_is_available() {
        let m = &MODELS[0];
        let Some(bytes) = std::env::var_os("PHOTOCRAFT_MODEL_DIR").and_then(|d| std::fs::read(std::path::Path::new(&d).join("u2netp.onnx")).ok()) else {
            eprintln!("skipped: set PHOTOCRAFT_MODEL_DIR to a folder with u2netp.onnx");
            return;
        };
        verify(m, &bytes).unwrap();
        let mut rt = Runtime::new();
        rt.load(m, &bytes, None).unwrap();
        let (w, h) = (96usize, 64usize);
        let rgb: Vec<[f32; 3]> = (0..w * h)
            .map(|i| {
                let (x, y) = ((i % w) as f32 - 48.0, (i / w) as f32 - 32.0);
                if x * x + y * y < 18.0 * 18.0 { [0.95, 0.85, 0.2] } else { [0.08, 0.1, 0.12] }
            })
            .collect();
        let out = rt.run(m, crate::tensor::subject_input(&rgb, w, h, m).unwrap()).unwrap();
        assert_eq!(out.data.len(), m.side * m.side);
        let mask = crate::tensor::subject_mask(&out.data, m, w, h).unwrap();
        assert!(mask[32 * w + 48] > 200, "centre of the disc: {}", mask[32 * w + 48]);
        assert!(mask[2 * w + 2] < 40 && mask[62 * w + 93] < 40, "corners: {} {}", mask[2 * w + 2], mask[62 * w + 93]);
        // A wrong-sized input is refused before it reaches the model.
        assert!(matches!(rt.run(m, Tensor { shape: vec![1, 3, 8, 8], data: vec![0.0; 192] }), Err(MlError::Shape { .. })));
    }
}
