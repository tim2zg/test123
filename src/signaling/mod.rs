//! # QR-Airgap Signaling Module (Phase 3)
//!
//! Implements server-less, manual WebRTC signaling via QR codes.
//!
//! ## Flow
//! 1. **Offer**: Host compresses SDP with Gzip, encodes as Base64, renders a QR code.
//! 2. **Scan**: Receiver scans the QR with their webcam; the payload is decoded.
//! 3. **Answer**: Receiver generates a response QR for the host to scan back.
//! 4. **Handshake**: `webrtc-rs` performs UDP hole-punch to link the two peers.

use std::io::{Read, Write};

use anyhow::{Context, Result};
use base64::{engine::general_purpose::STANDARD as BASE64, Engine};
use flate2::{read::GzDecoder, write::GzEncoder, Compression};
use image::{ImageEncoder, Luma};
use qrcode::QrCode;

/// A compressed, Base64-encoded SDP payload suitable for QR encoding.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QrPayload(pub String);

impl QrPayload {
    /// Returns the inner Base64 string.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Encode an SDP string into a [`QrPayload`].
///
/// Steps: UTF-8 bytes → Gzip compress → Base64 encode.
pub fn encode_sdp(sdp: &str) -> Result<QrPayload> {
    let compressed = gzip_compress(sdp.as_bytes())
        .context("failed to gzip-compress SDP")?;
    let encoded = BASE64.encode(&compressed);
    Ok(QrPayload(encoded))
}

/// Decode a [`QrPayload`] back into an SDP string.
///
/// Steps: Base64 decode → Gzip decompress → UTF-8 string.
pub fn decode_sdp(payload: &QrPayload) -> Result<String> {
    let compressed = BASE64
        .decode(payload.as_str())
        .context("failed to Base64-decode QR payload")?;
    let sdp_bytes = gzip_decompress(&compressed)
        .context("failed to gzip-decompress QR payload")?;
    String::from_utf8(sdp_bytes).context("SDP payload is not valid UTF-8")
}

/// Render a [`QrPayload`] to a PNG image file at `path`.
///
/// The QR code uses error-correction level M so that a single camera
/// scan is usually sufficient even with minor lens distortion.
pub fn render_qr_to_file(payload: &QrPayload, path: &str) -> Result<()> {
    let code =
        QrCode::new(payload.as_str().as_bytes()).context("failed to generate QR code")?;

    // Build a luma-8 image from the QR matrix (8× scale so it is
    // large enough to scan with a standard webcam).
    let image = code
        .render::<Luma<u8>>()
        .min_dimensions(400, 400)
        .build();

    image
        .save(path)
        .with_context(|| format!("failed to save QR image to '{path}'"))?;

    tracing::info!("QR code written to '{path}'");
    Ok(())
}

/// Render a [`QrPayload`] to a PNG byte vector (useful for embedding in
/// a GUI or transmitting in-memory).
#[allow(dead_code)]
pub fn render_qr_to_png_bytes(payload: &QrPayload) -> Result<Vec<u8>> {
    let code =
        QrCode::new(payload.as_str().as_bytes()).context("failed to generate QR code")?;

    let image = code
        .render::<Luma<u8>>()
        .min_dimensions(400, 400)
        .build();

    let mut buf = Vec::new();
    let encoder = image::codecs::png::PngEncoder::new(&mut buf);
    encoder
        .write_image(
            image.as_raw(),
            image.width(),
            image.height(),
            image::ExtendedColorType::L8,
        )
        .context("failed to encode QR image as PNG")?;

    Ok(buf)
}

/// Pretty-print a [`QrPayload`] to the terminal using Unicode block
/// characters so that the host/receiver can scan it directly from the
/// console (no file needed).
pub fn print_qr_to_terminal(payload: &QrPayload) -> Result<()> {
    let code =
        QrCode::new(payload.as_str().as_bytes()).context("failed to generate QR code")?;

    let string = code
        .render::<char>()
        .quiet_zone(true)
        .module_dimensions(2, 1)
        .dark_color('#')
        .light_color(' ')
        .build();

    println!("{string}");
    Ok(())
}

// ── internal helpers ──────────────────────────────────────────────────────────

fn gzip_compress(data: &[u8]) -> Result<Vec<u8>> {
    let mut encoder = GzEncoder::new(Vec::new(), Compression::best());
    encoder.write_all(data).context("gzip write failed")?;
    encoder.finish().context("gzip finish failed")
}

fn gzip_decompress(data: &[u8]) -> Result<Vec<u8>> {
    let mut decoder = GzDecoder::new(data);
    let mut out = Vec::new();
    decoder.read_to_end(&mut out).context("gzip decompress failed")?;
    Ok(out)
}

// ── tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE_SDP: &str = r#"v=0
o=- 4611731400430699288 2 IN IP4 127.0.0.1
s=-
t=0 0
a=group:BUNDLE 0
a=msid-semantic: WMS
m=video 9 UDP/TLS/RTP/SAVPF 96
c=IN IP4 0.0.0.0
a=rtcp:9 IN IP4 0.0.0.0
a=ice-ufrag:abc
a=ice-pwd:verylongpassword12345
a=fingerprint:sha-256 AA:BB:CC:DD:EE:FF:00:11:22:33:44:55:66:77:88:99:AA:BB:CC:DD:EE:FF:00:11:22:33:44:55:66:77:88:99
a=setup:actpass
a=mid:0
a=sendonly
a=rtpmap:96 H264/90000
a=fmtp:96 level-asymmetry-allowed=1;packetization-mode=1;profile-level-id=42e01f"#;

    #[test]
    fn roundtrip_encode_decode() {
        let payload = encode_sdp(SAMPLE_SDP).expect("encode failed");
        let decoded = decode_sdp(&payload).expect("decode failed");
        assert_eq!(decoded, SAMPLE_SDP);
    }

    #[test]
    fn encoded_is_smaller_than_raw_for_repetitive_sdp() {
        // Gzip should beat raw Base64 for typical SDP strings.
        let payload = encode_sdp(SAMPLE_SDP).expect("encode failed");
        let raw_b64_len = BASE64.encode(SAMPLE_SDP.as_bytes()).len();
        assert!(
            payload.as_str().len() < raw_b64_len,
            "expected compressed payload ({} bytes) < raw Base64 ({} bytes)",
            payload.as_str().len(),
            raw_b64_len
        );
    }

    #[test]
    fn qr_png_bytes_is_non_empty() {
        let payload = encode_sdp(SAMPLE_SDP).expect("encode failed");
        let png = render_qr_to_png_bytes(&payload).expect("render failed");
        assert!(!png.is_empty());
        // PNG magic bytes
        assert_eq!(&png[..4], b"\x89PNG");
    }

    #[test]
    fn decode_invalid_base64_returns_error() {
        let bad = QrPayload("not_valid_base64!!!".to_string());
        assert!(decode_sdp(&bad).is_err());
    }

    #[test]
    fn empty_sdp_roundtrips() {
        let payload = encode_sdp("").expect("encode empty failed");
        let decoded = decode_sdp(&payload).expect("decode empty failed");
        assert_eq!(decoded, "");
    }
}
