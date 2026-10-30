//! Accessibility preferences the browser reports through CSS media queries.

/// The user asked the system for less transparency or more contrast: the Liquid Glass themes
/// then draw opaque surfaces (`Services::reduce_transparency`).
// shortcut: read once at startup, a change takes effect on reload; add a `change` listener if
// it should follow the system live.
pub fn reduce_transparency() -> bool {
    let Some(window) = web_sys::window() else { return false };
    ["(prefers-reduced-transparency: reduce)", "(prefers-contrast: more)"]
        .iter()
        .any(|query| window.match_media(query).ok().flatten().is_some_and(|m| m.matches()))
}
