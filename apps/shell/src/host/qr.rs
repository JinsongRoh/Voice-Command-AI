//! QR codes as SVG strings (pairing links and TOTP enrollment).
//!
//! The webview shows the SVG as a `data:` image (allowed by the CSP's
//! `img-src`), never as inline markup.

use qrcode::render::svg;
use qrcode::{EcLevel, QrCode};

/// Dark modules: the brand's deep navy, dark enough for every camera.
const DARK: &str = "#0b1a33";
const LIGHT: &str = "#ffffff";

/// Renders `text` as an SVG QR code with a quiet zone, at least `min_px`
/// wide. Medium error correction: pairing links are long (~250 bytes) and
/// shown on screens, where damage is rare.
pub fn svg(text: &str, min_px: u32) -> Option<String> {
    let code = QrCode::with_error_correction_level(text.as_bytes(), EcLevel::M).ok()?;
    Some(
        code.render::<svg::Color<'_>>()
            .dark_color(svg::Color(DARK))
            .light_color(svg::Color(LIGHT))
            .quiet_zone(true)
            .min_dimensions(min_px, min_px)
            .build(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    const URI: &str = "vca://pair?v=1&k=AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA&id=0011223344556677&n=studio&s=c2VjcmV0c2VjcmV0c2VjcmV0c2VjcmV0&x=1700000000&e=ws%3A%2F%2F127.0.0.1%3A47800%2Fv1%2Fws";

    #[test]
    fn renders_a_square_svg() {
        let svg = svg(URI, 320).expect("encodable");
        assert!(svg.starts_with("<?xml"), "{svg}");
        assert!(svg.contains("<svg xmlns=\"http://www.w3.org/2000/svg\""));
        assert!(svg.ends_with("</svg>"));
        assert!(svg.contains(&format!("fill=\"{DARK}\"")));
        assert!(svg.contains(&format!("fill=\"{LIGHT}\"")));
        // Square and at least the requested size.
        let width: u32 = attr(&svg, "width").parse().unwrap();
        let height: u32 = attr(&svg, "height").parse().unwrap();
        assert_eq!(width, height);
        assert!(width >= 320, "{width}");
        // Nothing but the QR path: no script or foreign content.
        assert!(!svg.contains("<script"));
        assert!(
            !svg.contains("vca://"),
            "the payload is not embedded as text"
        );
    }

    #[test]
    fn different_payloads_give_different_codes() {
        assert_ne!(svg("vca://pair?a", 100), svg("vca://pair?b", 100));
    }

    #[test]
    fn too_long_payloads_fail() {
        assert!(svg(&"x".repeat(5000), 100).is_none());
    }

    fn attr<'a>(svg: &'a str, name: &str) -> &'a str {
        let start = svg.find(&format!(" {name}=\"")).unwrap() + name.len() + 3;
        let end = start + svg[start..].find('"').unwrap();
        &svg[start..end]
    }
}
