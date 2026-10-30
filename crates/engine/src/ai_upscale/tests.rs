use super::*;
use crate::ai_cmds::set_external;

const W: u32 = 200;
const H: u32 = 150;

/// A colour that changes with every pixel, so a misplaced tile shows.
fn colour(x: i32, y: i32) -> [f32; 3] {
    [(x as f32 * 7.0 % 256.0) / 255.0, (y as f32 * 5.0 % 256.0) / 255.0, ((x + y) as f32 * 3.0 % 256.0) / 255.0]
}

fn session(params: Value) -> Session {
    let mut s = painted(params, W, H, colour);
    set_external(&mut s, ["realesrgan"]);
    s
}

/// A new document whose Background is filled by `f`, whatever its channels.
fn painted(params: Value, w: u32, h: u32, f: impl Fn(i32, i32) -> [f32; 3]) -> Session {
    let mut s = Session::new();
    s.execute("file.new", params).unwrap();
    s.edit("paint", |doc, _| {
        let bg = doc.layers[0].surface_mut().unwrap();
        let (n, alpha) = (bg.channels(), bg.format().alpha);
        let mut data = Vec::new();
        for y in 0..h as i32 {
            for x in 0..w as i32 {
                let c = f(x, y);
                data.extend((0..n - usize::from(alpha)).map(|i| c[i % 3]));
                data.extend(alpha.then_some(1.0));
            }
        }
        bg.write_region(Rect::new(0, 0, w as i32, h as i32), &data);
        Ok(())
    })
    .unwrap();
    s
}

const UP: usize = 4;

/// A stand-in for the model: every input pixel becomes a 4 × 4 block.
fn blocks(input: &Tensor) -> Vec<f32> {
    let side = input.shape[2];
    let big = side * UP;
    (0..3 * big * big).map(|i| input.data[(i / (big * big)) * side * side + ((i % (big * big)) / big / UP) * side + (i % big) / UP]).collect()
}

/// Plays the browser shell: drives the job tile by tile, then hands it back.
fn answer(s: &mut Session, p: &Value) -> usize {
    let mut job = upscale_request(s, p).unwrap().expect("a job for the shell");
    let mut tiles = 0;
    while let Some(input) = job.next_input() {
        assert_eq!(input.shape, [1, 3, 128, 128]);
        job.put(&blocks(&input)).unwrap();
        tiles += 1;
    }
    assert!(job.is_done() && job.progress() == 1.0);
    upscale_ready(s, job);
    tiles
}

fn pixel(s: &Session, layer: usize, x: i32, y: i32) -> Vec<f32> {
    s.active().unwrap().doc.layers[layer].surface().unwrap().pixel(x, y)
}

#[test]
fn every_tile_lands_in_its_place_at_4x_and_2x_and_undoes() {
    for scale in [4, 2] {
        let mut s = session(json!({"width": W, "height": H}));
        let p = json!({"scale": scale, "model": "realesrgan"});
        let tiles = answer(&mut s, &p);
        assert_eq!(tiles, 4, "200 x 150 in 128 px tiles with overlap");
        let before = s.active().unwrap().history.past_len();
        let r = s.execute(CMD, p).unwrap();
        assert_eq!(
            (r["width"].as_u64(), r["height"].as_u64(), r["model"].as_str()),
            (Some(u64::from(W * scale)), Some(u64::from(H * scale)), Some("realesrgan"))
        );
        assert_eq!(s.active().unwrap().doc.size.width, W * scale);
        let k = scale as i32;
        // Across tile seams (x = 112 here), image edges and corners.
        for (x, y) in [(0, 0), (5, 9), (111, 60), (112, 60), (113, 61), (199, 149), (64, 148), (198, 3)] {
            for (dx, dy) in [(0, 0), (k - 1, k - 1)] {
                let got = pixel(&s, 0, x * k + dx, y * k + dy);
                let want = colour(x, y);
                assert!((0..3).all(|c| (got[c] - want[c]).abs() < 2.5 / 255.0), "{scale}x: pixel ({x}, {y}) + ({dx}, {dy}): {got:?} vs {want:?}");
            }
        }
        assert_eq!(s.active().unwrap().history.past_len(), before + 1, "one undo step");
        s.execute("edit.undo", json!({})).unwrap();
        assert_eq!(s.active().unwrap().doc.size.width, W);
    }
}

#[test]
fn a_small_layer_keeps_its_place_and_its_transparency() {
    let mut s = session(json!({"width": W, "height": H}));
    s.execute("layer.new.layer", json!({})).unwrap();
    s.edit("paint", |doc, _| {
        // Smaller than one tile, away from the origin: the model sees its edge repeated.
        let (r, mut data) = (Rect::new(20, 30, 90, 70), Vec::new());
        for y in r.y0..r.y1 {
            for x in r.x0..r.x1 {
                data.extend(colour(x, y).into_iter().chain([1.0]));
            }
        }
        doc.layers[1].surface_mut().unwrap().write_region(r, &data);
        Ok(())
    })
    .unwrap();
    let p = json!({"scale": 4, "model": "realesrgan"});
    assert_eq!(answer(&mut s, &p), 5, "4 tiles for the Background, 1 for the small layer");
    s.execute(CMD, p).unwrap();
    // Colours right up to the layer's edge; its alpha is the resized layer's own, so the outermost
    // pixels are soft, as after Image Size.
    for (x, y) in [(20, 30), (55, 50), (89, 69)] {
        let (got, want) = (pixel(&s, 1, x * 4 + 2, y * 4 + 2), colour(x, y));
        assert!((0..3).all(|c| (got[c] - want[c]).abs() < 2.5 / 255.0), "({x}, {y}): {got:?} vs {want:?}");
    }
    assert!(pixel(&s, 1, 55 * 4, 50 * 4)[3] > 0.99 && pixel(&s, 1, 22 * 4, 32 * 4)[3] > 0.99);
    assert!(pixel(&s, 1, 10 * 4, 10 * 4)[3] < 0.01 && pixel(&s, 1, 150 * 4, 100 * 4)[3] < 0.01, "outside the layer's pixels it stays transparent");
}

#[test]
fn sixteen_bit_and_grayscale_documents_are_enlarged_too() {
    let mut s = session(json!({"width": W, "height": H, "depth": 16}));
    let p = json!({"scale": 2, "model": "realesrgan"});
    answer(&mut s, &p);
    s.execute(CMD, p.clone()).unwrap();
    let (got, want) = (pixel(&s, 0, 77 * 2, 41 * 2), colour(77, 41));
    assert!((0..3).all(|c| (got[c] - want[c]).abs() < 2.5 / 255.0), "16-bit: {got:?} vs {want:?}");

    let mut s = session(json!({"width": W, "height": H, "mode": "gray"}));
    answer(&mut s, &p);
    s.execute(CMD, p).unwrap();
    assert_eq!(s.active().unwrap().doc.size.width, W * 2);
    assert!((pixel(&s, 0, 30 * 2, 20 * 2)[0] - colour(30, 20)[0]).abs() < 4.0 / 255.0, "gray stays gray");
}

#[test]
fn without_a_model_it_resamples_and_wrong_requests_are_errors() {
    let mut plain = session(json!({"width": W, "height": H}));
    set_external(&mut plain, []);
    assert!(upscale_request(&plain, &json!({})).unwrap().is_none());
    let r = plain.execute(CMD, json!({"scale": "4"})).unwrap();
    assert_eq!((r["model"].as_str(), r["width"].as_u64()), (Some("classical"), Some(800)), "the dialog's choice arrives as a string");
    assert!(plain.execute(CMD, json!({"model": "realesrgan"})).unwrap_err().to_string().contains("not installed"));

    let mut s = session(json!({"width": W, "height": H}));
    for p in [json!({"scale": 3}), json!({"scale": "big"}), json!({"scale": -2}), json!({"scale": 4.5}), json!({"model": 7}), json!({"model": "u2netp"})] {
        assert!(s.execute(CMD, p.clone()).is_err(), "{p}");
    }
    assert_eq!(s.active().unwrap().doc.size.width, W, "failed commands change nothing");
    // A job made before an edit is stale: asked for by name it fails, "auto" resamples instead.
    let p = json!({"scale": 2, "model": "realesrgan"});
    answer(&mut s, &p);
    s.execute("image.adjustments.invert", json!({})).unwrap();
    assert!(s.execute(CMD, p).unwrap_err().to_string().contains("no result"));
    answer(&mut s, &json!({"scale": 2}));
    s.execute("image.adjustments.invert", json!({})).unwrap();
    assert_eq!(s.execute(CMD, json!({"scale": 2})).unwrap()["model"], "classical");
    // Colour modes a model can't take are refused with a way forward; the classical path takes them.
    let mut cmyk = session(json!({"width": W, "height": H, "mode": "cmyk"}));
    let e = upscale_request(&cmyk, &json!({})).err().map(|e| e.to_string()).unwrap_or_default();
    assert!(e.contains("RGB and grayscale"), "{e}");
    assert_eq!(cmyk.execute(CMD, json!({"model": "classical"})).unwrap()["model"], "classical");
    assert!(Session::new().execute(CMD, json!({})).is_err(), "no document");
    // An absurd size is refused before anything is allocated.
    let mut huge = Session::new();
    huge.execute("file.new", json!({"width": 200_000, "height": 200_000})).unwrap();
    assert!(huge.execute(CMD, json!({"scale": 4})).is_err());
}

/// With the real model (never committed; `PHOTOCRAFT_MODEL_DIR` holding `realesrgan.onnx` and
/// `realesrgan.data`): the command runs it in this process and the picture is still the picture.
#[cfg(feature = "ml-runtime")]
#[test]
fn the_real_model_enlarges_a_document_in_process() {
    let Some(dir) = std::env::var_os("PHOTOCRAFT_MODEL_DIR").map(std::path::PathBuf::from).filter(|d| d.join("realesrgan.data").is_file()) else {
        eprintln!("skipped: set PHOTOCRAFT_MODEL_DIR to a folder with realesrgan.onnx and realesrgan.data");
        return;
    };
    // Left half dark, right half light.
    let mut s = painted(json!({"width": 160, "height": 140}), 160, 140, |x, _| if x < 80 { [0.1, 0.1, 0.12] } else { [0.9, 0.88, 0.85] });
    s.ai.model_dir = Some(dir);
    assert!(upscale_request(&s, &json!({})).unwrap().is_none(), "runs here, not in the shell");
    let r = s.execute(CMD, json!({"scale": 4})).unwrap();
    assert_eq!((r["model"].as_str(), r["width"].as_u64(), r["height"].as_u64()), (Some("realesrgan"), Some(640), Some(560)));
    let (dark, light) = (pixel(&s, 0, 100, 300), pixel(&s, 0, 540, 300));
    assert!(dark[0] < 0.25 && light[0] > 0.75, "{dark:?} {light:?}");
}

/// A stand-in for the denoiser: every pixel a little darker, so the tests see it ran.
fn darker(input: &Tensor) -> Vec<f32> {
    input.data.iter().map(|v| v * 0.5).collect()
}

/// Plays the browser shell for AI Denoise.
fn answer_denoise(s: &mut Session, p: &Value) -> usize {
    let mut job = denoise_request(s, p).unwrap().expect("a job for the shell");
    let mut tiles = 0;
    while let Some(input) = job.next_input() {
        assert_eq!(input.shape, [1, 3, 128, 128]);
        job.put(&darker(&input)).unwrap();
        tiles += 1;
    }
    upscale_ready(s, job);
    tiles
}

#[test]
fn denoise_works_on_the_active_layer_only_in_one_undo_step() {
    let mut s = painted(json!({"width": W, "height": H}), W, H, colour);
    s.execute("layer.new.layer", json!({})).unwrap();
    s.edit("paint", |doc, _| {
        let r = Rect::new(0, 0, W as i32, H as i32);
        let data: Vec<f32> = (0..W * H).flat_map(|_| [0.8, 0.6, 0.4, 1.0]).collect();
        doc.layers[1].surface_mut().unwrap().write_region(r, &data);
        Ok(())
    })
    .unwrap();
    set_external(&mut s, ["scunet"]);
    let p = json!({"model": "scunet"});
    assert_eq!(answer_denoise(&mut s, &p), 4, "200 x 150 in 128 px tiles, the active layer only");
    let before = s.active().unwrap().history.past_len();
    assert_eq!(s.execute(DENOISE_CMD, p).unwrap()["model"], "scunet");
    assert_eq!(s.active().unwrap().doc.size.width, W, "same size");
    for (x, y) in [(0, 0), (111, 60), (112, 60), (199, 149)] {
        let got = pixel(&s, 1, x, y);
        assert!((got[0] - 0.4).abs() < 2.5 / 255.0 && (got[2] - 0.2).abs() < 2.5 / 255.0, "({x}, {y}): {got:?}");
        let want = colour(x, y);
        assert!((0..3).all(|c| (pixel(&s, 0, x, y)[c] - want[c]).abs() < 1.0 / 255.0), "the Background is untouched");
    }
    assert_eq!(s.active().unwrap().history.past_len(), before + 1, "one undo step");
    s.execute("edit.undo", json!({})).unwrap();
    assert!((pixel(&s, 1, 50, 50)[0] - 0.8).abs() < 1.0 / 255.0);
}

#[test]
fn denoise_without_a_model_reduces_noise_and_wrong_requests_are_errors() {
    let mut s = painted(json!({"width": 64, "height": 64}), 64, 64, colour);
    assert!(denoise_request(&s, &json!({})).unwrap().is_none(), "nothing installed: no job for the shell");
    assert_eq!(s.execute(DENOISE_CMD, json!({})).unwrap()["model"], "classical");
    assert!(s.execute(DENOISE_CMD, json!({"model": "scunet"})).unwrap_err().to_string().contains("not installed"));
    for p in [json!({"model": 7}), json!({"model": "realesrgan"}), json!({"model": "nope"})] {
        assert!(s.execute(DENOISE_CMD, p.clone()).is_err(), "{p}");
    }
    // A stale job is not applied: asked for by name it fails, "auto" falls back.
    set_external(&mut s, ["scunet"]);
    answer_denoise(&mut s, &json!({}));
    s.execute("image.adjustments.invert", json!({})).unwrap();
    assert!(s.execute(DENOISE_CMD, json!({"model": "scunet"})).unwrap_err().to_string().contains("no result"));
    // A non-pixel layer and no document are refused.
    s.execute("layer.new.group", json!({})).unwrap();
    assert!(s.execute(DENOISE_CMD, json!({})).unwrap_err().to_string().contains("pixel layer"));
    assert!(Session::new().execute(DENOISE_CMD, json!({})).is_err(), "no document");
}

/// With the real denoiser (never committed; `PHOTOCRAFT_MODEL_DIR` holding `scunet.onnx` and
/// `scunet.data`): the command runs it in this process and noise goes down.
#[cfg(feature = "ml-runtime")]
#[test]
fn the_real_denoiser_cleans_a_layer_in_process() {
    let Some(dir) = std::env::var_os("PHOTOCRAFT_MODEL_DIR").map(std::path::PathBuf::from).filter(|d| d.join("scunet.data").is_file()) else {
        eprintln!("skipped: set PHOTOCRAFT_MODEL_DIR to a folder with scunet.onnx and scunet.data");
        return;
    };
    let noise = |x: i32, y: i32| {
        let mut h = (x as u32).wrapping_mul(0x9E37_79B9) ^ (y as u32).wrapping_mul(0x85EB_CA6B);
        h ^= h >> 15;
        h = h.wrapping_mul(0x2C1B_3C6D);
        h ^= h >> 12;
        (h % 1000) as f32 / 1000.0 - 0.5
    };
    let mut s = painted(json!({"width": 150, "height": 100}), 150, 100, |x, y| [0.5 + 0.2 * noise(x, y); 3]);
    s.ai.model_dir = Some(dir);
    assert!(denoise_request(&s, &json!({})).unwrap().is_none(), "runs here, not in the shell");
    assert_eq!(s.execute(DENOISE_CMD, json!({})).unwrap()["model"], "scunet");
    let v: Vec<f32> = (10..90).flat_map(|y| (10..140).map(move |x| (x, y))).map(|(x, y)| pixel(&s, 0, x, y)[1]).collect();
    let mean = v.iter().sum::<f32>() / v.len() as f32;
    let sd = (v.iter().map(|a| (a - mean).powi(2)).sum::<f32>() / v.len() as f32).sqrt();
    // Measured 0.034: the PSNR variant is conservative on fine grain.
    assert!((mean - 0.5).abs() < 0.05 && sd < 0.042, "mean {mean}, noise left {sd} (it was about 0.058)");
}
