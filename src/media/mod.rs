//! # GStreamer Media Engine (Phase 2)
//!
//! Captures the primary display at 3840×2160 @ 60 fps, hardware-encodes the
//! stream with NVENC (H.264/H.265) or AV1 when available, and exposes a
//! GStreamer `AppSink` so Phase-4 can pull encoded RTP packets and feed them
//! into the WebRTC track.
//!
//! ## Enabling
//! Compile with `--features gstreamer` and ensure the GStreamer 1.x
//! development libraries are installed (`libgstreamer1.0-dev`,
//! `libgstreamer-plugins-base1.0-dev`, `libgstreamer-plugins-bad1.0-dev`).
//!
//! ## Pipeline (host / sender)
//! ```text
//! ximagesrc  →  videoconvert  →  [nvh264enc | av1enc | x264enc]
//!           →  rtph264pay / rtpav1pay  →  appsink
//! ```
//!
//! ## Pipeline (client / receiver, preview only)
//! ```text
//! appsrc  →  rtph264depay / rtpav1depay  →  [nvh264dec | avdec_h264]
//!        →  videoconvert  →  autovideosink
//! ```

#[cfg(feature = "gstreamer")]
pub mod pipeline {
    use anyhow::{Context, Result};
    use gstreamer as gst;
    use gstreamer::prelude::*;

    /// Width of the captured / streamed frame.
    pub const FRAME_WIDTH: u32 = 3840;
    /// Height of the captured / streamed frame.
    pub const FRAME_HEIGHT: u32 = 2160;
    /// Target frame rate.
    pub const FRAME_RATE: u32 = 60;
    /// Target encoded bitrate in kbps.
    pub const ENCODE_BITRATE_KBPS: u32 = 45_000;

    /// Available hardware / software video encoder back-ends, in preference
    /// order.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum Encoder {
        /// NVIDIA NVENC H.264 (lowest latency on NVIDIA GPUs)
        NvencH264,
        /// NVIDIA NVENC H.265 / HEVC
        NvencH265,
        /// AMD VCE H.264 via VA-API
        VaapiH264,
        /// Software AV1 (CPU fallback, higher quality at same bitrate)
        Av1Sw,
        /// Software H.264 (x264, always available)
        X264,
    }

    impl Encoder {
        /// GStreamer element name for this encoder.
        pub fn element_name(&self) -> &'static str {
            match self {
                Encoder::NvencH264 => "nvh264enc",
                Encoder::NvencH265 => "nvh265enc",
                Encoder::VaapiH264 => "vaapih264enc",
                Encoder::Av1Sw => "av1enc",
                Encoder::X264 => "x264enc",
            }
        }

        /// RTP payloader element name for this encoder's codec.
        pub fn rtp_payloader(&self) -> &'static str {
            match self {
                Encoder::NvencH264 | Encoder::VaapiH264 | Encoder::X264 => "rtph264pay",
                Encoder::NvencH265 => "rtph265pay",
                Encoder::Av1Sw => "rtpav1pay",
            }
        }

        /// Probe the GStreamer registry for the first available encoder.
        pub fn detect() -> Self {
            let preference = [
                Encoder::NvencH264,
                Encoder::NvencH265,
                Encoder::VaapiH264,
                Encoder::Av1Sw,
                Encoder::X264,
            ];
            for enc in &preference {
                if gst::ElementFactory::find(enc.element_name()).is_some() {
                    tracing::info!("Selected encoder: {enc:?}");
                    return *enc;
                }
            }
            tracing::warn!("No preferred encoder found; falling back to x264enc");
            Encoder::X264
        }
    }

    /// Represents a running GStreamer capture + encode pipeline.
    pub struct CapturePipeline {
        pub pipeline: gst::Pipeline,
        pub encoder: Encoder,
    }

    impl CapturePipeline {
        /// Build and start a capture pipeline for the **host** (sender).
        ///
        /// The encoded RTP stream is exposed through the `appsink` element
        /// named `"rtp_sink"`.  Phase-4 retrieves samples from this sink and
        /// feeds them to the WebRTC video track.
        pub fn start_sender() -> Result<Self> {
            gst::init().context("failed to initialise GStreamer")?;

            let encoder = Encoder::detect();

            let pipeline_str = format!(
                "ximagesrc use-damage=false ! \
                 videoconvert ! \
                 videoscale ! \
                 video/x-raw,width={FRAME_WIDTH},height={FRAME_HEIGHT},framerate={FRAME_RATE}/1 ! \
                 {enc} bitrate={bitrate} ! \
                 {pay} pt=96 ! \
                 appsink name=rtp_sink sync=false max-buffers=2 drop=true",
                FRAME_WIDTH = FRAME_WIDTH,
                FRAME_HEIGHT = FRAME_HEIGHT,
                FRAME_RATE = FRAME_RATE,
                enc = encoder.element_name(),
                bitrate = ENCODE_BITRATE_KBPS,
                pay = encoder.rtp_payloader(),
            );

            let pipeline = gst::parse::launch(&pipeline_str)
                .context("failed to parse GStreamer pipeline")?
                .dynamic_cast::<gst::Pipeline>()
                .expect("element should be a Pipeline");

            pipeline
                .set_state(gst::State::Playing)
                .context("failed to start capture pipeline")?;

            tracing::info!(
                "Capture pipeline started: {FRAME_WIDTH}x{FRAME_HEIGHT}@{FRAME_RATE} \
                 encoder={enc:?}",
                enc = encoder
            );

            Ok(Self { pipeline, encoder })
        }

        /// Build and start a preview pipeline for the **client** (receiver).
        ///
        /// Renders received H.264 RTP packets to the default video sink
        /// (autovideosink) for local preview.
        pub fn start_receiver(encoder: Encoder) -> Result<Self> {
            gst::init().context("failed to initialise GStreamer")?;

            let depay = match encoder {
                Encoder::NvencH265 => "rtph265depay",
                Encoder::Av1Sw => "rtpav1depay",
                _ => "rtph264depay",
            };

            let dec = match encoder {
                Encoder::NvencH264 | Encoder::VaapiH264 | Encoder::X264 => "avdec_h264",
                Encoder::NvencH265 => "avdec_h265",
                Encoder::Av1Sw => "av1dec",
            };

            let pipeline_str = format!(
                "appsrc name=rtp_src ! \
                 {depay} ! \
                 {dec} ! \
                 videoconvert ! \
                 autovideosink sync=false"
            );

            let pipeline = gst::parse::launch(&pipeline_str)
                .context("failed to parse GStreamer receiver pipeline")?
                .dynamic_cast::<gst::Pipeline>()
                .expect("element should be a Pipeline");

            pipeline
                .set_state(gst::State::Playing)
                .context("failed to start receiver pipeline")?;

            tracing::info!("Receiver pipeline started with encoder={encoder:?}");
            Ok(Self { pipeline, encoder })
        }

        /// Stop the pipeline and release GStreamer resources.
        pub fn stop(&self) -> Result<()> {
            self.pipeline
                .set_state(gst::State::Null)
                .context("failed to stop pipeline")?;
            Ok(())
        }
    }
}

// ── stub when the `gstreamer` feature is disabled ────────────────────────────

#[cfg(not(feature = "gstreamer"))]
pub mod pipeline {
    /// Placeholder encoder enum (feature `gstreamer` not enabled).
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum Encoder {
        NvencH264,
        NvencH265,
        VaapiH264,
        Av1Sw,
        X264,
    }

    /// Target frame width (3840 for 4K UHD).
    pub const FRAME_WIDTH: u32 = 3840;
    /// Target frame height (2160 for 4K UHD).
    pub const FRAME_HEIGHT: u32 = 2160;
    /// Target frame rate.
    pub const FRAME_RATE: u32 = 60;
    /// Target encoded bitrate in kbps.
    pub const ENCODE_BITRATE_KBPS: u32 = 45_000;
}
