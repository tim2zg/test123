//! # Rust4K-P2P
//!
//! Decentralized, ultra-low latency voice & 4K 60Hz desktop streaming service.
//!
//! ## Quick Start
//!
//! **Host (sender)**
//! ```bash
//! rust4k-p2p offer
//! # Displays a QR code — receiver scans it.
//! # Enter the answer QR payload from the receiver:
//! rust4k-p2p answer <BASE64_PAYLOAD>
//! ```
//!
//! **Receiver (client)**
//! ```bash
//! rust4k-p2p receive <BASE64_PAYLOAD_FROM_HOST>
//! # Shows a response QR — host scans it back.
//! ```

use anyhow::Result;
use clap::{Parser, Subcommand};
use std::sync::Arc;
use tracing_subscriber::EnvFilter;

mod media;
mod signaling;
mod streaming;
mod webrtc_peer;

// Re-export top-level types for use as a library crate in tests.
pub use media::pipeline;
pub use signaling::{decode_sdp, encode_sdp, print_qr_to_terminal, render_qr_to_file, QrPayload};
pub use webrtc_peer::{munge_sdp_bitrate, PeerSession, TARGET_VIDEO_BITRATE_BPS};

// ── CLI definition ────────────────────────────────────────────────────────────

#[derive(Parser)]
#[command(
    name = "rust4k-p2p",
    about = "Decentralized 4K 60fps desktop streaming via WebRTC + QR-Airgap signaling",
    version
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Phase 1 + 3: Host generates an SDP offer, shows a QR code, and waits
    /// for the receiver to scan it.  Then enter the receiver's answer payload
    /// to complete the handshake and start streaming.
    Offer,

    /// Phase 1 + 3: Decode a remote offer payload (from scanning the host's
    /// QR), generate an answer, display the answer QR, and start receiving.
    Receive {
        /// Base64+Gzip encoded offer payload (from the host's QR code).
        payload: String,
    },

    /// Phase 2 (preview): Capture the local screen and display it in a window
    /// using GStreamer (requires `--features gstreamer`).
    Preview,

    /// Utility: encode an SDP string from stdin and print the QR code.
    EncodeQr,

    /// Utility: decode a QR payload string and print the SDP to stdout.
    DecodeQr {
        /// Base64+Gzip encoded payload.
        payload: String,
    },
}

// ── entry point ───────────────────────────────────────────────────────────────

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::from_default_env().add_directive(
            "rust4k_p2p=info".parse().expect("valid directive"),
        ))
        .init();

    let cli = Cli::parse();

    match cli.command {
        Command::Offer => run_offer().await,
        Command::Receive { payload } => run_receive(&payload).await,
        Command::Preview => run_preview(),
        Command::EncodeQr => run_encode_qr().await,
        Command::DecodeQr { payload } => run_decode_qr(&payload),
    }
}

// ── subcommand handlers ───────────────────────────────────────────────────────

/// Phase 1 + 3: Host side — generate offer → QR → wait for answer → stream.
async fn run_offer() -> Result<()> {
    tracing::info!("=== Rust4K-P2P: HOST MODE ===");

    // Phase 1: create the peer connection & data channel
    let session = PeerSession::new_host().await?;

    // Phase 1: generate SDP offer
    let raw_sdp = session.create_offer().await?;

    // Munge the SDP to request 45 Mbps
    let offer_sdp = munge_sdp_bitrate(&raw_sdp);
    tracing::info!("SDP offer generated ({} bytes)", offer_sdp.len());

    // Phase 3: compress + encode → QR
    let payload = encode_sdp(&offer_sdp)?;
    tracing::info!("QR payload: {} bytes (Base64+Gzip)", payload.as_str().len());

    println!("\n=== OFFER QR CODE — have the receiver scan this ===\n");
    print_qr_to_terminal(&payload)?;
    println!("\n--- Offer payload (paste to receiver if QR is unreadable) ---");
    println!("{}\n", payload.as_str());

    // Wait for the receiver to paste back their answer payload
    println!("Enter the receiver's answer payload and press Enter:");
    let mut answer_payload = String::new();
    std::io::stdin().read_line(&mut answer_payload)?;
    let answer_payload = QrPayload(answer_payload.trim().to_string());

    // Phase 3: decode answer
    let answer_sdp = decode_sdp(&answer_payload)?;
    tracing::info!("Answer SDP received ({} bytes)", answer_sdp.len());

    // Phase 1: complete the handshake
    session.apply_answer(&answer_sdp).await?;
    tracing::info!("Handshake complete — waiting for ICE connection...");
    session.wait_for_connected().await?;
    tracing::info!("✓ P2P link established");

    // Phase 4: start sending the 4K stream
    let conn = Arc::clone(&session.connection);
    streaming::start_host_stream(conn).await?;

    Ok(())
}

/// Phase 1 + 3: Receiver side — scan offer → generate answer QR → receive stream.
async fn run_receive(offer_payload: &str) -> Result<()> {
    tracing::info!("=== Rust4K-P2P: RECEIVER MODE ===");

    // Phase 3: decode offer
    let offer_payload = QrPayload(offer_payload.to_string());
    let offer_sdp = decode_sdp(&offer_payload)?;
    tracing::info!("Offer SDP decoded ({} bytes)", offer_sdp.len());

    // Phase 1: create peer connection and generate answer
    let session = PeerSession::new().await?;
    let answer_sdp = session.create_answer(&offer_sdp).await?;

    // Phase 3: compress + encode answer → QR
    let answer_payload = encode_sdp(&answer_sdp)?;

    println!("\n=== ANSWER QR CODE — have the host scan this ===\n");
    print_qr_to_terminal(&answer_payload)?;
    println!("\n--- Answer payload (paste to host if QR is unreadable) ---");
    println!("{}\n", answer_payload.as_str());

    // Phase 1: wait for the host to complete the ICE handshake
    tracing::info!("Waiting for P2P connection...");
    session.wait_for_connected().await?;
    tracing::info!("✓ P2P link established");

    // Phase 4: start receiving the 4K stream
    let conn = Arc::clone(&session.connection);
    streaming::start_receiver_stream(conn).await?;

    // Keep alive
    tokio::signal::ctrl_c().await?;
    session.close().await?;

    Ok(())
}

/// Phase 2: local screen capture preview (GStreamer required).
fn run_preview() -> Result<()> {
    #[cfg(feature = "gstreamer")]
    {
        use media::pipeline::CapturePipeline;
        let pipeline = CapturePipeline::start_sender()?;
        println!("Preview pipeline running. Press Ctrl-C to stop.");
        // Block until SIGINT is received, then clean up the pipeline.
        let rt = tokio::runtime::Handle::current();
        rt.block_on(async { tokio::signal::ctrl_c().await })?;
        pipeline.stop()?;
        return Ok(());
    }
    #[allow(unreachable_code)]
    {
        eprintln!(
            "GStreamer support is not compiled in.\n\
             Re-build with: cargo build --features gstreamer"
        );
        std::process::exit(1);
    }
}

/// Encode SDP from stdin, print QR to terminal.
async fn run_encode_qr() -> Result<()> {
    use std::io::Read;
    let mut sdp = String::new();
    std::io::stdin().read_to_string(&mut sdp)?;
    let payload = encode_sdp(&sdp)?;
    print_qr_to_terminal(&payload)?;
    println!("{}", payload.as_str());
    Ok(())
}

/// Decode a QR payload back to SDP and print it.
fn run_decode_qr(payload: &str) -> Result<()> {
    let qr = QrPayload(payload.to_string());
    let sdp = decode_sdp(&qr)?;
    println!("{sdp}");
    Ok(())
}
