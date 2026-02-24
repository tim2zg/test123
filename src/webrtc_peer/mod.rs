//! # WebRTC P2P Module (Phase 1)
//!
//! Provides helpers to create a `webrtc-rs` [`RTCPeerConnection`] pre-configured for:
//! * STUN-based ICE (Google's public servers + self-provided list)
//! * A data channel for low-latency round-trip ping measurement
//! * Video / audio track attachment (used by Phase 4)
//!
//! ## Data-channel ping (Phase 1)
//! ```no_run
//! # tokio_test::block_on(async {
//! use rust4k_p2p::webrtc_peer::PeerSession;
//! let session = PeerSession::new_host().await.unwrap();
//! let offer_sdp = session.create_offer().await.unwrap();
//! println!("Offer SDP:\n{offer_sdp}");
//! # });
//! ```

use anyhow::{Context, Result};
use std::sync::Arc;
use webrtc::{
    api::{
        interceptor_registry::register_default_interceptors,
        media_engine::MediaEngine,
        APIBuilder,
    },
    data_channel::{
        data_channel_message::DataChannelMessage, RTCDataChannel,
    },
    ice_transport::{
        ice_candidate::RTCIceCandidateInit,
        ice_server::RTCIceServer,
    },
    interceptor::registry::Registry,
    peer_connection::{
        configuration::RTCConfiguration,
        peer_connection_state::RTCPeerConnectionState,
        sdp::session_description::RTCSessionDescription,
        RTCPeerConnection,
    },
};

/// STUN servers used for ICE candidate gathering.
const STUN_SERVERS: &[&str] = &[
    "stun:stun.l.google.com:19302",
    "stun:stun1.l.google.com:19302",
    "stun:stun2.l.google.com:19302",
];

/// Desired 4K 60 fps video bitrate in bits-per-second.
pub const TARGET_VIDEO_BITRATE_BPS: u32 = 45_000_000; // 45 Mbps

/// Wraps an [`RTCPeerConnection`] and manages the data channel used for
/// Phase-1 ping tests as well as media tracks added in Phase 4.
pub struct PeerSession {
    pub connection: Arc<RTCPeerConnection>,
}

impl PeerSession {
    /// Build a new [`RTCPeerConnection`] with the default STUN servers.
    pub async fn new() -> Result<Self> {
        let mut media_engine = MediaEngine::default();
        media_engine
            .register_default_codecs()
            .context("failed to register default codecs")?;

        let mut registry = Registry::new();
        registry = register_default_interceptors(registry, &mut media_engine)
            .context("failed to register interceptors")?;

        let api = APIBuilder::new()
            .with_media_engine(media_engine)
            .with_interceptor_registry(registry)
            .build();

        let config = RTCConfiguration {
            ice_servers: STUN_SERVERS
                .iter()
                .map(|url| RTCIceServer {
                    urls: vec![url.to_string()],
                    ..Default::default()
                })
                .collect(),
            ..Default::default()
        };

        let connection = Arc::new(
            api.new_peer_connection(config)
                .await
                .context("failed to create RTCPeerConnection")?,
        );

        Ok(Self { connection })
    }

    /// Convenience constructor for the **host** (offer) side.
    ///
    /// Opens a reliable, ordered data channel named `"ping"` so that
    /// Phase-1 latency measurements can begin as soon as ICE connects.
    pub async fn new_host() -> Result<Self> {
        let session = Self::new().await?;
        session.open_ping_channel().await?;
        Ok(session)
    }

    /// Create the `"ping"` data channel and wire up echo / latency logging.
    pub async fn open_ping_channel(&self) -> Result<Arc<RTCDataChannel>> {
        let dc = self
            .connection
            .create_data_channel("ping", None)
            .await
            .context("failed to create data channel")?;

        let dc2 = Arc::clone(&dc);
        dc.on_open(Box::new(move || {
            let dc3 = Arc::clone(&dc2);
            Box::pin(async move {
                tracing::info!("Data channel 'ping' open – sending initial ping");
                if let Err(e) = dc3.send_text("ping".to_string()).await {
                    tracing::warn!("ping send error: {e}");
                }
            })
        }));

        dc.on_message(Box::new(|msg: DataChannelMessage| {
            let text = String::from_utf8_lossy(&msg.data).to_string();
            tracing::info!("Data channel message received: '{text}'");
            Box::pin(async {})
        }));

        Ok(dc)
    }

    /// Generate a WebRTC **offer** SDP and return it as a plain string.
    pub async fn create_offer(&self) -> Result<String> {
        let offer = self
            .connection
            .create_offer(None)
            .await
            .context("failed to create offer")?;

        self.connection
            .set_local_description(offer.clone())
            .await
            .context("failed to set local description")?;

        Ok(offer.sdp)
    }

    /// Generate a WebRTC **answer** SDP after applying the remote offer.
    pub async fn create_answer(&self, remote_offer_sdp: &str) -> Result<String> {
        let offer = RTCSessionDescription::offer(remote_offer_sdp.to_string())
            .context("invalid remote offer SDP")?;

        self.connection
            .set_remote_description(offer)
            .await
            .context("failed to set remote description")?;

        let answer = self
            .connection
            .create_answer(None)
            .await
            .context("failed to create answer")?;

        self.connection
            .set_local_description(answer.clone())
            .await
            .context("failed to set local answer")?;

        Ok(answer.sdp)
    }

    /// Apply the remote **answer** SDP (called on the host side after receiving
    /// the receiver's QR-Airgap response).
    pub async fn apply_answer(&self, remote_answer_sdp: &str) -> Result<()> {
        let answer = RTCSessionDescription::answer(remote_answer_sdp.to_string())
            .context("invalid remote answer SDP")?;

        self.connection
            .set_remote_description(answer)
            .await
            .context("failed to apply remote answer")
    }

    /// Add a remote ICE candidate (called when the peer's ICE candidates
    /// are received out-of-band, e.g. appended to the QR payload).
    pub async fn add_ice_candidate(&self, candidate_json: &str) -> Result<()> {
        let candidate: RTCIceCandidateInit =
            serde_json::from_str(candidate_json).context("invalid ICE candidate JSON")?;

        self.connection
            .add_ice_candidate(candidate)
            .await
            .context("failed to add ICE candidate")
    }

    /// Block until the peer connection reaches the `Connected` state or
    /// returns an error/failure state.
    pub async fn wait_for_connected(&self) -> Result<()> {
        let (tx, mut rx) = tokio::sync::mpsc::channel::<RTCPeerConnectionState>(1);
        self.connection
            .on_peer_connection_state_change(Box::new(move |state| {
                let tx = tx.clone();
                Box::pin(async move {
                    tracing::info!("PeerConnection state: {state}");
                    let _ = tx.send(state).await;
                })
            }));

        loop {
            match rx.recv().await {
                Some(RTCPeerConnectionState::Connected) => return Ok(()),
                Some(
                    RTCPeerConnectionState::Failed
                    | RTCPeerConnectionState::Disconnected
                    | RTCPeerConnectionState::Closed,
                ) => anyhow::bail!("PeerConnection reached terminal state"),
                _ => continue,
            }
        }
    }

    /// Close the peer connection and release all resources.
    pub async fn close(&self) -> Result<()> {
        self.connection
            .close()
            .await
            .context("failed to close PeerConnection")
    }
}

/// Helper: Build the SDP munging string that raises the video bitrate to
/// `TARGET_VIDEO_BITRATE_BPS`.  This is appended to the SDP before QR
/// encoding so the receiver knows to accept the higher bitrate.
pub fn munge_sdp_bitrate(sdp: &str) -> String {
    // Insert a `b=AS:<kbps>` line immediately after the `m=video` line.
    let kbps = TARGET_VIDEO_BITRATE_BPS / 1000;
    let mut result = String::with_capacity(sdp.len() + 32);
    for line in sdp.lines() {
        result.push_str(line);
        result.push('\n');
        if line.starts_with("m=video") {
            result.push_str(&format!("b=AS:{kbps}\n"));
        }
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn munge_sdp_inserts_bitrate() {
        let sdp = "v=0\nm=video 9 UDP/TLS/RTP/SAVPF 96\na=sendonly\n";
        let munged = munge_sdp_bitrate(sdp);
        assert!(munged.contains("b=AS:45000\n"), "munged:\n{munged}");
        // Original lines still present
        assert!(munged.contains("a=sendonly"));
    }

    #[test]
    fn munge_sdp_no_video_line_is_unchanged() {
        let sdp = "v=0\nm=audio 9 UDP/TLS/RTP/SAVPF 111\n";
        let munged = munge_sdp_bitrate(sdp);
        assert!(!munged.contains("b=AS:"), "should not insert b=AS for audio-only SDP");
    }
}
