// demoscope as a library crate.
//
// The CLI lives in `src/main.rs`. To avoid duplicating the parser, this lib
// pulls the same source file in as a sub-module (via the `#[path]` attribute)
// and exposes the byte-slice entry points it needs.
//
// On `target_arch = "wasm32"` we add a `#[wasm_bindgen]` shim so the parser
// can be driven from a browser: pass a `.dem` blob (and optionally a `.bsp`)
// and get back the same self-contained HTML the CLI writes to disk.

#[path = "main.rs"]
mod cli;

// Re-export the byte-slice parser so consumers of the `rlib` (e.g. tests,
// future tooling) can call it directly without going through wasm-bindgen.
pub use cli::generate_html_string;

#[cfg(target_arch = "wasm32")]
mod wasm {
    use wasm_bindgen::prelude::*;

    /// Parse a Source Engine `.dem` byte buffer (optionally with a matching
    /// `.bsp`) and return the self-contained HTML viewer as a String.
    ///
    /// Pass `name_hint` as the display filename - it surfaces in the demo
    /// metadata header. `jump_threshold` should be `0` for auto-derive
    /// (recommended); any positive value overrides.
    ///
    /// `diff` optionally overlays a second demo's recorder path as a ghost for
    /// side-by-side comparison (Source demos only; ignored for Quake/GoldSrc).
    /// `diff_name` is the ghost's display label.
    #[wasm_bindgen]
    pub fn parse_demo_to_html(
        demo: &[u8],
        bsp: Option<Vec<u8>>,
        name_hint: &str,
        jump_threshold: f32,
        diff: Option<Vec<u8>>,
        diff_name: &str,
    ) -> Result<String, JsValue> {
        // Route panics to console.error so a browser session shows a real
        // stack trace instead of "unreachable executed".
        console_error_panic_hook::set_once();
        // Source 2 (PBDEMS2: CS2 / Dota 2 / Deadlock) - checked first: its magic
        // is unambiguous, whereas the Quake route matches by `.dem` extension and
        // would otherwise swallow it. Metadata-only viewer for now.
        if super::cli::source2::is_source2(demo) {
            // CS2 maps are VPK-packed Source 2 resources, not VBSP — so the
            // optional second buffer doubles as the `.vpk` map pak here (the
            // drag-and-drop UI accepts a `.bsp` or a `.vpk` beside the demo).
            return super::cli::generate_source2_html(demo, bsp.as_deref(), None, name_hint)
                .map_err(|e| JsValue::from_str(&e.to_string()));
        }
        // Quake-family demos (Q1/Q2/Q3) route to the dedicated decoder; HL2DEMO
        // demos fall through to the Source path. Detection checks the HL2DEMO
        // magic first, so Source demos are never misclassified.
        if let Some(kind) = super::cli::quake::detect(name_hint, demo) {
            return super::cli::generate_quake_html(demo, kind, bsp.as_deref(), name_hint)
                .map_err(|e| JsValue::from_str(&e.to_string()));
        }
        // GoldSrc (HL1) HLDEMO container - recorder POV (+ optional BSP overlay).
        if super::cli::goldsrc::is_goldsrc(demo) {
            return super::cli::generate_goldsrc_html(demo, bsp.as_deref(), name_hint)
                .map_err(|e| JsValue::from_str(&e.to_string()));
        }
        // A second demo overlays as translucent ghosts in the same scene.
        let diff_arg = diff.as_deref().map(|b| (b, diff_name));
        super::cli::generate_html_string(demo, bsp.as_deref(), name_hint, jump_threshold, diff_arg)
            .map_err(|e| JsValue::from_str(&e.to_string()))
    }

    /// Tiny version-stamp so the JS side can verify it's loaded the wasm it
    /// was built against. Returns the crate version as `"X.Y.Z"`.
    #[wasm_bindgen]
    pub fn version() -> String {
        env!("CARGO_PKG_VERSION").to_string()
    }
}
