# Transport & Latency Research for Macast `screen_mirror` Redesign

**Date:** 2026-10-02
**Scope:** External research only. No file inside `/Users/pavia/githome/Macast` was read, created, or modified. No code was changed.
**Method:** Primary sources — `chromium/openscreen`, `chromium/chromium`, `aiortc/aiortc`, `FFmpeg/FFmpeg`, `bluenviron/mediamtx`, `LizardByte/Sunshine`, `moonlight-stream/moonlight-qt`, `videolan/vlc`, `philippe44/AirConnect`, `greensteindesign/mirrorcast-macos`, W3C specs, IETF drafts, ffmpeg muxer source. Retrieved via `gh api ... -H "Accept: application/vnd.github.raw"`, `gh search code`, `WebFetch`, and `curl -sL` + HTML strip.

> Note: the `r.jina.ai` reader proxy suggested in the task returns empty bodies / HTTP 000 from this environment and was unusable; `WebFetch` was substituted, and GitHub raw content was read via `gh api`.

---

## 0. Executive summary — top 3 opportunities, ranked by (latency win) / (implementation risk)

### #1 — Add a `webrtc` output shape using **stock aiortc with zero-re-encode H.264 pass-through**
**Latency win:** largest available. LAN glass-to-glass <50 ms (getstream), measured WebRTC transport contribution ≈10 ms with a ~7 ms jitter buffer on a clean LAN (transitiverobotics infinite-mirror). Compare: DLNA fake-file ≈4 s (Macast) to 20–25 s (MirrorCast on a real Philips); fMP4+MSE bounded by segment/append cadence; Cast Streaming bounded by `targetDelay` (default 400 ms).
**Risk:** low-moderate. It is *stock, upstreamed, documented* aiortc behaviour — `RTCRtpSender._next_encoded_frame()` branches on `isinstance(data, Frame)`; if the track returns anything else it calls `self.__encoder.pack(data)`, and `H264Encoder.pack()` does NAL-split + RTP packetize with **no libx264 and no PyAV CodecContext**. Two production users rely on it (cyberwave-python, selkies). Signaling is exactly the pattern Macast already has: aiortc's own `examples/server` serves a page from an aiohttp server and answers `POST /offer` with `{sdp,type}` JSON, and `RTCConfiguration.iceServers` defaults to `None` (host candidates only, no STUN/TURN).
**Hard constraints found:** input must be **Annex-B** (AVCC yields zero RTP payloads and the peer tears down); aiortc advertises H.264 only as `profile-level-id=42001f`/`42e01f` with `packetization-mode=1`, so **ffmpeg must emit `-profile:v baseline -level 3.1`**; RTP payload MTU is 1300; PLI cannot conjure a keyframe in pure pass-through → use a short GOP.
**Bonus:** `cryptography` is one of aiortc's 7 pip deps, so adopting aiortc also fixes the `caststream` pure-Python AES bottleneck (≈1.3 MB/s → 4.5 Mbps cap) for free.
Sources: `aiortc/src/rtcrtpsender.py`, `aiortc/src/codecs/h264.py`, `aiortc/src/codecs/__init__.py`, `aiortc/src/rtcconfiguration.py`, `aiortc/pyproject.toml`, `aiortc/examples/server/{server.py,client.js}`, aiortc discussion #769, `cyberwave-os/cyberwave-python:cyberwave/sensor/camera_h264.py`, `selkies-project/selkies:src/selkies/webrtc/codecs/base.py`.

### #2 — Fix the existing `caststream` shape: `targetDelay` is a free 0–5000 ms knob, and the 12-frame in-flight window is ~3–6× too big
**Latency win:** potentially 400 ms → tens of ms, plus removal of the artificial frame-count burst cap.
**Risk:** low for the protocol-side changes (they are spec-legal and mirror openscreen), **high for verification** — nobody has run a hand-rolled sender against real Chromecast firmware in any public source I could find.
Evidence: `kMinTargetPlayoutDelay = milliseconds(0)`, `kMaxTargetPlayoutDelay = 5000`, `kDefaultTargetPlayoutDelay(400)`; `SenderImpl` only asserts `OSP_CHECK_GT(target_playout_delay_, milliseconds::zero())`. Chrome itself exposes `--cast-mirroring-target-playout-delay` / `streamingTargetPlayoutDelayMillis=`. The ANSWER's `max_delay` feeds **capture recommendations only** — it does not clamp the sender's target playout delay in openscreen. And `SenderImpl::GetMaxInFlightMediaDuration()` = `clamp(2*RTT, 66 ms, max(66 ms, target_playout_delay/3))`, i.e. **≤133 ms at the 400 ms default (≈4 frames at 30 fps)**, with `kMaxUnackedFrames = 120` being only a *FrameId-span* ambiguity limit, not a burst window.
Sources: `openscreen:cast/streaming/public/{constants.h,offer_messages.h,session_config.h,answer_messages.h,receiver_constraints.h,capture_recommendations.cc}`, `openscreen:cast/streaming/impl/sender_impl.cc`, `chromium:chrome/browser/media/router/media_router_feature.cc:192`, `chromium:chrome/browser/media/router/providers/cast/mirroring_activity.cc`.

### #3 — Stop treating the DLNA prefill as a receiver law, and stop trusting the proxy receiver's container ranking
**Latency win:** the ~4 s visible delay is a pure sender knob. The field-tested competitor pre-fills **~20 MiB ≈ 20–25 s** and documents it as "by design … Reduce `burstBytes` … if your TV tolerates it". AirConnect exposes `-l <[rtp][:http]>` latency in ms and `-g <-3|-1|0>` content-length mode as runtime options.
**Risk:** moderate — you can only tune it against real TVs, and Macast currently has no real-TV test data.
**Critical correction:** on a real 2016 Philips 43PFS5301, **MPEG-PS works and live MPEG-TS is refused** ("file not supported"), the opposite of Macast's mpv/Macast-receiver measurement (MPEG-TS 2.47 s vs MPEG-PS 5.86 s). The proxy receiver is not representative.
Sources: `greensteindesign/mirrorcast-macos:{Sources/HTTPStreamServer.swift,Sources/StreamHub.swift,Sources/Profiles.swift,docs/HOW-IT-WORKS.md}`, `philippe44/AirConnect:airupnp/src/airupnp.c`, `videolan/vlc:modules/stream_out/dlna/{profile_names.hpp,dlna.cpp}`.

---

## 1. Q2 — Latency budget per transport shape (comparison table)

LAN = same subnet, wired or good 5 GHz, screen capture (no camera/USB term).

| Shape | Transport | Realistic LAN glass-to-glass | Dominant term | Source strength |
|---|---|---|---|---|
| **WebRTC (aiortc / WHIP-WHEP)** | SRTP/UDP, host candidates | **<50 ms**; measured transport-only ≈**10 ms** + jitter buffer ≈**7 ms** local / 10 ms remote | encoder + display vsync | Strong: getstream.io protocol comparison; transitiverobotics measured breakdown (May 2026) |
| **MediaMTX WHEP** | WebRTC | **P50 180 ms / P95 240 ms** (LAN RK3568 loopback); real user report 300–500 ms | server + client pacer | Moderate: adaptnxt benchmark blog; mediamtx discussion #1691 |
| **Cast Streaming (`caststream`)** | Cast RTP + AES-128-CTR/UDP | **`targetDelay`**, default **400 ms**, spec floor **0 ms**, max 5000 ms; sender in-flight cap = `targetDelay/3`, floor 66 ms | receiver playout buffer | Strong for the *spec*; **zero** real-device measurements |
| **SRT** | UDP/TSBPD | **fixed** ≈ `RTT₀/2 + negotiated latency` (max of both ends) | your configured `latency` | Strong: draft-sharabayko-srt-00 §4.4–4.5, `libavformat/libsrt.c` |
| **LL-CMAF / LL-DASH (chunked)** | HTTP/1.1 chunked or HTTP/2 | ≈ **chunk duration** once mid-segment start is allowed (1 s chunks → ~3 s total in the Fraunhofer worked example) | segment/chunk duration + player target latency | Strong: Fraunhofer video-dev; W3C/ISO ATO signalling |
| **LL-HLS** | HTTP | **2–5 s** (200 ms parts, 1 s segments) | part duration + playlist reload | Moderate: 100ms.live, cloudinary |
| **fMP4 + MSE (`browser`, today)** | HTTP, ring buffer | = fragment duration + append cadence + live-edge seek; **spec-soft** (see §2) | `SourceBuffer.appendBuffer` cadence | Mixed: W3C media-source-2 says cadence is implementation-defined |
| **Progressive MPEG-TS over HTTP (`cast`, today)** | HTTP pull by Chromecast | **not measured anywhere public** — Google's live-receiver docs were unreachable both via WebFetch and curl | Cast receiver buffering | **Unverified** |
| **Classic HLS/DASH** | HTTP | 10–30 s (HLS 30–60 s) | segment count × duration | Strong, multiple |
| **RTSP** | RTP/TCP or UDP | **~2 s** (single anecdote, MediaMTX discussion #1691) | server + player buffering | Weak (n=1) |
| **DLNA "fake file" (`dlna`, today)** | HTTP + `Content-Length` | **= sender prefill.** Macast 2–8 MiB ≈ **4 s**; MirrorCast 20 MiB ≈ **20–25 s**; container adds ~3.4 s more (PS 5.86 s vs TS 2.47 s, Macast's own measurement) | prefill + TV's near-zero buffering | Strong: MirrorCast source + docs; AirConnect options |
| **DLNA plain live TS** | HTTP chunked | refused outright by the tested Philips ("file not supported") | — | Moderate: MirrorCast docs |
| **RIST** | UDP | no numbers found | — | **Unverified** |

Reference points for "how good is good enough": Apple/Hopp — ">100 ms in the UI disrupts smooth interactions", "ideal … below 100 ms, … significantly declines when latency exceeds 150 ms".

### The `browser` (fMP4+MSE) ceiling is spec-soft, not spec-hard
W3C `media-source-2`, Segment Parser Loop, verbatim:
> "If the [[input buffer]] contains one or more complete coded frames, then run the coded frame processing algorithm. **Note: The frequency at which the coded frame processing algorithm is run is implementation-specific. The coded frame processing algorithm MAY be called when the input buffer contains the complete media segment or it MAY be called multiple times as complete coded frames are added to the input buffer.**"

So per-frame fragments *can* give per-frame latency, but a browser MAY wait for the whole media segment. The ffmpeg lever exists: `movflags=+frag_every_frame` (plus `delay_moov`, `cmaf`, `default_base_moof`, `skip_sidx`, `separate_moof`), with `frag_duration` / `min_frag_duration` / `frag_custom` for coarser control (verified in `libavformat/movenc.c`, ~9697 lines; cut logic ~line 7532).

Plain-HTTP chunked transfer alone is **not** enough for LL-CMAF: the low-latency behaviour is signalled in the manifest (`@availabilityTimeComplete="false"` + `@availabilityTimeOffset` for DASH; parts + blocking playlist reload for LL-HLS), and MediaMTX defaults to `hlsVariant: lowLatency`, `hlsSegmentDuration: 1s`, `hlsPartDuration: 200ms`.

---

## 2. Q1 — Cast Streaming / WebRTC casting

### 2.1 What is app id `0F5096E8`?
**It is the official Cast Streaming audio+video receiver — the same app Chrome launches for tab/desktop mirroring.**

`chromium/openscreen:cast/common/public/cast_streaming_app_ids.h`:
```cpp
constexpr const char* GetCastStreamingAudioVideoAppId() { return "0F5096E8"; }
constexpr const char* GetCastStreamingAudioOnlyAppId()  { return "85CDB22F"; }
// AndroidMirroringAudioVideo "674A0243", AndroidMirroringAudioOnly "8E6C866D",
// AndroidAppStreamingAudioVideo "96084372", IosAppStreamingAudioVideo "BFD92C23"
```
`chromium/chromium` corroborates: `kCastStreamingAppId[] = "0F5096E8"`, `kMirroringAppUri[] = "cast:0F5096E8"`.

### 2.2 Which real senders use it?
Chromium itself (`mirroring_activity.cc`), and third-party/independent senders: `MSEndpointMgr/1PhoneMirror`, `tristanpenman/go-cast`, `dylanmckay/gcast` (Rust), `owntone/owntone-server` (`CAST_APP_ID_OLD`), `cretz/owncast-old`, `Aphrodine-wq/omacast` (Rust). **I found no public test report from any of these against real Chromecast hardware.**

### 2.3 The playout-delay and in-flight findings (the actionable core)

`cast/streaming/public/constants.h`: `kDefaultTargetPlayoutDelay(400)`, `kDefaultCastStreamingPort = 2344`, `kRtcpReportInterval(500)`, `kRequiredNetworkPacketSize = 256`, `kRtpVideoTimebase = 90000`, `kDefaultFrameRate = 30`, `kDefaultVideoMinBitRate = 300*1000`, `kDefaultVideoMaxBitRate = 10*1000*1000`, `kDefaultAudioMinBitRate = 32*1000`, `kDefaultAudioMaxBitRate = 256*1000`;
```cpp
enum class AudioCodec { kAac, kOpus, kNotSpecified };
enum class VideoCodec { kH264, kVp8, kHevc, kNotSpecified, kVp9, kAv1 };
enum class CastMode : uint8_t { kMirroring, kRemoting };
enum class DataTransportProtocol { kUnknown, kWebTransport };
// "This value is carefully choosen such that it fits in the 8-bits range for
//  frame IDs. It is also less than half of the full 8-bits range such that
//  logic can handle wrap around and compare two frame IDs meaningfully."
inline constexpr int kMaxUnackedFrames = 120;
```

`cast/streaming/public/offer_messages.h`: `kMinTargetPlayoutDelay = milliseconds(0)`, `kMaxTargetPlayoutDelay = 5000`, `kDefaultMaxFrameRate = 30`; `struct Stream { … std::chrono::milliseconds target_delay; std::array<uint8_t,16> aes_key; std::array<uint8_t,16> aes_iv_mask; … }`; `struct Offer { CastMode cast_mode = CastMode::kMirroring; std::vector<AudioStream> audio_streams; std::vector<VideoStream> video_streams; … }` — **one OFFER carries audio and video together, and the sender chooses `target_delay` with a 0 ms floor.**

`cast/streaming/impl/sender_impl.cc` (the 12-vs-120 resolution), verbatim:
```cpp
// The minimum amount of media the Sender keeps in-flight, regardless of the
// measured network round-trip time. This keeps the encoder pipeline flowing on
// low-latency networks (roughly two video frames at 30 FPS). See
// crbug.com/498035450.
constexpr Clock::duration kMinSenderInFlight = Clock::to_duration(milliseconds(66));

Clock::duration SenderImpl::GetMaxInFlightMediaDuration() const {
  if (config_.max_in_flight_media_duration) return config_.max_in_flight_media_duration.value();
  // … capped at a third of the playout delay window so that the majority of the
  // budget is reserved for the Receiver …
  const Clock::duration max_in_flight = std::max(
      kMinSenderInFlight, Clock::to_duration(target_playout_delay_) / 3);
  return std::clamp(round_trip_time_ * 2, kMinSenderInFlight, max_in_flight);
}
```
and in `EnqueueFrame()`:
```cpp
// Even if `num_frames_in_flight_` is less than kMaxUnackedFrames,
// it's the span of FrameIds that is restricted.
if ((frame.frame_id - checkpoint_frame_id_) > kMaxUnackedFrames) return REACHED_ID_SPAN_LIMIT;
if (GetInFlightMediaDuration(frame.rtp_timestamp) > GetMaxInFlightMediaDuration())
  return MAX_DURATION_IN_FLIGHT;
```
Also `OSP_CHECK_GT(target_playout_delay_, milliseconds::zero());`, `OSP_CHECK_NE(sender_ssrc, receiver_ssrc)`, kickstart interval = `max(target_playout_delay_ * kWaitFraction::num/den, round_trip_time_ * kLowerBoundRoundTrips)`.

**Conclusion: 120 is a FrameId-span ambiguity limit. The real burst bound is a *duration*: 66 ms floor (≈2 frames at 30 fps), cap `targetDelay/3`. At the 400 ms default that is ≤133 ms ≈ 4 frames. Macast's 12-frame window is ~3–6× more generous than openscreen's, and lowering `targetDelay` shrinks it automatically.**

`session_config.h` also exposes: `is_pli_enabled = false`, `allow_skip_to_keyframe = false` ("Optional optimization to skip incomplete/late frames on packet loss"), `sender_keyframe_cooldown`, `receiver_proactive_pli_interval`, and `std::optional<milliseconds> max_in_flight_media_duration` ("Optional override for the maximum in-flight media duration").

### 2.4 Does the receiver negotiate the delay down?
**No — not in openscreen.** `Answer.constraints.{audio,video}.max_delay` is `std::optional` and may be null; the only consumer is `capture_recommendations.cc::ApplyConstraints()`, which writes it into `recommendations->video.max_delay` (a capture-side hint). `sender_session.cc` builds `SessionConfig` from `config.target_playout_delay` / `stream.target_delay` (the OFFER values). No `SetTargetPlayoutDelay` symbol exists in the repo.
*Caveat:* that is openscreen's implementation of the receiver side. Real Chromecast firmware may clamp internally; unverifiable without hardware.

### 2.5 Is the RTP + AES-128-CTR payload format documented outside openscreen?
Only by independent transcription. `Aphrodine-wq/omacast:docs/protocol-notes.md` (Rust) corroborates **every** byte layout Macast uses: 12-byte RTP header (seq fresh on retransmits) + 7-byte Cast header (flags bit7 keyframe / bit6 referenced-id, frame-id low 8, packet-id u16, max-packet-id u16, referenced-frame-id low 8); AES-128-CTR over the whole access unit **before** packetization, nonce = zero block with frame-id BE at bytes 8..12 XOR `aesIvMask`; SR PT 200 / 28 bytes / every 500 ms and after the first frame ("Receivers do not render without this mapping"); Cast Feedback PT 206 FMT 15 with `"CAST"` magic (checkpoint frame-id, loss-count, playout-delay u16, loss fields {frame u8, packet u16, bitmask u8}, packet `0xFFFF` = whole frame, trailing `"CST2"` skipped); PLI PT 206 FMT 1 → shed deltas until next keyframe; kickstart after 250 ms idle; one UDP socket for both streams + RTCP; `rtpProfile:"cast"`, video PT 96 / audio PT 127, timeBase 1/90000 & 1/48000, receiver SSRC = sender SSRC + 1; "Some Cast firmware only accepts the control connection over IPv4". No public "Cast V2 Mirroring Control Protocol" spec URL exists (openscreen comments reference one).

Two independent transcriptions agreeing lowers the "we invented a format" risk materially.

### 2.6 Gotchas checklist
- **Audio is not optional in spirit but is optional in practice:** `Offer.audio_streams` may be empty; `85CDB22F` is an audio-only app, `0F5096E8` is A/V. Opus PT 127 @ 48 kHz. "No sound" in Macast is an omission, not a protocol limit.
- **H.264 profile:** openscreen's `VideoCodec` list includes `kH264` first-class; `capture_recommendations.h` says mirroring max 1920×1080, min 320×240, and "recommendations … are NOT maximum operational limits". No baseline-vs-high constraint is stated for Cast Streaming (unlike WHIP/aiortc).
- **RTCP is load-bearing:** Sender Report every 500 ms *and after the first frame*; without the RTP↔NTP mapping "receivers do not render".
- **Device auth is contradictory:** Macast's own notes say a proper `DeviceAuthMessage{AuthResponse{signature, client_auth_certificate}}` is required (as VLC does); omacast installs a no-op TLS verifier on 8009 and says "Chrome authenticates devices via a separate `DeviceAuthMessage` flow **we don't need for mirroring**". **Unresolved without hardware.**
- **IPv4-only control connections** on some firmware.
- **Pure-Python AES ≈1.3 MB/s caps the channel at 4.5 Mbps** — `cryptography` (already an aiortc dep) or `pycryptodome` removes this. Not benchmarked here.

---

## 3. Q3 — WebRTC without a signaling server, LAN-only

### 3.1 Can we serve a page from our own HTTP server and complete offer/answer over it?
**Yes — that is literally aiortc's reference example.** `aiortc/examples/server/server.py`:
```python
async def offer(request):
    params = await request.json()
    offer = RTCSessionDescription(sdp=params["sdp"], type=params["type"])
    pc = RTCPeerConnection()
    ...
    answer = await pc.createAnswer()
    await pc.setLocalDescription(answer)
    return web.Response(content_type="application/json", body=json.dumps(...))
```
`aiortc/examples/server/client.js`:
```js
if (document.getElementById('use-stun').checked) {
    config.iceServers = [{ urls: ['stun:stun.l.google.com:19302'] }];
}
pc = new RTCPeerConnection(config);
...
return fetch('/offer', { body: JSON.stringify({ sdp: offer.sdp, type: offer.type, ... }),
                         headers: {'Content-Type':'application/json'}, method: 'POST' });
```
STUN is behind an *unchecked-by-default checkbox*. `aiortc/src/rtcconfiguration.py`:
```python
@dataclass
class RTCConfiguration:
    iceServers: Optional[list[RTCIceServer]] = None
    "A list of :class:`RTCIceServer` objects to configure STUN / TURN servers."
    bundlePolicy: RTCBundlePolicy = RTCBundlePolicy.BALANCED
    alwaysNegotiateDataChannels: bool = False
```
Default `None` → host candidates only → fine on a LAN. Independent corroboration: `pollen-robotics/microduck:docs/design/remote-webrtc.md` — "aiortc's STUN client works where its TURN client does not."

### 3.2 Pre-encoded H.264 pass-through (the question that matters)
`aiortc/src/rtcrtpsender.py`, lines 294–330:
```python
async def _next_encoded_frame(self, codec) -> Optional[RTCEncodedFrame]:
    data = await self.__track.recv()
    ...
    if isinstance(data, Frame):
        payloads, timestamp = await self.__loop.run_in_executor(
            None, self.__encoder.encode, data, force_keyframe)
    else:
        # Pack the pre-encoded data.
        payloads, timestamp = self.__encoder.pack(data)
```
`aiortc/src/codecs/h264.py`:
```python
def pack(self, packet: Packet) -> tuple[list[bytes], int]:
    assert isinstance(packet, av.Packet)
    packages = self._split_bitstream(bytes(packet))
    timestamp = convert_timebase(packet.pts, packet.time_base, VIDEO_TIME_BASE)
    return self._packetize(packages), timestamp
```
`pack()` touches **no** CodecContext — pure NAL split (`_split_bitstream` on `0x000001`) + FU-A/STAP-A packetization at `PACKET_MAX = 1300`. The `DEFAULT_BITRATE = 1000000 / MIN 500000 / MAX 3000000` clamp only applies to `encode()`'s rate control, **not** to `pack()`. Origin: aiortc discussion #769 "Send pre-encoded data from GStreamer via aiortc".

`aiortc/src/codecs/__init__.py` advertises VP8, then H264 twice with `profile-level-id` **only** `"42001f"`/`"42e01f"`, `"level-asymmetry-allowed":"1"`, `"packetization-mode":"1"`, clockRate 90000, `rtcpFeedback=[nack, nack pli, goog-remb]`; extensions id 1 `sdes:mid`, id 3 `abs-send-time`. → **`-profile:v baseline -level 3.1` is mandatory for the ffmpeg side.**

Production pass-through users:
- `cyberwave-os/cyberwave-python:cyberwave/sensor/camera_h264.py` — `H264PacketVideoTrack(BaseVideoTrack)`, `get_packet() -> (bytes, is_keyframe) | None` ("Must be fast and non-blocking"), `framing ∈ auto|annexb|avcc`, `to_annex_b()`, `_ANNEXB_START = b"\x00\x00\x00\x01"`, `DEFAULT_FPS = 15`. Documents: AVCC input → zero RTP payloads → SFU tears down (`connectionState=closed`); detection must be AVCC-first (a 256–511-byte first NAL encodes as `00 00 01 xx`); cached-keyframe replay on PLI causes visible flicker → **shorter keyframe interval is the right lever**; encoded access units are **not idempotent** (a repeat/drop breaks the P-frame chain until the next keyframe).
- `selkies-project/selkies:src/selkies/webrtc/codecs/base.py` — vendored aiortc codec layer with `class EncodedPacket` (`data,pts,dts,time_base,keyframe,timing,dependency`) and `Encoder.pack(...) -> tuple[list[bytes], int, bool]`; zero-copy via `memoryview(packet.data)` "both cuts latency and frees the GIL". README: "Moonlight, Google Stadia, or GeForce NOW in noVNC form factor … at least 60 frames per second on Full HD resolution".

### 3.3 Hand-rolling DTLS + SRTP + SCTP in Python
**Not researched in depth — aiortc makes it moot.** What is verifiable: aiortc's dependency set is `aioice`, `av`, `cryptography`, `google-crc32c`, `pyee`, `pylibsrtp`, `pyopenssl` — i.e. DTLS comes from pyopenssl, SRTP from `pylibsrtp` (a libsrtp binding), ICE from `aioice`. So the primitives are all C-backed pip wheels; writing them yourself would mean re-implementing DTLS handshake, SRTP key derivation, and (optionally) SCTP. **Effort estimate not sourced.** For Macast this also means 7 new root import names for the Part 30 import check, plus `requirements/*.txt`, `scripts/setup_py2app.py` (`includes`/`packages`), and the 3 PyInstaller `--hidden-import=` lists in `.github/workflows/build.yml`. `av` and `cryptography` and `pyopenssl` are binary wheels (PyAV bundles FFmpeg libs) — packaging weight is real but not unusual.

### 3.4 Alternative: **FFmpeg ≥ 8.0 has a native `-f whip` muxer**
Verified in `FFmpeg/FFmpeg:libavformat/whip.c` (2218 lines on master, 70,122 B on branch `n8.0`; **404 on `n7.1`**; first commit 2025-05-16; `allformats.c:530 extern const FFOutputFormat ff_whip_muxer;`):
```c
/* WebRTC-HTTP ingestion protocol (WHIP) muxer ...
 * Note that only baseline and constrained baseline profiles of h264 are supported.
 * TODO: FIXME: There is an issue with the timestamp of OPUS audio ... causing
 *  Chrome to play the audio stream with noise. */
{ "handshake_timeout", "... ICE and DTLS handshake.", {.i64 = 5000} },
{ "pkt_size", "The maximum size, in bytes, of RTP packets that send out", {.i64 = 1200} },
{ "dtls_active", ... {.i64 = WHIP_DTLS_ACTIVE} },
{ "rtp_history", "The number of RTP history items to store", ... },
{ "authorization", "The optional Bearer token for WHIP Authorization", ... },
.p.audio_codec = AV_CODEC_ID_OPUS, .p.video_codec = AV_CODEC_ID_H264,
.p.flags = AVFMT_GLOBALHEADER | AVFMT_NOFILE | AVFMT_EXPERIMENTAL,
```
It auto-inserts `ff_stream_add_bitstream_filter(st, "h264_mp4toannexb", NULL)` for AVCC input, and uses an internal `dtls` protocol. Caveats: `AVFMT_EXPERIMENTAL`, `FF_OFMT_FLAG_ONLY_DEFAULT_CODECS`, H.264 baseline/constrained-baseline + Opus only, and the flagged Opus timestamp bug. `RELEASE` says `8.0.git`; the Changelog has **no** `whip|webrtc` entry, so "FFmpeg 8.0+" is the safe claim.

This gives a second WebRTC route with **no Python SRTP at all** on the sender side — but it still needs a WHIP/WHEP endpoint (MediaMTX, or a Python one).

---

## 4. Q4 — MediaMTX / ready-made media servers

`bluenviron/mediamtx`: **MIT**, 20,307 stars, pushed 2026-09-30. "Ready-to-use Media-over-QUIC / SRT / WebRTC / RTSP / RTMP / LL-HLS / MPEG-TS / RTP live media server and media proxy". Zero-dependency single executable for Linux/Windows/macOS. Publish via MoQ, SRT, WebRTC, RTSP, RTMP, HLS, MPEG-TS, RTP (explicitly lists FFmpeg/GStreamer/OBS/Python/Go/Unity/browsers/RPi); read via MoQ, SRT, WebRTC, RTSP, RTMP, HLS; automatic protocol conversion; hot reload; Control API; Prometheus; hooks. Codecs: H264, AV1, VP9, VP8, H265, Opus, G722, G711.

**Binary size (release v1.21.1 assets):** darwin_amd64 28,691,323 B; darwin_arm64 26,924,039; linux_amd64 27,453,737; linux_arm64 29,679,470; linux_armv6 29,773,461; linux_armv7 29,756,196; windows_amd64.zip 27,753,692. → **~27–30 MB compressed per platform.** Realistic to bundle in a desktop app (comparable to shipping an ffmpeg binary), though it adds a Go binary + a license/attribution surface and a second listening port set.

**Defaults relevant to latency** (from the shipped `mediamtx.yml`, 889 lines): `udpMaxPayloadSize: 1452`, `api: false`, `metrics: false`, `hlsAddress: :8888`, `hlsAllowOrigins: ["*"]`, `hlsAlwaysRemux: false`, **`hlsVariant: lowLatency`**, `hlsSegmentCount: 7`, **`hlsSegmentDuration: 1s`**, **`hlsPartDuration: 200ms`**, `hlsSegmentMaxSize: 50M`, `webrtcAddress: :8889`, **`webrtcEncryption: false`**, `webrtcLocalUDPAddress: :8189`, `webrtcICEServers2: []`, `srt: true`, `srtAddress: :8890`, `moq: true`.

**What it would replace, per Macast output shape:**
| Macast shape | MediaMTX equivalent | Replaces? |
|---|---|---|
| `cast` (TS over our HTTP) | none | **No** |
| `caststream` | none | **No** |
| `dlna` | **no DLNA output exists** | **No** |
| `browser` (fMP4+MSE) | WHEP WebRTC (`:8889`) or LL-HLS (`:8888`, 1 s/200 ms parts) | **Yes, and much better** |
| (new) | SRT `:8890`, RTSP, RTMP, MoQ | net-new outputs |

**Reported latency:** WHEP LAN P50 **180 ms** / P95 240 ms (adaptnxt edge-AI loopback, RK3568); a real user (discussion #1691, OBS RTMP → MediaMTX → WebRTC on the same LAN) reports **300–500 ms** WebRTC and **~2 s** RTSP. There is **no `srtLatency` config key** in `mediamtx.yml`, so MediaMTX's SRT latency behaviour is unconfirmed.

**Alternative:** GStreamer `webrtcsink` also "takes pre-encoded H.264 on its sink pad" (microduck design doc) — but it is a much heavier dependency than aiortc for a Python desktop app.

---

## 5. Q5 — DLNA/UPnP live streaming latency

### 5.1 Is ~4 s prefill inherent? **No — it is a sender choice, and Macast is already aggressive.**
`greensteindesign/mirrorcast-macos` (Swift, macOS→DLNA-TV live mirroring, reverse-engineered against a real **Philips 43PFS5301, 2016, non-Android**; pushed 2026-07-27):
- `Sources/StreamHub.swift`: `chunkSize = 64*1024`, `ringChunks = 768` (≈48 MiB history), **`burstBytes = 320 * chunkSize` (≈20 MiB head start)**. Comment: "Why byte-addressed? Picky TV players reconnect mid-stream and ask for a specific byte offset. Serving them *anything else* (e.g. the newest data) makes playback jump forward and the audio crackle."
- `docs/HOW-IT-WORKS.md` obstacle 6: "The TV barely buffers … pre-fills ~20 MiB (≈20 s at 8 Mbit/s) *before* handing the URL to the TV … **This is the source of the visible latency**."
- Known limits: "**Latency ≈ 20–25 s** by design (see obstacle 6). Fine for video, useless for gaming. **Reduce `burstBytes` in `StreamHub.swift` if your TV tolerates it.**"
- `Sources/HTTPStreamServer.swift`: `static let fakeSize = 1_900_000_000  // < 2^31`, `static let fakeDuration = "0:36:00.000"`, headers `Content-Length`, `Accept-Ranges: bytes`, `transferMode.dlna.org: Streaming`, `contentFeatures.dlna.org: http-get:*:<mime>:<dlnaFlags>`, `Connection: close`; "Bounded probe … if let e = end, e - start + 1 <= 4 * 1024 * 1024".

`philippe44/AirConnect:airupnp/src/airupnp.c` (mature live-UPnP bridge) makes latency and length-mode **explicit runtime knobs**:
```
"  -g <-3|-1|0>           HTTP content-length mode (-3:chunked, -1:none, 0:fixed)\n"
"  -l <[rtp][:http][:f]>  RTP and HTTP latency (ms), ':f' forces silence fill\n"
"  -S <broadcast|track|radio>  how a Sonos is told to present the stream (default broadcast)\n"
```
(Defaults for `-l` were not found in `airupnp.c` or `common/squeezelite.h` — **unverified**.)

### 5.2 `transferMode.dlna.org` and `contentFeatures.dlna.org`
Authoritative bit layout, `videolan/vlc:modules/stream_out/dlna/profile_names.hpp` lines 79–100:
```
 *     Example: (1 << 24) | (1 << 22) | (1 << 21) | (1 << 20)
 *       DLNA.ORG_FLAGS=01700000[000000000000000000000000] // [] show padding
enum dlna_org_flags_t {
  DLNA_ORG_FLAG_SENDER_PACED               = (1 << 31),
  DLNA_ORG_FLAG_TIME_BASED_SEEK            = (1 << 30),
  DLNA_ORG_FLAG_BYTE_BASED_SEEK            = (1 << 29),
  DLNA_ORG_FLAG_PLAY_CONTAINER             = (1 << 28),
  DLNA_ORG_FLAG_S0_INCREASE                = (1 << 27),
  DLNA_ORG_FLAG_SN_INCREASE                = (1 << 26),
  DLNA_ORG_FLAG_RTSP_PAUSE                 = (1 << 25),
  DLNA_ORG_FLAG_STREAMING_TRANSFER_MODE    = (1 << 24),
  DLNA_ORG_FLAG_INTERACTIVE_TRANSFERT_MODE = (1 << 23),
  DLNA_ORG_FLAG_BACKGROUND_TRANSFERT_MODE  = (1 << 22),
  DLNA_ORG_FLAG_CONNECTION_STALL           = (1 << 21),
  DLNA_ORG_FLAG_DLNA_V15                   = (1 << 20),
};
```
`videolan/vlc:modules/stream_out/dlna/dlna.cpp:108` uses `STREAMING | BACKGROUND | CONNECTION_STALL | DLNA_V15` → `01700000…`. MirrorCast uses `01500000…` = STREAMING | BACKGROUND | DLNA_V15. AirConnect uses `0d500000…` = S0_INCREASE | SN_INCREASE | STREAMING | BACKGROUND | DLNA_V15. **All three set the STREAMING bit (1<<24).** The `CONNECTION_STALL` bit (1<<21) is the one that tells the renderer it may stall waiting for data — VLC sets it, MirrorCast does not; worth an experiment given Macast's blocking-until-produced behaviour.

### 5.3 Does `DLNA.ORG_OP=01` (byte-seek) hurt live streams?
**Evidence says no — both values work in the field:**
- MirrorCast: `dlnaPN + "DLNA.ORG_OP=01;DLNA.ORG_CI=0;DLNA.ORG_FLAGS=01500000000000000000000000000000"` (needs byte-seek for its resume design).
- AirConnect: `DLNA.ORG_OP=00` for all its live audio profiles (`http-get:*:audio/L16;rate=44100;channels=2:DLNA.ORG_PN=LPCM;DLNA.ORG_OP=00;DLNA.ORG_CI=0;DLNA.ORG_FLAGS=0d500000…`).
- VLC: `DLNA_ORG_OPERATION_RANGE`.
The material bit is STREAMING (1<<24), not `OP`. AirConnect's compatibility lever: it returns `contentFeatures.dlna.org` **only when the renderer sends `getcontentFeatures.dlna.org`**, and always sends `transferMode.dlna.org: Streaming`:
```c
if (kd_lookup(headers, "getcontentFeatures.dlna.org") &&
    (p = strcasestr(Device->ProtocolInfo, "DLNA.ORG")) != NULL) {
    kd_add(response, "contentFeatures.dlna.org", p);
}
kd_add(response, "transferMode.dlna.org", "Streaming");
```

### 5.4 Concrete firmware bugs to code around (MirrorCast, real Philips)
1. "The TV lies about its capabilities … Push a live MPEG-TS stream and the set answers 'file not supported'. What works is **MPEG-PS**." ← **contradicts Macast's proxy-receiver measurement.**
2. "Live streams are refused; files are accepted … Chunked transfer without a length is rejected outright."
3. **The 2 GiB cliff:** a ~3.9 GB advertised size produced `Range: bytes=0-18446744072564584319` = `2^64 − 1_899_999_999` — an unsigned wrap of a *signed 32-bit underflow*. Keeping `Content-Length` < 2³¹ (1.9 GB) makes it request `bytes=0-1899999999` and play.
4. "Bounded probes must be answered exactly … any bounded range ≤ 4 MiB with *exactly* that many bytes, padded with an MPEG padding packet (`0x000001BE`)."
5. "Reconnects must resume byte-exactly … a request for bytes that do not exist yet simply **blocks until the encoder has produced them**."
6. Prefill = the visible latency (see 5.1).
7. "A quiet desktop stalls the audio … `-max_interleave_delta 500000 -flush_packets 1` … and a constant-bitrate video track (`-minrate = -maxrate`) gives the TV the steady data rate its buffer model expects." ← directly parallels Sunshine's `minimum_fps_target` duplicate-frame padding (§6).

Default profile (`ps`): `-vf scale=720:576,setdar=16/9,format=yuv420p -r 25 -c:v mpeg2video -b:v 4500k -minrate 4500k -maxrate 4500k -bufsize 1600k -g 15 -c:a ac3 -b:a 192k -ar 48000 -ac 2 -max_interleave_delta 500000 -flush_packets 1 -muxrate 8000000 -f vob`, mime `video/mpeg`, `DLNA.ORG_PN=MPEG_PS_PAL;`. Also `m2ts` (`-mpegts_m2ts_mode 1 -f mpegts`, mime `video/vnd.dlna.mpeg-tts`, `DLNA.ORG_PN=MPEG_TS_SD_EU_T;`), `mkv-h264` and `m2ts-h264` (`-profile:v high -level 4.0 -preset ultrafast -tune zerolatency -b:v 3500k -maxrate 4M -bufsize 6M -g 50`). DIDL-Lite: `<res protocolInfo="http-get:*:<mime>:<flags>" size="<size>" duration="<duration>">`. Sequence: `Stop` → `Thread.sleep(1.0)` → `SetAVTransportURI` → `Play <Speed>1</Speed>`. UPnP error 705 → unplug the TV from **mains** for 15 s (standby is insufficient on Philips quick-start sets).

**Not examined:** `hzeller/gmrender-resurrect` (939 stars) source; BubbleUPnP.

---

## 6. Q6 — Sunshine/Moonlight pipeline, and what transfers

**Verified from source (`LizardByte/Sunshine`, C++, 41,737 stars, GPL-3.0):**

- **FEC is Reed-Solomon per frame, and it is budgeted out of the video bitrate.** `src/rtsp.cpp`: `if (config::stream.fec_percentage <= 80) { configuredBitrateKbps /= 100.f / (100 - config::stream.fec_percentage); }`. `docs/configuration.md`: `fec_percentage` default **20**, range 1–255.
- `src/stream.cpp` (packetization, ~line 1598):
  ```cpp
  auto fecPercentage = config::stream.fec_percentage;
  auto blocksize = session->config.packetsize + MAX_RTP_HEADER_SIZE;
  // There are 2 bits for FEC block count for a maximum of 4 FEC blocks
  constexpr auto MAX_FEC_BLOCKS = 4;
  // D = 255 - P ; P = D * F  =>  D = (255 * 100) / (100 + F)
  auto max_data_shards_per_fec_block = (DATA_SHARDS_MAX * 100) / (100 + fecPercentage);
  ... if (fec_blocks_needed > MAX_FEC_BLOCKS) { "Skipping FEC for abnormally large encoded frame"; fecPercentage = 0; }
  // "If we exceed the 10-bit FEC packet index (… our frame exceeded 4096 packets), the frame will be unrecoverable."
  auto shards = fec::encode(current_payload, blocksize, fecPercentage,
                            session->config.minRequiredFecPackets,
                            session->video.cipher ? sizeof(video_packet_enc_prefix_t) : 0);
  ```
  SOF/EOF flags on the first/last packet; a `frame_header.frame_processing_latency` field is filled from `steady_clock::now() - *packet->frame_timestamp` and logged.
- **Loss recovery prefers reference-frame invalidation over IDR.** `src/video.cpp`: `request_idr_frame()` sets `force_idr = true`; `invalidate_ref_frames(int64_t first, int64_t last)` calls `device->nvenc->invalidate_ref_frames(...)` and **only falls back to `force_idr = true` if that fails**. Encoder presets pin `{"forced-idr", 1}`, `{"gops_per_idr", 1}`, `{"idr_interval", INT_MAX}`, `{"header_insertion_mode", "idr"}`. Events flow through a mailbox (`mail::idr`, `mail::invalidate_ref_frames`).
- **Rate control:** `qp` default **28** — "Some devices don't support Constant Bit Rate. For those devices, QP is used instead."
- **Static-screen padding:** `minimum_fps_target` default **0** — "Sunshine tries to save bandwidth when content on screen is static or a low framerate. Because many clients expect a constant stream of video frames, a certain amount of duplicate frames are sent when this happens." (Same problem as MirrorCast obstacle 7.)
- **Capture:** `capture = kms` on Linux; WGC/`gfxcapture` on Windows; VideoToolbox on macOS. Encoders: NVENC / AMF / VideoToolbox / Vulkan.
- **Session control is RTSP** (`src/rtsp.cpp`), not WebRTC signalling.

**Client side (`moonlight-stream/moonlight-qt`): there is no video jitter buffer — a *pacer* with drop-on-late.** `app/streaming/video/ffmpeg.cpp:945` prints its own latency breakdown:
```
"Frames dropped by your network connection: %.2f%%\n"      // networkDroppedFrames / totalFrames
"Frames dropped due to network jitter: %.2f%%\n"           // pacerDroppedFrames / decodedFrames
"Average network latency: %u ms (variance: %u ms)\n"
"Average decoding time: %.2f ms\n"                         // totalDecodeTimeUs
"Average frame queue delay: %.2f ms\n"                     // totalPacerTimeUs
"Average rendering time (including monitor V-sync latency): %.2f ms\n"
```
Decoder probing uses `avcodec_receive_frame_flags(ctx, frame, AV_CODEC_RECEIVE_FRAME_FLAG_SYNCHRONOUS)`. Audio *does* buffer ("The buffering helps avoid audio underruns due to network jitter" — `sdlaud.cpp`); video does not.

**What transfers to a TV-casting app and what does not:**
| Sunshine/Moonlight technique | Transfers? |
|---|---|
| FEC (RS, 20%) instead of retransmit round-trips | Only to a client you control. Cast Streaming uses NACK + retransmit + checkpoint; browsers use WebRTC NACK/PLI. |
| `invalidate_ref_frames` instead of IDR on loss | **No** — requires encoder cooperation (NVENC). ffmpeg+libx264 has no equivalent mid-stream API. |
| No video jitter buffer / drop-late pacer | **No** — the receiver is a TV or a browser. But it explains *why* they hit single-digit-to-20 ms: they refuse to buffer. |
| Duplicate-frame padding for static screens | **Yes** — directly applicable to the DLNA/avfoundation quiet-desktop stall. |
| `qp`-based rate control for non-CBR decoders | **Yes** for the DLNA/MPEG-PS profile. |
| Hardware encoders (NVENC/AMF/VideoToolbox) | **Yes** — Macast should prefer `h264_videotoolbox` / `h264_nvenc` / `h264_qsv` over libx264 where available; that is the single biggest capture-side latency term left. |
| Controlling both ends | **No.** This is the fundamental limit: Sunshine's numbers are unreachable when the receiver is a Chromecast or an old TV. |

**The "single-digit-to-20 ms glass-to-glass" claim in the task prompt was not corroborated.** Closest public data points: a discussion reply of "40 ms lag with 1080p@60fps using steam"; "100–200 ms" from another user; and transitiverobotics' measurement that WebRTC transport itself adds ~10 ms.

---

## 7. Could NOT be verified

**Cast Streaming (Q1)**
- **No real-Chromecast test report from any hand-rolled sender** (omacast, 1PhoneMirror, go-cast, gcast) — so the probability that Macast's `caststream` is accepted by real firmware is unknown.
- The **device-auth contradiction is unresolved**: VLC-style `DeviceAuthMessage{AuthResponse{signature, client_auth_certificate}}` (Macast's notes) vs omacast's no-op TLS verifier + "we don't need it for mirroring".
- Whether the RTP+AES-CTR format is documented anywhere besides openscreen and independent transcriptions. No public "Cast V2 Mirroring Control Protocol" spec URL was found.
- Whether real receiver firmware honours `targetDelay` < 400 ms or clamps it (openscreen does *not* clamp, but openscreen ≠ Chromecast).
- Actual measured Cast Streaming latency on a LAN.

**Transport latency table (Q2)**
- **Progressive MPEG-TS over HTTP pulled by a real Chromecast** — no public number. `developers.google.com/cast/docs/web_receiver/live` failed via WebFetch *and* `curl -sL` (empty body).
- **RIST** — no latency numbers found at all.
- Cost of `SourceBuffer.appendBuffer` + `video.currentTime` live-edge chasing in a real browser. Chromium issue 41161663 was unreachable ("fetch failed").
- LL-HLS blocking-playlist-reload specifics.
- RTSP ~2 s is a **single anecdote** (mediamtx discussion #1691).
- Wowza's low-latency-CMAF article returned only nav/CSS; Fraunhofer was substituted.
- gethopp's four tuning changes ("Quantizer and rate control", "Buffer size", "Combine all", "Set screencast mode in WebRTC") — headings retrieved, **numeric content not reached**.
- Chromium's auto-throttled-screen-capture design doc yielded only two facts (400 ms default; budget covers "capture, encoding, transmission to the receiver, decoding, and presentation").

**WebRTC (Q3)**
- Difficulty of hand-rolling DTLS+SRTP+SCTP in Python from scratch — never quantified (aiortc makes it moot, but the question was asked).
- aiortc's CPU usage and bitrate ceiling for screen sharing — searches returned only generic pages. The 3 Mbps clamp is *rate-control only* and does not apply to `pack()`, but no measured pass-through throughput figure was found.
- Whether `webrtcsink`-style pass-through has been benchmarked at 1080p60 from Python specifically (selkies claims 60 fps FHD but is a vendored fork, not stock aiortc).

**MediaMTX (Q4)**
- **No `srtLatency` config key exists**, so MediaMTX's SRT latency behaviour is unconfirmed.
- No official per-output latency figures from bluenviron; the 180/240 ms P50/P95 come from a third-party benchmark blog (adaptnxt), and 300–500 ms from one user discussion.
- Whether `webrtcEncryption: false` is honoured by all browsers (Chrome requires a secure context but not necessarily DTLS-SRTP on localhost/LAN) — not tested.

**DLNA (Q5)**
- **AirConnect's default `-l` RTP/HTTP latency values** — grep of `airupnp.c` and `common/squeezelite.h` found nothing.
- `hzeller/gmrender-resurrect` source not read; **BubbleUPnP not examined**.
- Whether any real TV benefits from the `CONNECTION_STALL` bit (1<<21).
- The exact minimum prefill a given TV tolerates — MirrorCast explicitly frames `burstBytes` as "reduce it if your TV tolerates it", i.e. per-device empirical.
- No independent confirmation of MirrorCast's Philips findings on other TV brands/years.

**Sunshine/Moonlight (Q6)**
- Moonlight's client-side frame pacer implementation (`totalPacerTimeUs` source) and its drop-late threshold were not read.
- `src/network.cpp` contains no FEC logic (it is address-family/socket plumbing); the FEC path is entirely in `src/stream.cpp`.
- No corroborated single-digit-to-20 ms glass-to-glass figure from any primary source.

**Method/tooling failures (affect coverage, not conclusions)**
- `https://r.jina.ai/<url>` returns empty / HTTP 000 in this environment.
- `https://www.phoronix.com/news/FFmpeg-Lands-WHIP-Muxer` → 403.
- `https://issues.chromium.org/41161663` → "fetch failed".
- `raw.githubusercontent.com` returned 0 bytes for `movenc.c`; `gh api ... -H "Accept: application/vnd.github.raw"` worked.
- `gh search issues --repo bluenviron/mediamtx "latency webrtc"` → `[]`; had to read discussion #1691 directly.
- WebSearch rejects queries >100 characters.
- FFmpeg version attribution for `-f whip` is indirect: `RELEASE` = `8.0.git`, Changelog has no whip entry; established via branch existence (`n8.0` yes, `n7.1` 404), first-commit date 2025-05-16, and HN/Phoronix dates (2025-06-04).

---

## 8. Appendix — packaging cost of the recommended path

If `aiortc` is adopted for a `webrtc` shape, Macast's constraints (from `AGENTS.md`) require:
1. **Part 30 import check:** 7 new root import names — `aiortc`, `aioice`, `av`, `cryptography`, `google_crc32c` (module name for `google-crc32c`), `pyee`, `pylibsrtp`, `OpenSSL` (module name for `pyopenssl`). All must be added to the allowed set (they are all top-level modules provided by `requirements/*.txt`, so adding them there satisfies the rule).
2. `requirements/*.txt` — add `aiortc>=…`.
3. `scripts/setup_py2app.py` — add to `includes`/`packages`.
4. `.github/workflows/build.yml` — add `--hidden-import=` for each of the 7–8 names to **all 3** PyInstaller jobs.
5. Binary-wheel weight: `av` (PyAV, bundles FFmpeg libs), `cryptography`, `pyopenssl`, `pylibsrtp`, `google-crc32c` are all platform wheels — bundle size grows materially.

**ffmpeg-side changes required for pass-through:** `-profile:v baseline -level 3.1` (mandatory for aiortc's `42001f`/`42e01f`), Annex-B output (`-bsf:v h264_mp4toannexb` if coming from an MP4/fMP4 path; `-f mpegts`/raw pipe output is already Annex-B), short GOP (PLI cannot force a keyframe in pure pass-through).

**Cheapest first experiment:** keep the existing ffmpeg pipeline, add `-profile:v baseline -level 3.1`, and pipe Annex-B access units into an `aiortc` `MediaStreamTrack` whose `recv()` returns `av.Packet`s, served from the HTTP server Macast already runs, with the aiortc `examples/server/client.js` page as the receiver. No STUN, no TURN, no new ports.
