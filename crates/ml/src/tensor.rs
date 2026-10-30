//! Images to model tensors and model outputs back to masks. No model runtime needed, so the
//! browser's main wasm does this part and ships only small tensors to its worker.

use crate::{MlError, Model};

/// A dense `f32` tensor: what goes into and comes out of a model.
#[derive(Clone, Debug, PartialEq)]
pub struct Tensor {
    pub shape: Vec<usize>,
    pub data: Vec<f32>,
}

/// Bilinear sample of a row-major single-channel image at continuous `(x, y)` (pixel centres at
/// integers), clamped at the borders.
#[inline]
fn bilinear(get: impl Fn(usize, usize) -> f32, w: usize, h: usize, x: f32, y: f32) -> f32 {
    let (x, y) = (x.clamp(0.0, (w - 1) as f32), y.clamp(0.0, (h - 1) as f32));
    let (x0, y0) = (x as usize, y as usize);
    let (x1, y1) = ((x0 + 1).min(w - 1), (y0 + 1).min(h - 1));
    let (fx, fy) = (x - x0 as f32, y - y0 as f32);
    let top = get(x0, y0) * (1.0 - fx) + get(x1, y0) * fx;
    let bottom = get(x0, y1) * (1.0 - fx) + get(x1, y1) * fx;
    top * (1.0 - fy) + bottom * fy
}

/// The model's input for an RGB image (`0..=1`, row-major, `w × h`): stretched to the model's
/// square side, normalised per channel, laid out `[1, 3, side, side]`.
///
/// The models were trained on images squeezed to a square, so the aspect ratio is not kept.
pub fn subject_input(rgb: &[[f32; 3]], w: usize, h: usize, model: &Model) -> Result<Tensor, MlError> {
    let side = model.side;
    if w == 0 || h == 0 || rgb.len() != w.saturating_mul(h) {
        return Err(MlError::Shape { name: model.name, got: rgb.len(), want: w.saturating_mul(h) });
    }
    let mut data = vec![0.0f32; 3 * side * side];
    let (sx, sy) = (w as f32 / side as f32, h as f32 / side as f32);
    for (c, plane) in data.chunks_exact_mut(side * side).enumerate() {
        let (mean, std) = (model.norm.mean[c], model.norm.std[c]);
        for (i, out) in plane.iter_mut().enumerate() {
            let (x, y) = ((i % side) as f32, (i / side) as f32);
            let v = bilinear(|px, py| rgb[py * w + px][c], w, h, (x + 0.5) * sx - 0.5, (y + 0.5) * sy - 0.5);
            let v = if v.is_finite() { v.clamp(0.0, 1.0) } else { 0.0 };
            *out = (v - mean) / std;
        }
    }
    Ok(Tensor { shape: vec![1, 3, side, side], data })
}

/// A model's saliency output (`side × side`, any range) as 8-bit coverage over a `w × h` image:
/// stretched to `0..=1` by its own minimum and maximum (as the models' reference code does), then
/// scaled back to the image.
pub fn subject_mask(output: &[f32], model: &Model, w: usize, h: usize) -> Result<Vec<u8>, MlError> {
    let side = model.side;
    if output.len() != side * side || w == 0 || h == 0 {
        return Err(MlError::Shape { name: model.name, got: output.len(), want: side * side });
    }
    let (lo, hi) = output.iter().filter(|v| v.is_finite()).fold((f32::MAX, f32::MIN), |(lo, hi), v| (lo.min(*v), hi.max(*v)));
    if hi <= lo {
        // A flat (or all-NaN) output carries no subject.
        return Ok(vec![0; w * h]);
    }
    let scale = 1.0 / (hi - lo);
    let (sx, sy) = (side as f32 / w as f32, side as f32 / h as f32);
    let at = |x: usize, y: usize| {
        let v = output[y * side + x];
        if v.is_finite() { (v - lo) * scale } else { 0.0 }
    };
    let mut mask = vec![0u8; w * h];
    for (y, row) in mask.chunks_exact_mut(w).enumerate() {
        let fy = (y as f32 + 0.5) * sy - 0.5;
        for (x, out) in row.iter_mut().enumerate() {
            *out = (bilinear(at, side, side, (x as f32 + 0.5) * sx - 0.5, fy) * 255.0 + 0.5) as u8;
        }
    }
    Ok(mask)
}

/// What SAM pads its square input with: the pixel mean it normalises by, so padding comes out 0.
const SAM_MEAN: [f32; 3] = [123.675, 116.28, 103.53];
/// The side of a SAM decoder's low-resolution mask.
const SAM_LOW: usize = 256;

/// A segmentation model's input for an RGB image (`0..=1`, `w × h`) and a point in it: the image
/// scaled so its longer side is the model's square, `0..=255`, row-major `H × W × 3`, padded on
/// the right and bottom with the pixel mean; then the point in that square.
pub fn segment_input(rgb: &[[f32; 3]], w: usize, h: usize, point: [f32; 2], model: &Model) -> Result<Tensor, MlError> {
    let side = model.side;
    if w == 0 || h == 0 || rgb.len() != w.saturating_mul(h) || !point.iter().all(|v| v.is_finite()) {
        return Err(MlError::Shape { name: model.name, got: rgb.len(), want: w.saturating_mul(h) });
    }
    let k = side as f32 / w.max(h) as f32;
    let (sw, sh) = (((w as f32 * k).round() as usize).clamp(1, side), ((h as f32 * k).round() as usize).clamp(1, side));
    let mut data = Vec::with_capacity(3 * side * side + 2);
    for y in 0..side {
        for x in 0..side {
            if x < sw && y < sh {
                let (fx, fy) = ((x as f32 + 0.5) / k - 0.5, (y as f32 + 0.5) / k - 0.5);
                data.extend((0..3).map(|c| bilinear(|i, j| rgb[j * w + i][c], w, h, fx, fy).clamp(0.0, 1.0) * 255.0));
            } else {
                data.extend(SAM_MEAN);
            }
        }
    }
    data.extend([(point[0] * k).clamp(0.0, side as f32), (point[1] * k).clamp(0.0, side as f32)]);
    Ok(Tensor { shape: model.input_shape(), data })
}

/// A segmentation model's low-resolution logits (`256 × 256`, over the padded square) as a mask
/// for the `w × h` image: the image's part cut out, enlarged, positive logits in.
pub fn segment_mask(output: &[f32], model: &Model, w: usize, h: usize) -> Result<Vec<u8>, MlError> {
    if output.len() != SAM_LOW * SAM_LOW || w == 0 || h == 0 {
        return Err(MlError::Shape { name: model.name, got: output.len(), want: SAM_LOW * SAM_LOW });
    }
    // The image covers this much of the low-resolution square.
    let k = SAM_LOW as f32 / w.max(h) as f32;
    let at = |x: usize, y: usize| {
        let v = output[y * SAM_LOW + x];
        if v.is_finite() { v } else { -1.0 }
    };
    let mut mask = vec![0u8; w * h];
    for (y, row) in mask.chunks_exact_mut(w).enumerate() {
        let fy = (y as f32 + 0.5) * k - 0.5;
        for (x, out) in row.iter_mut().enumerate() {
            // A soft edge a pixel or two wide, from the logit's slope.
            let v = bilinear(at, SAM_LOW, SAM_LOW, (x as f32 + 0.5) * k - 0.5, fy);
            *out = ((v * 2.0 + 0.5).clamp(0.0, 1.0) * 255.0 + 0.5) as u8;
        }
    }
    Ok(mask)
}

/// An inpainting model's input for an RGB patch (sRGB, `0..=1`, `w × h`) and its hole: the patch
/// stretched to the model's square as three planes, and a fourth plane that is 1 where the model
/// is to paint. Any model pixel that touches the hole counts as hole, so a thin stroke survives a
/// patch larger than the model's square.
pub fn inpaint_input(rgb: &[[f32; 3]], hole: &[bool], w: usize, h: usize, model: &Model) -> Result<Tensor, MlError> {
    let side = model.side;
    if w == 0 || h == 0 || rgb.len() != w.saturating_mul(h) || hole.len() != rgb.len() {
        return Err(MlError::Shape { name: model.name, got: rgb.len(), want: w.saturating_mul(h) });
    }
    let mut data = vec![0.0f32; 4 * side * side];
    let (sx, sy) = (w as f32 / side as f32, h as f32 / side as f32);
    let (image, mask) = data.split_at_mut(3 * side * side);
    for (c, plane) in image.chunks_exact_mut(side * side).enumerate() {
        for (i, out) in plane.iter_mut().enumerate() {
            let v = bilinear(|px, py| rgb[py * w + px][c], w, h, ((i % side) as f32 + 0.5) * sx - 0.5, ((i / side) as f32 + 0.5) * sy - 0.5);
            *out = if v.is_finite() { v.clamp(0.0, 1.0) } else { 0.0 };
        }
    }
    for (i, out) in mask.iter_mut().enumerate() {
        // The source pixels this model pixel covers.
        let (x0, y0) = (((i % side) as f32 * sx) as usize, ((i / side) as f32 * sy) as usize);
        let (x1, y1) = (((((i % side) + 1) as f32 * sx).ceil() as usize).clamp(x0 + 1, w), ((((i / side) + 1) as f32 * sy).ceil() as usize).clamp(y0 + 1, h));
        let any = (y0.min(h - 1)..y1).any(|y| (x0.min(w - 1)..x1).any(|x| hole[y * w + x]));
        *out = f32::from(u8::from(any));
    }
    Ok(Tensor { shape: vec![1, 4, side, side], data })
}

/// An inpainting model's output (`[1, 3, side, side]`, values `0..=255`) scaled back to the
/// `w × h` patch, as sRGB pixels in `0..=1`.
pub fn inpaint_output(output: &[f32], model: &Model, w: usize, h: usize) -> Result<Vec<[f32; 3]>, MlError> {
    let side = model.side;
    if output.len() != 3 * side * side || w == 0 || h == 0 {
        return Err(MlError::Shape { name: model.name, got: output.len(), want: 3 * side * side });
    }
    let (sx, sy) = (side as f32 / w as f32, side as f32 / h as f32);
    let mut px = Vec::with_capacity(w * h);
    for y in 0..h {
        for x in 0..w {
            px.push([0, 1, 2].map(|c| {
                let v =
                    bilinear(|px, py| output[c * side * side + py * side + px], side, side, (x as f32 + 0.5) * sx - 0.5, (y as f32 + 0.5) * sy - 0.5) / 255.0;
                if v.is_finite() { v.clamp(0.0, 1.0) } else { 0.0 }
            }));
        }
    }
    Ok(px)
}

/// One square tile of an image, for a model that enlarges fixed-size squares.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Tile {
    /// The tile's top-left corner in the image. A tile may reach past the image's far edges when
    /// the image is smaller than a tile; those pixels repeat the edge.
    pub x: usize,
    pub y: usize,
    /// The part of the tile that is kept, in tile pixels: `[x0, y0, x1, y1]`. The rest is overlap
    /// with the neighbours, which gives the model context and hides the seams.
    pub keep: [usize; 4],
}

/// Along one axis: (origin, kept from, kept to) for tiles of `side` over `len` pixels.
fn axis(len: usize, side: usize, margin: usize) -> Vec<(usize, usize, usize)> {
    if len <= side {
        return vec![(0, 0, len)];
    }
    let stride = side.saturating_sub(2 * margin).max(1);
    let mut origins: Vec<usize> = (0..).map(|i| i * stride).take_while(|o| o + side < len).collect();
    origins.push(len - side);
    // Neighbours meet in the middle of their overlap.
    let cut = |i: usize| (origins[i] + side + origins[i + 1]) / 2;
    (0..origins.len())
        .map(|i| (origins[i], if i == 0 { 0 } else { cut(i - 1) - origins[i] }, if i + 1 == origins.len() { side } else { cut(i) - origins[i] }))
        .collect()
}

/// Tiles of `side` that cover a `w × h` image so that every pixel is kept from exactly one tile,
/// at least `margin` pixels inside it wherever the tile has a neighbour. Memory stays bounded by
/// one tile whatever the image's size.
pub fn tiles(w: usize, h: usize, side: usize, margin: usize) -> Vec<Tile> {
    if w == 0 || h == 0 || side == 0 {
        return Vec::new();
    }
    let (xs, ys) = (axis(w, side, margin), axis(h, side, margin));
    ys.iter().flat_map(|&(y, y0, y1)| xs.iter().map(move |&(x, x0, x1)| Tile { x, y, keep: [x0, y0, x1, y1] })).collect()
}

/// The model's input for one tile: `rgb(x, y)` gives the image's sRGB pixel (`0..=1`) at image
/// coordinates, which the caller clamps to the image for tiles that reach past it.
pub fn tile_input(rgb: impl Fn(usize, usize) -> [f32; 3], tile: &Tile, side: usize) -> Tensor {
    let mut data = vec![0.0f32; 3 * side * side];
    for y in 0..side {
        for x in 0..side {
            let p = rgb(tile.x + x, tile.y + y);
            for (c, v) in p.into_iter().enumerate() {
                data[c * side * side + y * side + x] = if v.is_finite() { v.clamp(0.0, 1.0) } else { 0.0 };
            }
        }
    }
    Tensor { shape: vec![1, 3, side, side], data }
}

/// The kept part of a tile's enlarged output (`[1, 3, side·up, side·up]`), as rows of sRGB pixels:
/// `(width, height, pixels)`. `shrink` 2 halves it by averaging 2 × 2 (a 4x model used for 2x).
pub fn tile_output(output: &[f32], model: &Model, up: usize, shrink: usize, tile: &Tile) -> Result<(usize, usize, Vec<[f32; 3]>), MlError> {
    let big = model.side * up;
    if up == 0 || shrink == 0 || !up.is_multiple_of(shrink) || output.len() != 3 * big * big {
        return Err(MlError::Shape { name: model.name, got: output.len(), want: 3 * big * big });
    }
    let [x0, y0, x1, y1] = tile.keep;
    if x1 > model.side || y1 > model.side || x0 > x1 || y0 > y1 {
        return Err(MlError::Shape { name: model.name, got: x1.max(y1), want: model.side });
    }
    let k = up / shrink;
    let (w, h) = ((x1 - x0) * k, (y1 - y0) * k);
    let at = |c: usize, x: usize, y: usize| {
        let v = output[c * big * big + y * big + x];
        if v.is_finite() { v.clamp(0.0, 1.0) } else { 0.0 }
    };
    let mut px = Vec::with_capacity(w * h);
    for y in 0..h {
        for x in 0..w {
            let (sx, sy) = (x0 * up + x * shrink, y0 * up + y * shrink);
            px.push([0, 1, 2].map(|c| {
                let mut sum = 0.0;
                for dy in 0..shrink {
                    for dx in 0..shrink {
                        sum += at(c, sx + dx, sy + dy);
                    }
                }
                sum / (shrink * shrink) as f32
            }));
        }
    }
    Ok((w, h, px))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::MODELS;

    #[test]
    fn input_is_normalised_and_planar() {
        let m = &MODELS[0];
        // A 2 × 1 image: red on the left, blue on the right.
        let t = subject_input(&[[1.0, 0.0, 0.0], [0.0, 0.0, 1.0]], 2, 1, m).unwrap();
        assert_eq!(t.shape, [1, 3, m.side, m.side]);
        let px = |c: usize, x: usize| t.data[c * m.side * m.side + x];
        let norm = |v: f32, c: usize| (v - m.norm.mean[c]) / m.norm.std[c];
        assert!((px(0, 0) - norm(1.0, 0)).abs() < 1e-5 && (px(2, 0) - norm(0.0, 2)).abs() < 1e-5);
        assert!((px(0, m.side - 1) - norm(0.0, 0)).abs() < 1e-5 && (px(2, m.side - 1) - norm(1.0, 2)).abs() < 1e-5);
        // Out-of-range and non-finite samples are clamped, not passed on.
        let t = subject_input(&[[f32::NAN, 7.0, -3.0]], 1, 1, m).unwrap();
        assert!(t.data.iter().all(|v| v.is_finite()));
        assert_eq!(t.data[m.side * m.side], norm(1.0, 1));
    }

    #[test]
    fn mask_is_stretched_and_scaled_to_the_image() {
        let m = &MODELS[0];
        let side = m.side;
        // Left half 2.0, right half 6.0: becomes 0 and 255 whatever the output's range.
        let out: Vec<f32> = (0..side * side).map(|i| if i % side < side / 2 { 2.0 } else { 6.0 }).collect();
        let mask = subject_mask(&out, m, 8, 4).unwrap();
        assert_eq!(mask.len(), 32);
        assert!(mask.chunks(8).all(|r| r[0] == 0 && r[2] == 0 && r[5] == 255 && r[7] == 255), "{mask:?}");
        assert_eq!(subject_mask(&vec![0.3; side * side], m, 3, 3).unwrap(), vec![0; 9], "a flat output selects nothing");
        assert_eq!(subject_mask(&vec![f32::NAN; side * side], m, 3, 3).unwrap(), vec![0; 9]);
    }

    #[test]
    fn inpaint_patches_carry_their_hole_and_come_back_at_their_own_size() {
        let m = &MODELS[3];
        let side = m.side;
        // A 1024 x 600 patch (larger than the model's square) with a 1 px wide vertical stroke.
        let (w, h) = (1024, 600);
        let rgb = vec![[0.25, 0.5, 0.75]; w * h];
        let hole: Vec<bool> = (0..w * h).map(|i| i % w == 400).collect();
        let t = inpaint_input(&rgb, &hole, w, h, m).unwrap();
        assert_eq!(t.shape, [1, 4, side, side]);
        assert!((t.data[0] - 0.25).abs() < 1e-6 && (t.data[2 * side * side + 5] - 0.75).abs() < 1e-6);
        let mask = &t.data[3 * side * side..];
        assert!(mask.iter().all(|v| *v == 0.0 || *v == 1.0));
        assert!(mask[200] == 1.0 && mask[side * 300 + 200] == 1.0 && mask[10] == 0.0, "the thin stroke is not lost in scaling down");
        // The model answers in 0..=255; the patch comes back in 0..=1 at its own size.
        let px = inpaint_output(&vec![127.5; 3 * side * side], m, w, h).unwrap();
        assert_eq!(px.len(), w * h);
        assert!((px[12345][1] - 0.5).abs() < 1e-5);
        assert!(inpaint_input(&rgb, &hole[1..], w, h, m).is_err() && inpaint_input(&[], &[], 0, 0, m).is_err());
        assert!(inpaint_output(&[0.0; 9], m, 4, 4).is_err() && inpaint_output(&vec![0.0; 3 * side * side], m, 0, 4).is_err());
        assert!(inpaint_output(&vec![f32::NAN; 3 * side * side], m, 2, 2).unwrap().iter().all(|p| p.iter().all(|v| *v == 0.0)));
    }

    #[test]
    fn tiles_cover_every_pixel_once_with_overlap_between_neighbours() {
        for (w, h) in [(1, 1), (100, 60), (128, 128), (129, 128), (300, 200), (1000, 37), (513, 777)] {
            let (side, margin) = (128, 8);
            let ts = tiles(w, h, side, margin);
            let mut seen = vec![0u8; w * h];
            for t in &ts {
                let [x0, y0, x1, y1] = t.keep;
                assert!(x0 <= x1 && y0 <= y1 && x1 <= side && y1 <= side, "{w}x{h}: {t:?}");
                // Wherever the tile has a neighbour, what it keeps stays `margin` inside it.
                assert!(x0 == 0 || x0 >= margin, "{w}x{h}: {t:?}");
                assert!(t.x + x1 == w.max(x1) || side - x1 >= margin || t.x + side >= w, "{w}x{h}: {t:?}");
                for y in y0..y1 {
                    for x in x0..x1 {
                        if t.x + x < w && t.y + y < h {
                            seen[(t.y + y) * w + t.x + x] += 1;
                        }
                    }
                }
            }
            assert!(seen.iter().all(|n| *n == 1), "{w}x{h}: every pixel kept exactly once");
            assert_eq!(ts.len() == 1, w <= side && h <= side);
        }
        assert!(tiles(0, 10, 128, 8).is_empty() && tiles(10, 10, 0, 0).is_empty());
    }

    #[test]
    fn tile_pixels_go_in_planar_and_come_out_enlarged_or_halved() {
        let m = &MODELS[2];
        let (side, up) = (m.side, 4);
        let tile = Tile { x: 3, y: 5, keep: [8, 8, 10, 9] };
        let t = tile_input(|x, y| [x as f32 / 1000.0, y as f32 / 1000.0, f32::NAN], &tile, side);
        assert_eq!(t.shape, [1, 3, side, side]);
        assert_eq!((t.data[0], t.data[side * side], t.data[2 * side * side]), (0.003, 0.005, 0.0));
        // An output whose value is its own x position: the kept columns are 8·4 .. 10·4.
        let big = side * up;
        let out: Vec<f32> = (0..3 * big * big).map(|i| (i % big) as f32 / big as f32).collect();
        let (w, h, px) = tile_output(&out, m, up, 1, &tile).unwrap();
        assert_eq!((w, h, px.len()), (8, 4, 32));
        assert_eq!(px[0][0], 32.0 / big as f32);
        let (w2, h2, half) = tile_output(&out, m, up, 2, &tile).unwrap();
        assert_eq!((w2, h2), (4, 2));
        assert!((half[0][0] - 32.5 / big as f32).abs() < 1e-6, "2 x 2 average of columns 32 and 33");
        assert!(tile_output(&out[1..], m, up, 1, &tile).is_err() && tile_output(&out, m, up, 3, &tile).is_err() && tile_output(&out, m, 0, 1, &tile).is_err());
        assert!(tile_output(&out, m, up, 1, &Tile { keep: [0, 0, side + 1, 4], ..tile }).is_err());
    }

    #[test]
    fn segment_input_keeps_the_aspect_pads_with_the_mean_and_scales_the_point() {
        let m = crate::find("mobilesam").unwrap();
        // 200 × 100: the long side becomes 1024, the image fills the top half.
        let rgb = vec![[1.0, 0.5, 0.0]; 200 * 100];
        let t = segment_input(&rgb, 200, 100, [100.0, 50.0], m).unwrap();
        assert_eq!(t.shape, [1, 3 * 1024 * 1024 + 2]);
        let px = |x: usize, y: usize| &t.data[(y * 1024 + x) * 3..(y * 1024 + x) * 3 + 3];
        assert_eq!(px(10, 10), [255.0, 127.5, 0.0], "inside the image, 0..=255");
        assert_eq!(px(10, 600), SAM_MEAN, "below it, the mean");
        assert_eq!(&t.data[3 * 1024 * 1024..], [512.0, 256.0], "the point in the 1024 square");
        assert!(segment_input(&rgb, 200, 99, [0.0, 0.0], m).is_err());
        assert!(segment_input(&rgb, 200, 100, [f32::NAN, 0.0], m).is_err());
    }

    #[test]
    fn segment_mask_cuts_the_image_out_of_the_square() {
        let m = crate::find("mobilesam").unwrap();
        // Positive logits on the left half of the low-resolution square, negative elsewhere.
        let out: Vec<f32> = (0..256 * 256).map(|i| if i % 256 < 128 { 8.0 } else { -8.0 }).collect();
        // A 400 × 100 image covers the top quarter of the square: its left half is in.
        let mask = segment_mask(&out, m, 400, 100).unwrap();
        assert_eq!(mask.len(), 400 * 100);
        assert_eq!((mask[50 * 400 + 50], mask[50 * 400 + 350]), (255, 0));
        assert!(segment_mask(&out[1..], m, 400, 100).is_err());
        assert!(segment_mask(&out, m, 0, 100).is_err());
    }

    #[test]
    fn wrong_sizes_are_errors_not_panics() {
        let m = &MODELS[0];
        assert!(matches!(subject_input(&[], 0, 0, m), Err(MlError::Shape { .. })));
        assert!(matches!(subject_input(&[[0.0; 3]; 3], 2, 2, m), Err(MlError::Shape { .. })));
        assert!(matches!(subject_input(&[[0.0; 3]], usize::MAX, 2, m), Err(MlError::Shape { .. })));
        assert!(matches!(subject_mask(&[0.0; 5], m, 4, 4), Err(MlError::Shape { .. })));
        assert!(matches!(subject_mask(&vec![0.0; m.side * m.side], m, 0, 4), Err(MlError::Shape { .. })));
    }
}
