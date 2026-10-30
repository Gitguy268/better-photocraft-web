//! The models PhotoCraft can download, pinned by URL and SHA-256.
//!
//! `docs/ai-models.md` is the human-readable source of truth (licence checks, dates); the test
//! below fails when this table and that file disagree.

/// What a model is used for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Purpose {
    /// Salient-object segmentation: Remove Background, Select Subject.
    Subject,
    /// 4x super-resolution of one square tile: AI Upscale.
    Upscale,
    /// Fills a hole in a square patch from its surroundings: the Remove tool.
    Inpaint,
    /// Removes noise from one square tile, same size out: AI Denoise.
    Denoise,
    /// Segments the object under a clicked point (an image encoder plus a prompt decoder):
    /// Object Selection's click.
    Segment,
}

/// A second file some models keep their weights in (ONNX "external data"), beside the graph.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct DataFile {
    pub bytes: u64,
    pub url: &'static str,
    pub sha256: &'static str,
}

/// How pixel values (`0..=1`) are normalised per channel before they reach the model:
/// `(value - mean) / std`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Norm {
    pub mean: [f32; 3],
    pub std: [f32; 3],
}

/// One downloadable model.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Model {
    /// Stable id used in command params, preferences and the model cache.
    pub id: &'static str,
    pub name: &'static str,
    pub purpose: Purpose,
    /// One line for the download prompt: what the user gets.
    pub blurb: &'static str,
    /// SPDX id of the licence, shown before the download.
    pub license: &'static str,
    pub bytes: u64,
    /// Pinned to a commit, so the file can't change under its hash.
    pub url: &'static str,
    /// Lower-case hex.
    pub sha256: &'static str,
    /// The model's square input side, in pixels.
    pub side: usize,
    pub norm: Norm,
    /// The weights, when they are not inside the model file.
    pub data: Option<DataFile>,
}

impl Model {
    /// The hash as a Subresource Integrity value (`sha256-<base64>`): given to `fetch`, the
    /// browser itself rejects a download whose content differs.
    pub fn integrity(&self) -> String {
        const B64: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
        let nibble = |c: u8| match c {
            b'0'..=b'9' => c - b'0',
            b'a'..=b'f' => c - b'a' + 10,
            _ => 0,
        };
        let raw: Vec<u8> = self.sha256.as_bytes().chunks(2).map(|p| nibble(p[0]) << 4 | p.get(1).map_or(0, |c| nibble(*c))).collect();
        let mut out = String::from("sha256-");
        for chunk in raw.chunks(3) {
            let n = u32::from(chunk[0]) << 16 | u32::from(chunk.get(1).copied().unwrap_or(0)) << 8 | u32::from(chunk.get(2).copied().unwrap_or(0));
            for (i, shift) in [18, 12, 6, 0].into_iter().enumerate() {
                out.push(if i <= chunk.len() { char::from(B64[(n >> shift) as usize & 63]) } else { '=' });
            }
        }
        out
    }

    /// Planes of the model's input tensor `[1, planes, side, side]`: RGB, plus the hole mask for an
    /// inpainting model (which the runtime hands to the model as its second input).
    pub fn planes(&self) -> usize {
        if self.purpose == Purpose::Inpaint { 4 } else { 3 }
    }

    /// The shape of the one tensor the app hands the runtime. A segmentation model takes the
    /// image (`side × side × 3`, `0..=255`) followed by the clicked point `(x, y)` in it.
    pub fn input_shape(&self) -> Vec<usize> {
        if self.purpose == Purpose::Segment { vec![1, 3 * self.side * self.side + 2] } else { vec![1, self.planes(), self.side, self.side] }
    }

    /// The files to download, as (URL, SHA-256, bytes): the model file, then its weights file if
    /// it has one.
    pub fn files(&self) -> impl Iterator<Item = (&'static str, &'static str, u64)> {
        std::iter::once((self.url, self.sha256, self.bytes)).chain(self.data.map(|d| (d.url, d.sha256, d.bytes)))
    }

    /// Everything there is to download.
    pub fn total_bytes(&self) -> u64 {
        self.bytes + self.data.map_or(0, |d| d.bytes)
    }

    /// Size for the download prompt, e.g. "4.4 MB".
    pub fn size_label(&self) -> String {
        let mb = self.total_bytes() as f64 / (1024.0 * 1024.0);
        if mb < 10.0 { format!("{mb:.1} MB") } else { format!("{mb:.0} MB") }
    }
}

const IMAGENET: Norm = Norm { mean: [0.485, 0.456, 0.406], std: [0.229, 0.224, 0.225] };

/// Every model the app may download, the default for each purpose first.
pub const MODELS: &[Model] = &[
    Model {
        id: "u2netp",
        name: "U²-Netp",
        purpose: Purpose::Subject,
        blurb: "Fast subject detection for Remove Background and Select Subject.",
        license: "Apache-2.0",
        bytes: 4_574_861,
        url: "https://huggingface.co/BritishWerewolf/U-2-Netp/resolve/7112208dbac3a3642496c8d54e2f0f9bb3dc1dc8/onnx/model.onnx",
        sha256: "309c8469258dda742793dce0ebea8e6dd393174f89934733ecc8b14c76f4ddd8",
        side: 320,
        norm: IMAGENET,
        data: None,
    },
    Model {
        id: "isnet",
        name: "IS-Net",
        purpose: Purpose::Subject,
        blurb: "High-quality subject detection with finer edges. A large download that needs about 2 GB of memory.",
        license: "Apache-2.0",
        bytes: 178_648_008,
        url: "https://huggingface.co/BritishWerewolf/IS-Net/resolve/9783722d9f964c0286a411e7e8e6fede947d5a53/onnx/model.onnx",
        sha256: "60920e99c45464f2ba57bee2ad08c919a52bbf852739e96947fbb4358c0d964a",
        side: 1024,
        norm: Norm { mean: [0.5; 3], std: [1.0; 3] },
        data: None,
    },
    Model {
        id: "realesrgan",
        name: "Real-ESRGAN",
        purpose: Purpose::Upscale,
        blurb: "Enlarges images 2x or 4x with sharper detail than resampling.",
        license: "BSD-3-Clause",
        bytes: 42_443,
        url: "https://huggingface.co/TheGuy444/Real-ESRGAN-ONNX/resolve/4c80f3da1a8a85f0757a9e7f79e08d0307220eb5/onnx/model.onnx",
        sha256: "2d1810f8b75c6a899487decf933697241eafbe6327d7b723674ba6df73a08fda",
        side: 128,
        norm: Norm { mean: [0.0; 3], std: [1.0; 3] },
        data: Some(DataFile {
            bytes: 4_836_096,
            url: "https://huggingface.co/TheGuy444/Real-ESRGAN-ONNX/resolve/4c80f3da1a8a85f0757a9e7f79e08d0307220eb5/onnx/model.data",
            sha256: "512d0ec9940c2e9d85d27f2952f12a0b77b7841dc22df4ce9f3ea458bc98f37f",
        }),
    },
    Model {
        id: "lama",
        name: "LaMa",
        purpose: Purpose::Inpaint,
        blurb: "Fills what the Remove tool paints over with matching surroundings. A large download that needs about 2 GB of memory.",
        license: "Apache-2.0",
        bytes: 208_044_816,
        url: "https://huggingface.co/Carve/LaMa-ONNX/resolve/c3c0c9e468934d62e79c329e35d82dd09ff8c444/lama_fp32.onnx",
        sha256: "1faef5301d78db7dda502fe59966957ec4b79dd64e16f03ed96913c7a4eb68d6",
        side: 512,
        norm: Norm { mean: [0.0; 3], std: [1.0; 3] },
        data: None,
    },
    Model {
        id: "scunet",
        name: "SCUNet",
        purpose: Purpose::Denoise,
        blurb: "Removes noise and grain from photos while keeping detail.",
        license: "Apache-2.0",
        bytes: 3_798_678,
        url: "https://huggingface.co/Heliosoph/scunet-onnx/resolve/6d11417ee2fbcc73783c502a238ac115097754fe/scunet_color_real_psnr.onnx",
        sha256: "231be201ab413dbc999d7951caa9844846b93a12a40a41e037d6b5888ed4e88c",
        // A multiple of 64, as its windowed attention needs.
        side: 128,
        norm: Norm { mean: [0.0; 3], std: [1.0; 3] },
        data: Some(DataFile {
            bytes: 73_138_176,
            url: "https://huggingface.co/Heliosoph/scunet-onnx/resolve/6d11417ee2fbcc73783c502a238ac115097754fe/scunet_color_real_psnr.onnx.data",
            sha256: "98825ea1210b641c71e5f052f582c70c49fd44b35387ebe2c034268c17df3feb",
        }),
    },
    Model {
        id: "mobilesam",
        name: "MobileSAM",
        purpose: Purpose::Segment,
        blurb: "Selects the object you click with the Object Selection tool.",
        license: "Apache-2.0",
        bytes: 28_157_093,
        // The image encoder; the prompt decoder is the second file.
        url: "https://huggingface.co/Acly/MobileSAM/resolve/0d3b403339b4674a82493d5e97964dd78089ddc8/mobile_sam_image_encoder.onnx",
        sha256: "580f5fb648ea1062c0aabc26217aed56921985f03f0cbbd852bba81d760cc749",
        side: 1024,
        norm: Norm { mean: [0.0; 3], std: [1.0; 3] },
        // Not external weights like the others' second file: a graph of its own (see the runtime).
        data: Some(DataFile {
            bytes: 16_501_323,
            url: "https://huggingface.co/Acly/MobileSAM/resolve/0d3b403339b4674a82493d5e97964dd78089ddc8/sam_mask_decoder_single.onnx",
            sha256: "93915fc7c993ab9d59ab8c9ccd3bce37f7509c81ab4150a74abd4d2abbd8570d",
        }),
    },
];

/// The model with this id.
pub fn find(id: &str) -> Result<&'static Model, crate::MlError> {
    MODELS.iter().find(|m| m.id == id).ok_or_else(|| crate::MlError::UnknownModel(id.chars().take(40).collect()))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `docs/ai-models.md` lists every shipped model with the same URL, hash, size and licence.
    #[test]
    fn the_registry_matches_the_documented_models() {
        let doc = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/../../docs/ai-models.md")).unwrap();
        let shipped = doc.split("## Candidates checked").next().unwrap();
        for m in MODELS {
            let row = shipped
                .lines()
                .find(|l| l.starts_with(&format!("| `{}` |", m.id)) && l.contains(m.sha256))
                .unwrap_or_else(|| panic!("{}: no table row with its hash", m.id));
            let bytes = m.bytes.to_string().as_bytes().rchunks(3).rev().map(|c| std::str::from_utf8(c).unwrap()).collect::<Vec<_>>().join(",");
            assert!(row.contains(&format!("{bytes} B")), "{}: size {bytes} B not in {row}", m.id);
            assert!(row.contains(m.license), "{}: licence", m.id);
            assert!(shipped.contains(m.url), "{}: url", m.id);
            if let Some(d) = m.data {
                assert!(shipped.contains(d.url) && shipped.contains(d.sha256), "{}: its weights file is documented", m.id);
                assert!(d.url.starts_with("https://") && d.sha256.len() == 64);
            }
            assert!(m.url.starts_with("https://") && m.sha256.len() == 64 && m.sha256.bytes().all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase()));
        }
        let documented = shipped.lines().filter(|l| l.starts_with("| `") && l.contains(" B (")).count();
        assert_eq!(documented, MODELS.len(), "a documented model is missing from the registry");
    }

    #[test]
    fn ids_are_unique_and_unknown_ids_are_errors() {
        for m in MODELS {
            assert_eq!(MODELS.iter().filter(|o| o.id == m.id).count(), 1);
            assert_eq!(find(m.id), Ok(m));
        }
        assert_eq!(find("nope"), Err(crate::MlError::UnknownModel("nope".into())));
        assert!(matches!(find(&"x".repeat(10_000)), Err(crate::MlError::UnknownModel(s)) if s.len() == 40));
    }

    #[test]
    fn integrity_is_the_base64_of_the_hash() {
        // sha256("") = e3b0c442…b855, whose SRI form is well known.
        let m = Model { sha256: "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855", ..MODELS[0] };
        assert_eq!(m.integrity(), "sha256-47DEQpj8HBSa+/TImW+5JCeuQeRkm5NMpJWZG3hSuFU=");
        assert_eq!(MODELS[0].size_label(), "4.4 MB");
        assert_eq!(MODELS[1].size_label(), "170 MB");
        assert_eq!(MODELS[2].size_label(), "4.7 MB", "the graph and its weights file together");
        assert_eq!((MODELS[0].planes(), MODELS[3].planes()), (3, 4), "an inpainting model's input carries its mask");
    }
}
