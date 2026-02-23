//! # 4K P2P Streaming (Phase 4)
//!
//! Combines the GStreamer capture pipeline (Phase 2) with the WebRTC peer
//! connection (Phase 1) to stream 4K 60 fps desktop video over the P2P link
//! established through the QR-Airgap signaling (Phase 3).
//!
//! ## Architecture
//! ```text
//! ┌──────────────── HOST ─────────────────┐       ┌──────────── RECEIVER ────────────────┐
//! │                                       │       │                                      │
//! │  ximagesrc ──► [NVENC/AV1] ──► AppSink│       │  AppSrc ──► [decode] ──► autovideosink│
//! │                     │                 │       │      ▲                               │
//! │             RTP packets               │  UDP  │      │                               │
//! │                     ▼                 │◄─────►│      │ RTP packets                  │
//! │  WebRTC VideoTrack (write_rtp_sample) │       │  WebRTC VideoTrack (on_track)       │
//! └───────────────────────────────────────┘       └──────────────────────────────────────┘
//! ```
//!
//! The two peers exchange SDP + ICE candidates via the QR-Airgap signaling flow
//! (see [`crate::signaling`]) before calling [`start_host_stream`] /
//! [`start_receiver_stream`].

use std::sync::Arc;

use anyhow::Result;
use webrtc::peer_connection::RTCPeerConnection;
#[cfg(feature = "gstreamer")]
use webrtc::{
    rtp_transceiver::rtp_codec::RTCRtpCodecCapability,
    track::track_local::{
        track_local_static_rtp::TrackLocalStaticRTP,
        TrackLocal,
    },
};

/// MIME type string for H.264 video.
#[cfg(feature = "gstreamer")]
pub const MIME_TYPE_H264: &str = "video/H264";

/// Build the codec capability for the default 4K stream (H.264 baseline profile).
#[cfg(feature = "gstreamer")]
pub fn h264_codec() -> RTCRtpCodecCapability {
    RTCRtpCodecCapability {
        mime_type: MIME_TYPE_H264.to_string(),
        clock_rate: 90_000,
        channels: 0,
        sdp_fmtp_line:
            "level-asymmetry-allowed=1;packetization-mode=1;profile-level-id=42e01f".to_string(),
        rtcp_feedback: vec![],
    }
}

/// Add a local H.264 send-only video track to `connection` and return it so
/// Phase-4 can push RTP samples into it.
#[cfg(feature = "gstreamer")]
pub async fn add_video_track(
    connection: &Arc<RTCPeerConnection>,
) -> Result<Arc<TrackLocalStaticRTP>> {
    let track = Arc::new(TrackLocalStaticRTP::new(
        h264_codec(),
        "video".to_string(),
        "rust4k-p2p".to_string(),
    ));

    connection
        .add_track(Arc::clone(&track) as Arc<dyn TrackLocal + Send + Sync>)
        .await?;

    Ok(track)
}

/// Host entry point: begin reading RTP packets from the GStreamer pipeline and
/// writing them to the WebRTC video track.
///
/// **Requires** the `gstreamer` feature to run the real pipeline; without it
/// the function logs a warning and returns immediately (useful for integration
/// tests that only exercise signaling).
pub async fn start_host_stream(connection: Arc<RTCPeerConnection>) -> Result<()> {
    #[cfg(feature = "gstreamer")]
    {
        use crate::media::pipeline::CapturePipeline;
        use gstreamer::prelude::*;
        use gstreamer_app::AppSink;

        let capture = CapturePipeline::start_sender()?;
        let track = add_video_track(&connection).await?;

        let appsink = capture
            .pipeline
            .by_name("rtp_sink")
            .expect("pipeline has no element named 'rtp_sink'")
            .dynamic_cast::<AppSink>()
            .expect("element is not an AppSink");

        let stream_result = async {
            loop {
                let sample = appsink
                    .pull_sample()
                    .map_err(|_| anyhow::anyhow!("appsink EOS or pipeline stopped"))?;

                if let Some(buf) = sample.buffer() {
                    let map = buf.map_readable().expect("buffer not readable");
                    // `write` is a low-level helper on TrackLocalStaticRTP
                    // that accepts raw RTP bytes.
                    track.write(&map).await?;
                }
            }
            #[allow(unreachable_code)]
            anyhow::Ok(())
        }
        .await;

        capture.stop()?;
        return stream_result;
    }

    #[cfg(not(feature = "gstreamer"))]
    {
        let _ = connection;
        tracing::warn!(
            "start_host_stream: GStreamer feature not enabled. \
             Compile with --features gstreamer to send real video."
        );
        // Keep the connection alive so the data channel can still be used.
        tokio::time::sleep(tokio::time::Duration::MAX).await;
        Ok(())
    }
}

/// Receiver entry point: wire up the WebRTC `on_track` callback so that
/// incoming RTP packets are forwarded to the GStreamer decode pipeline for
/// local display.
pub async fn start_receiver_stream(connection: Arc<RTCPeerConnection>) -> Result<()> {
    #[cfg(feature = "gstreamer")]
    {
        use crate::media::pipeline::{CapturePipeline, Encoder};
        use gstreamer::prelude::*;
        use gstreamer_app::AppSrc;

        let recv_pipeline = Arc::new(CapturePipeline::start_receiver(Encoder::NvencH264)?);
        let rp = Arc::clone(&recv_pipeline);

        connection.on_track(Box::new(move |track, _, _| {
            let rp = Arc::clone(&rp);
            Box::pin(async move {
                let appsrc = rp
                    .pipeline
                    .by_name("rtp_src")
                    .expect("no element 'rtp_src'")
                    .dynamic_cast::<AppSrc>()
                    .expect("not an AppSrc");

                loop {
                    match track.read_rtp().await {
                        Ok((pkt, _)) => {
                            let bytes = pkt.marshal().unwrap_or_default();
                            let buf = gstreamer::Buffer::from_slice(bytes);
                            let _ = appsrc.push_buffer(buf);
                        }
                        Err(e) => {
                            tracing::warn!("on_track read error: {e}");
                            break;
                        }
                    }
                }
            })
        }));
    }

    #[cfg(not(feature = "gstreamer"))]
    {
        let _ = connection;
        tracing::warn!(
            "start_receiver_stream: GStreamer feature not enabled. \
             Compile with --features gstreamer to receive real video."
        );
    }

    Ok(())
}
