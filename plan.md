🏗️ Project Profile: Rust4K-P2P

Goal: A decentralized, ultra-low latency voice & 4K 60Hz desktop streaming service.
1. Core Technical Stack

    Language: Rust (for memory safety and zero-copy performance).

    WebRTC Stack: webrtc-rs (P2P connectivity, STUN/ICE).

    Media Engine: GStreamer (via gstreamer-rs) to access GPU hardware encoders.

    Audio: CPAL or Oboe for low-latency Opus voice encoding.

2. High-Performance Features
Feature	Implementation	Benefit
Video Quality	3840x2160 @ 60fps	Native desktop clarity.
Encoding	AV1 or NVENC (H.264/H.265)	Offloads 4K processing to the GPU; saves CPU.
Transport	UDP (via STUN)	Bypasses NAT for direct peer-to-peer speed.
Bitrate	35 – 50 Mbps (Munged)	Prevents the "blurry" look typical of browser WebRTC.
3. The "QR-Airgap" Signaling Flow

Since we want to avoid a central server, we use Manual Signaling:

    Offer: Host generates a WebRTC SDP, compresses it with Gzip, and renders a QR Code.

    Scan: The Receiver scans the QR with their webcam. The string is decoded.

    Answer: Receiver generates a response QR. The Host scans it back.

    Handshake: The webrtc-rs stack performs a "UDP Hole Punch" to link the two IPs.

4. System Requirements

    Sender (Host): * High-speed Upload (>50 Mbps).

        Nvidia/AMD/Apple Silicon GPU for hardware encoding.

    Receiver (Client): * 4K Monitor for native scaling.

        Hardware decoding support (VP9/AV1/H.265).

    Network: * Network must allow UDP traffic (STUN verified).

5. Implementation Roadmap (The "Build" Order)

    Phase 1: Basic webrtc-rs "Data Channel" setup to test P2P ping.

    Phase 2: Integrate GStreamer to capture the screen and pipe it to a local video player.

    Phase 3: Build the QR Signaling Module (Base64 + Gzip + QR).

    Phase 4: Combine the two—send the 4K stream over the P2P link.
