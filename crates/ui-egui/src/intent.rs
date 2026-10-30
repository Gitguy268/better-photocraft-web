//! Plain-language commands for the ⌘F palette: "blur 5", "make it brighter", "remove the
//! background", "upscale 4x" become one command with its parameters, run without a dialog.
//! English and Dutch phrases; everything runs locally, nothing is sent anywhere.
// shortcut: keyword + synonym matching, no language model; upgrade to a local embedding model
// (e.g. MiniLM, Apache-2.0) if phrases miss too often.

use serde_json::{Value, json};

/// What a phrase asks for, given the number in it (if any) and its lowercase words.
type Build = fn(Option<f64>, &[&str]) -> (&'static str, Value);

/// Phrases, most specific first: the first rule with a phrase in the query wins.
const RULES: &[(&[&str], Build)] = &[
    (
        &["remove background", "remove the background", "cut out", "cutout", "achtergrond verwijderen", "verwijder achtergrond", "verwijder de achtergrond"],
        |_, _| ("layer.removeBackground", json!({})),
    ),
    (&["select subject", "select the subject", "onderwerp selecteren", "selecteer onderwerp", "selecteer het onderwerp"], |_, _| ("select.subject", json!({}))),
    (&["denoise", "remove noise", "remove grain", "less noise", "noise", "grain", "ontruis", "verwijder ruis", "minder ruis", "ruis", "korrel"], |_, _| {
        ("filter.ai.denoise", json!({"model": "auto"}))
    }),
    (&["upscale", "enlarge", "bigger", "opschalen", "vergroot", "vergroten", "groter"], |n, _| {
        ("filter.ai.upscale", json!({"scale": if n.is_some_and(|n| n >= 3.0) { 4 } else { 2 }}))
    }),
    (&["blur", "vervaag", "vervagen", "wazig"], |n, _| ("filter.blur.gaussianBlur", json!({"radius": n.unwrap_or(5.0).clamp(0.1, 1000.0)}))),
    (&["sharpen", "sharper", "verscherp", "verscherpen", "scherper"], |n, _| {
        ("filter.sharpen.unsharpMask", json!({"amount": n.unwrap_or(80.0).clamp(1.0, 500.0), "radius": 1.0, "threshold": 0}))
    }),
    (&["brighter", "lighter", "lichter", "helderder"], |n, _| {
        ("image.adjustments.brightnessContrast", json!({"brightness": n.unwrap_or(30.0).abs().min(150.0), "contrast": 0}))
    }),
    (&["darker", "donkerder"], |n, _| ("image.adjustments.brightnessContrast", json!({"brightness": -n.unwrap_or(30.0).abs().min(150.0), "contrast": 0}))),
    (&["contrast"], |n, _| ("image.adjustments.brightnessContrast", json!({"brightness": 0, "contrast": n.unwrap_or(25.0).clamp(-50.0, 100.0)}))),
    (&["black and white", "grayscale", "greyscale", "desaturate", "zwart-wit", "zwartwit", "grijstinten"], |_, _| ("image.adjustments.desaturate", json!({}))),
    (&["invert", "negative", "omkeren", "negatief", "inverteren"], |_, _| ("image.adjustments.invert", json!({}))),
    (&["rotate", "turn", "draai", "draaien", "roteer"], |n, words| {
        let id = if n.is_some_and(|n| (n.abs() - 180.0).abs() < 1.0) {
            "image.imageRotation.180"
        } else if words.iter().any(|w| matches!(*w, "left" | "links" | "counterclockwise" | "ccw")) || n.is_some_and(|n| n < 0.0) {
            "image.imageRotation.90ccw"
        } else {
            "image.imageRotation.90cw"
        };
        (id, json!({}))
    }),
    (&["flip", "mirror", "spiegel", "spiegelen"], |_, words| {
        let vertical = words.iter().any(|w| matches!(*w, "vertical" | "vertically" | "upside" | "verticaal"));
        (if vertical { "image.imageRotation.flipCanvasVertical" } else { "image.imageRotation.flipCanvasHorizontal" }, json!({}))
    }),
    (&["auto tone", "fix colors", "fix colours", "enhance", "verbeter", "verbeteren"], |_, _| ("image.autoTone", json!({}))),
];

/// A word as a number ("5", "-20", "4x", "12px", "90°", "2,5").
fn as_number(w: &str) -> Option<f64> {
    w.trim_end_matches("px").trim_end_matches(['x', '%', '°']).replace(',', ".").parse::<f64>().ok().filter(|v| v.is_finite())
}

/// The first number in the query.
fn number(words: &[&str]) -> Option<f64> {
    words.iter().find_map(|w| as_number(w))
}

/// Words a phrase may carry besides its keyword without asking for something else: what the
/// builders read (direction, axis), units, and filler. Any other word ("motion blur", "invert
/// selection", "make the brush bigger") means the query names a command of its own.
const ALLOWED: &[&str] = &[
    "left",
    "right",
    "links",
    "rechts",
    "clockwise",
    "counterclockwise",
    "cw",
    "ccw",
    "vertical",
    "vertically",
    "horizontal",
    "horizontally",
    "verticaal",
    "horizontaal",
    "upside",
    "down",
    "degrees",
    "degree",
    "graden",
    "px",
    "pixels",
    "%",
    "x",
    "times",
    "make",
    "it",
    "the",
    "a",
    "an",
    "by",
    "to",
    "more",
    "bit",
    "little",
    "image",
    "photo",
    "picture",
    "please",
    "maak",
    "het",
    "de",
    "een",
    "iets",
    "wat",
    "beetje",
    "afbeelding",
    "foto",
    "meer",
    "naar",
    "met",
    "keer",
];

/// The command and parameters a plain-language query asks for. A single word is left to the
/// ordinary search ("blur" lists every blur), so a phrase needs a number or a second word.
pub fn parse(query: &str) -> Option<(&'static str, Value)> {
    let q = query.trim().to_lowercase();
    let words: Vec<&str> = q.split(|c: char| c.is_whitespace() || matches!(c, '.' | '!' | '?' | ':')).filter(|w| !w.is_empty()).collect();
    if words.len() < 2 {
        return None;
    }
    let joined = words.join(" ");
    let is = |w: &str, k: &str| w == k || w.strip_suffix('s') == Some(k);
    let has = |phrase: &str| if phrase.contains(' ') { joined.contains(phrase) } else { words.iter().any(|w| is(w, phrase)) };
    let (phrases, build) = RULES.iter().find(|(phrases, _)| phrases.iter().any(|p| has(p)))?;
    // Every word is the rule's own, a number or an allowed extra.
    let own: Vec<&str> = phrases.iter().filter(|p| has(p)).flat_map(|p| p.split(' ')).collect();
    let covered = |w: &&str| own.iter().any(|k| is(w, k)) || as_number(w).is_some() || ALLOWED.contains(w);
    words.iter().all(covered).then(|| build(number(&words), &words))
}

/// A short human summary of the parameters, e.g. "radius 5" (empty when there are none to show).
pub fn summary(params: &Value) -> String {
    let Some(obj) = params.as_object() else { return String::new() };
    obj.iter()
        .filter(|(k, v)| *k != "model" && v.as_f64().is_some_and(|n| n != 0.0))
        .map(|(k, v)| format!("{k} {}", v.as_f64().map_or(String::new(), |n| if n.fract() == 0.0 { format!("{n:.0}") } else { format!("{n}") })))
        .collect::<Vec<_>>()
        .join(", ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn phrases_become_commands_with_their_numbers() {
        let cases: &[(&str, &str, Value)] = &[
            ("blur 5", "filter.blur.gaussianBlur", json!({"radius": 5.0})),
            ("Blur by 12px", "filter.blur.gaussianBlur", json!({"radius": 12.0})),
            ("vervaag 2,5", "filter.blur.gaussianBlur", json!({"radius": 2.5})),
            ("make it brighter", "image.adjustments.brightnessContrast", json!({"brightness": 30.0, "contrast": 0})),
            ("darker 50", "image.adjustments.brightnessContrast", json!({"brightness": -50.0, "contrast": 0})),
            ("remove the background", "layer.removeBackground", json!({})),
            ("achtergrond verwijderen", "layer.removeBackground", json!({})),
            ("upscale 4x", "filter.ai.upscale", json!({"scale": 4})),
            ("make it bigger", "filter.ai.upscale", json!({"scale": 2})),
            ("remove noise", "filter.ai.denoise", json!({"model": "auto"})),
            ("verwijder ruis", "filter.ai.denoise", json!({"model": "auto"})),
            ("rotate 90 degrees to the left", "image.imageRotation.90ccw", json!({})),
            ("make it a bit sharper", "filter.sharpen.unsharpMask", json!({"amount": 80.0, "radius": 1.0, "threshold": 0})),
            ("sharpen 120", "filter.sharpen.unsharpMask", json!({"amount": 120.0, "radius": 1.0, "threshold": 0})),
            ("rotate 180", "image.imageRotation.180", json!({})),
            ("rotate left", "image.imageRotation.90ccw", json!({})),
            ("draai 90", "image.imageRotation.90cw", json!({})),
            ("flip vertically", "image.imageRotation.flipCanvasVertical", json!({})),
            ("make it black and white", "image.adjustments.desaturate", json!({})),
            ("select the subject", "select.subject", json!({})),
        ];
        for (q, id, params) in cases {
            assert_eq!(parse(q), Some((*id, params.clone())), "{q}");
        }
    }

    #[test]
    fn single_words_and_nonsense_are_left_to_the_search() {
        for q in ["blur", "", "   ", "levels", "hello there", "open file", "remove"] {
            assert_eq!(parse(q), None, "{q:?}");
        }
        // Names of other commands that contain a keyword stay searches, so Enter runs what they name.
        for q in [
            "motion blur",
            "lens blur",
            "surface blur",
            "add noise",
            "reduce noise",
            "invert selection",
            "auto contrast",
            "smart sharpen",
            "make the brush bigger",
            "rotate the layer",
            "flip layer horizontal",
        ] {
            assert_eq!(parse(q), None, "{q:?}");
        }
        // Hostile numbers are clamped to what the command accepts.
        assert_eq!(parse("blur 1e309"), None, "an infinite number is no number, and no allowed word");
        assert_eq!(parse("brighter 9000"), Some(("image.adjustments.brightnessContrast", json!({"brightness": 150.0, "contrast": 0}))));
        assert_eq!(summary(&json!({"radius": 5.0, "model": "auto"})), "radius 5");
        assert_eq!(summary(&json!({"brightness": -2.5, "contrast": 0})), "brightness -2.5");
        assert_eq!(summary(&Value::Null), "");
    }

    /// Every rule names a real command, and the parameters it builds are ones the command takes.
    #[test]
    fn every_rule_runs_a_real_command() {
        let mut s = photocraft_engine::Session::new();
        s.execute("file.new", json!({"width": 32, "height": 32})).unwrap();
        s.execute("layer.new.layer", json!({})).unwrap();
        for (phrases, build) in RULES {
            let (id, params) = build(Some(4.0), &["x"]);
            assert!(photocraft_engine::commands::find(id).is_some(), "{phrases:?}: {id} is not a command");
            if !matches!(id, "layer.removeBackground" | "select.subject" | "filter.ai.upscale" | "filter.ai.denoise" | "image.autoTone") {
                s.execute(id, params.clone()).unwrap_or_else(|e| panic!("{id} {params}: {e}"));
            }
        }
    }
}
