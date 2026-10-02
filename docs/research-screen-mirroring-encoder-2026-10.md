# Macast `screen_mirror` — latency / efficiency research

Research only. No files written or modified inside `/Users/pavia/githome/Macast`.
Host: Mac14,9 (Apple M2 Pro, 10 cores), macOS with ffmpeg 9.0.2, Python 3.14.7 / OpenSSL 3.6.4.
Everything marked **[measured]** was run on this machine in this session or the previous one.
Everything marked **[unverified]** could not be reached from this host — see §7.

---

## 0. Executive summary

| # | Opportunity | Win | Risk | Verdict |
|---|---|---|---|---|
| 1 | CommonCrypto `CCCryptorReset`+`CCCryptorUpdate` via ctypes replaces the pure-Python AES-128-CTR | **~7,900× crypto throughput**; removes the `CAST_STREAM_MAX_BITRATE = 4500000` ceiling entirely (protocol default max is **10 Mbps**) | Very low — zero new deps, byte-exact vs LibreSSL, one `dlopen` of a dylib that exists on every macOS | **Do first** |
| 2 | Make the Cast OFFER multi-codec (H.264 + HEVC, hardware-first → software-fallback) and relax `parse_answer`'s `if 0 not in indexes: raise` | Higher quality/bitrate on receivers that have HEVC; hardware encode off the CPU | Low — the protocol already specifies exactly this negotiation and Chrome implements it verbatim | **Do second** |
| 3 | Re-examine the hardware/software encoder choice with the measured numbers: `libx264 ultrafast + zerolatency` is **~190 ms lower latency** than `h264_videotoolbox` and holds 5.6–17.9× realtime at every resolution up to 4K | ~190 ms glass-to-glass | Low — pure flag change | **Do third** |
| 4 | ScreenCaptureKit via `pyobjc-framework-ScreenCaptureKit` (~10 KB wheel): native system audio (**retires BlackHole + the CoreAudio aggregate device**), `SCFrameStatusIdle`, `SCStreamFrameInfoDirtyRects` | Removes the single worst UX requirement; enables change-driven VFR | Medium — new pyobjc subpackage (same family as the already-declared `pyobjc-framework-Cocoa`), new capture pipeline | Strong, but larger |
| 5 | `mpdecimate` + `-fps_mode vfr` for change-driven frame rate | Big CPU/bandwidth win on static desktops | Medium — Cast path already tolerant (wall-clock RTP timestamps); browser-MSE / MPEG-TS paths need care | Worth prototyping |

---

## 1. Q1 — Hardware-speed AES-128-CTR with no new pip dependency

### 1.1 macOS: CommonCrypto via ctypes — **[measured, works]**

`/usr/lib/system/libcommonCrypto.dylib` loads with `ctypes.CDLL` and zero new dependencies.
It is in the **dyld shared cache**, so `find /usr/lib -iname "*commonCrypto*"` returns nothing even
though it loads fine — do not use filesystem probing as the availability test.

Working signature set (verified this session):

```python
lib = ctypes.CDLL('/usr/lib/system/libcommonCrypto.dylib')
kCCEncrypt = 0; kCCModeCTR = 4; kCCAlgorithmAES = 0; ccNoPadding = 0

lib.CCCryptorCreateWithMode.argtypes = [ctypes.c_uint]*4 + [ctypes.c_void_p]*2 + \
    [ctypes.c_size_t, ctypes.c_void_p, ctypes.c_int, ctypes.c_int, ctypes.c_uint,
     ctypes.POINTER(ctypes.c_void_p)]
lib.CCCryptorUpdate.argtypes = [ctypes.c_void_p, ctypes.c_void_p, ctypes.c_size_t,
                                ctypes.c_void_p, ctypes.c_size_t, ctypes.POINTER(ctypes.c_size_t)]
lib.CCCryptorReset.argtypes  = [ctypes.c_void_p, ctypes.c_void_p]
lib.CCCryptorRelease.argtypes= [ctypes.c_void_p]

# arg order: (op, mode, alg, padding, IV, key, keyLen, tweak, tweakLen, numRounds, options, &ref)
lib.CCCryptorCreateWithMode(kCCEncrypt, kCCModeCTR, kCCAlgorithmAES, ccNoPadding,
                            iv, key, len(key), None, 0, 0, 0, ctypes.byref(ref))
```

**Throughput [measured], one `CCCryptorReset` + one `CCCryptorUpdate` per frame:**

| access-unit size | throughput | per frame |
|---|---|---|
| 1,200 B (tiny) | 921 MB/s (7,365 Mbps) | 1.3 µs |
| 8,192 B | 4,547 MB/s | 1.8 µs |
| **41,667 B** (1080p AU @ 8 Mbps/30 fps) | **10,508 MB/s (84,067 Mbps)** | **4.0 µs** |
| 262,144 B (4K AU) | 13,078 MB/s | 20.0 µs |

Previous session, same wrapper: 6,485 MB/s at 41,667 B; 7,109 MB/s @200 KB; 9,074 MB/s @1 MiB;
**811 MB/s when the same data was split into 1,200-byte chunks** (≈1.4 µs fixed ctypes overhead per call).
Macast's own pure-Python keystream measured **1.33 MB/s** in the previous session.

⇒ **Encrypt each whole access unit in one `CCCryptorUpdate`.** Never chunk. The realistic gain over
the current pure-Python path is **~4,900–7,900×**, i.e. a 1080p30 8 Mbps stream needs 1.0 MB/s and
CommonCrypto delivers 10,508 MB/s — five orders of magnitude of headroom.

Even the naive create-per-frame pattern hits 4,994 MB/s, so `CCCryptorReset` is an optimisation, not
a correctness requirement. But it *is* bit-exact:

**Correctness [measured, this session]:**
- `CCCryptorReset(ref, iv)` + `Update` produces byte-identical output to a fresh
  `CCCryptorCreateWithMode(..., iv, ...)` for `frame_id ∈ {0,1,2,3,7,255,256,65535,2^31}`
  using Macast's exact `frame_iv()` (`00*8 || be32(frame_id) || 00*4` XOR `aesIvMask`). ✅
- CommonCrypto output is byte-identical to `/usr/bin/openssl enc -aes-128-ctr` for all nine of those
  IVs. ✅ So the IV **is** consumed as a full 16-byte initial counter block.

**⚠ Counter-width caveat — this is a genuine correction to the hypothesis in the brief.**
CommonCrypto CTR increments **only the low 64 bits** of the counter block; the high 64 bits are a
fixed nonce. LibreSSL/OpenSSL increments the **full 128 bits**. Proved [measured] by AES-ECB block
matching with key `000102…0f`:

```
IV = 0xff*16
  CommonCrypto block0 keystream = 3c441f32ce07822364d7a2990e50bb13 = ECB(0xff*16)            ✓
  CommonCrypto block1 keystream = 25d4e948bd5e1296afc0bf87095a7248 = ECB(0xff*8 || 0x00*8)     ✓  (low-64 wrap)
                                                              ≠ ECB(0x00*16)  (full-128 wrap)   ✗
```

**Why this is still safe for Cast:** `frame_iv` places `frame_id` in bytes 8..12 and zeros in
bytes 12..16, so the incrementing low 64 bits have 2^32 blocks = **68.7 GB of headroom per access
unit**. A carry into the fixed high half would require `aesIvMask[8:16]` to be within ~2,600 of
2^64 — probability ≈ 2^-51 per session. That is why all nine frame IDs above match LibreSSL exactly.
Recommend a **one-time known-answer self-test at startup** (encrypt a fixed 32-byte buffer with a
fixed key/IV, compare against a hardcoded constant) so a future macOS that changes this fails loudly
instead of producing garbage a receiver cannot decode.

**AES-NI / ARMv8 Crypto Extensions:** not confirmed by disassembly. The observed 10.5 GB/s
single-threaded (≈84 Gbps, ~0.33 cycles/byte at 3.5 GHz) is exactly the ARMv8 Crypto-Extension
`AESE/AESMC` rate with 4-way block interleaving; a software AES-128 table implementation tops out
around 100–200 MB/s per core. Treat as **strongly implied, not directly verified**.

**⚠ macOS pitfall [measured]:** `ctypes.CDLL('libcrypto.dylib')` by bare name **aborts the
interpreter** (`SIGABRT`, exit 134) because CPython already has Homebrew's `libcrypto` loaded and the
flat-namespace load collides (`"... is loading libcrypto in an unsafe way"`). On macOS use
CommonCrypto only; never probe LibreSSL by bare soname.

### 1.2 Windows: `bcrypt.dll` — signatures confirmed, throughput **[unverified — no benchmark run yet]**

Confirmed from Microsoft's own docs (reachable from this host):

`BCryptEncrypt` — <https://learn.microsoft.com/en-us/windows/win32/api/bcrypt/nf-bcrypt-bcryptencrypt>

```c
NTSTATUS BCryptEncrypt(
  BCRYPT_KEY_HANDLE hKey, PUCHAR pbInput, ULONG cbInput, VOID *pPaddingInfo,
  PUCHAR pbIV, ULONG cbIV, PUCHAR pbOutput, ULONG cbOutput, ULONG *pcbResult, ULONG dwFlags);
```

Key facts from that page:
- `pbIV` is **`[in, out]`** — "This function will modify the contents of this buffer." CTR advances
  the counter in place, so pass a **fresh mutable 16-byte buffer per frame** (the Cast `frame_iv`).
  There is no `BCryptReset`-equivalent; re-seeding *is* passing a new `pbIV`.
- `pbInput` and `pbOutput` may alias (in-place encryption allowed).
- For a symmetric key with no padding, `dwFlags = 0`; `cbInput` need not be a multiple of the block
  size in CTR.
- Minimum supported client: **Windows Vista** for `BCryptEncrypt`; `BCryptSetProperty` docs
  (same site) confirm the `BCRYPT_CHAINING_MODE` property and the requirement that its value be a
  **null-terminated UTF-16 string**.

Setup sequence (values cross-checked against real-world callers):

```
BCryptOpenAlgorithmProvider(&hAlg, L"AES", NULL, 0)
BCryptSetProperty(hAlg, L"ChainingMode", (PUCHAR)L"ChainingModeCTR",
                  (wcslen(L"ChainingModeCTR")+1)*2, 0)      # BCRYPT_CHAINING_MODE / BCRYPT_CHAIN_MODE_CTR
BCryptGetProperty(hAlg, L"ObjectLength", &cbKeyObj, ..., &cbData, 0)   # size the key object yourself
BCryptGenerateSymmetricKey(hAlg, &hKey, keyObj, cbKeyObj, key, 16, 0)
BCryptEncrypt(hKey, in, n, NULL, iv16, 16, out, n, &cbResult, 0)
```

- `BCRYPT_CHAINING_MODE` = `L"ChainingMode"` — confirmed in
  <https://github.com/wine-mirror/wine/blob/master/include/bcrypt.h> (line 56).
- `BCRYPT_CHAIN_MODE_CTR` = `L"ChainingModeCTR"` — confirmed in real callers, e.g.
  <https://github.com/user-unfold/HLPlayer/blob/master/src/crypto/AesCtr256.h> which `#define`s it
  with the comment *"MinGW's bcrypt.h may lack these constants"* and describes the layout as
  `nonce[12] || counter[4]` with "BCrypt handles CTR counter increment in hardware; bulk data passes
  through in a single API call".
- That same file implements an **ECB fallback** (encrypt the counter block with `ChainingModeECB`,
  XOR by hand) for pre-Windows-8 systems. Adopt the same belt-and-braces shape.

**Unverified:** the exact minimum Windows version for `ChainingModeCTR` (widely reported as
Windows 8 / Server 2012, but I could not load the authoritative Microsoft page — the CNG property
identifiers page 404s from this host), and any Windows throughput number. Plan for a runtime probe
(`BCryptSetProperty` returns `STATUS_NOT_SUPPORTED` if CTR is absent) with the ECB-manual-CTR
fallback.

### 1.3 Linux: `libcrypto` (OpenSSL) — preferred over `libgcrypt`

`EVP_aes_128_ctr()` is a **public, documented** OpenSSL API:
<https://github.com/openssl/openssl/blob/master/doc/man3/EVP_aes_128_gcm.pod> lists
`EVP_aes_128_ctr`, `EVP_aes_192_ctr`, `EVP_aes_256_ctr` and states AES supports
"CBC, CCM, CFB (1-bit and 8-bit shift), **CTR**, ECB, GCM, OFB".

Sequence:
```
EVP_CIPHER_CTX_new()
EVP_EncryptInit_ex(ctx, EVP_aes_128_ctr(), NULL, key, iv)   # 16-byte iv = full counter block
EVP_EncryptUpdate(ctx, out, &outl, in, inl)                  # repeat per frame after re-Init
EVP_CIPHER_CTX_free(ctx)
```
For a stream cipher `EVP_EncryptFinal_ex` returns 0 bytes, so it can be skipped or called once.
Re-seeding per frame = call `EVP_EncryptInit_ex(ctx, NULL, NULL, NULL, iv)` again (passing `NULL`
cipher/key reuses the existing one).

**Why OpenSSL over libgcrypt:**
1. `libcrypto.so.3` / `libcrypto.so.1.1` is present on effectively every desktop Linux that has
   Python — `curl`, `apt`/`dnf`, `wget`, `ssh` all link it, and **CPython's own `_ssl` module links
   it**. So `import ssl` succeeding is itself a proof the image is in the process.
2. Because it is already loaded, `ctypes.CDLL("libcrypto.so.3")` resolves by soname from the loaded
   image without touching the filesystem — no `-dev` symlink needed.
3. Robust fallback: parse `/proc/self/maps` for the actual `libcrypto.so*` path the `_ssl` extension
   pulled in, then `CDLL` that absolute path.
4. `libgcrypt.so.20` has no such guarantee (it is a GNOME/GPG dependency, not a Python one) and its
   API (`gcry_cipher_open(GCRY_CIPHER_AES128, GCRY_CIPHER_MODE_CTR, 0)` /
   `gcry_cipher_setctr` / `gcry_cipher_encrypt`) has no advantage here.

**Suggested `dlopen` chain:** `libcrypto.so.3` → `libcrypto.so.1.1` → `libcrypto.so.1.0.0` →
`/proc/self/maps` scan → `libgcrypt.so.20` → pure-Python (keep the 4.5 Mbps cap as the last resort).

**Unverified:** no Linux host here, so no measured MB/s and no confirmation of the soname list on
Alpine/musl (musl ships `libcrypto.so.3` too, but Macast's Linux build is glibc-oriented).

### 1.4 Does the stdlib give AES? **No — [measured]**

```
hashlib.algorithms_available =
  blake2b blake2s md5 md5-sha1 ripemd160 sha1 sha224 sha256 sha384
  sha3_224 sha3_256 sha3_384 sha3_512 sha512 sha512_224 sha512_256
  shake_128 shake_256 sm3
any('aes' in a for a in ...) -> False
hmac: no cipher primitives at all
```
`hashlib`, `hmac`, `ssl`, `secrets`, `zlib`, `bz2`, `lzma` — none expose a block cipher. There is no
stdlib AES in CPython.

### 1.5 If a pip dependency were acceptable — packaging cost [measured from pypi.org JSON]

| package | version | wheel size (mac arm64 / win_amd64 / manylinux x86_64) | transitive deps | does AES-CTR? |
|---|---|---|---|---|
| **none (ctypes to OS lib)** | — | **0 MB, 0 deps** | — | **yes** |
| `pycryptodome` | 3.23.0 | 2.50 / 1.80 / 2.27 MB | none | yes (`Crypto.Cipher.AES.new(k, AES.MODE_CTR, nonce=b'', initial_value=iv)` — needs `nonce=b''` to get full-128-bit counter semantics) |
| `cryptography` | 50.0.2 | 3.91 / 3.82 / 4.72 MB | **`cffi>=2.0.0` → `pycparser`** (2 extra) | yes |
| `pynacl` | 1.6.2 | 0.39 / 0.24 / 1.40 MB | `cffi` | **NO — libsodium is ChaCha20/XSalsa20/AEGIS, no AES.** Useless here. |
| `pyopenssl` | 26.4.0 | 0.06 MB (pure) | `cryptography`, `cffi` | indirectly, and no CTR convenience |
| `oscrypto` | 1.3.0 | 0.19 MB (pure) | `asn1crypto` | yes — literally the same idea as §1.1–1.3, but unmaintained since 2022 |

Against Macast's actual packaging surface
(`/Users/pavia/githome/Macast/scripts/setup_py2app.py` lines 474 & 494:
`'packages': ['rumps','macast','macast_renderer','zeroconf','ifaddr']`,
`'includes': ['cherrypy','lxml','netifaces','appdirs','pyperclip', ...]`;
`/Users/pavia/githome/Macast/.github/workflows/build.yml` lines 324–347 and 423–426: two
`--collect-all` / ~20 `--hidden-import` blocks) — `cryptography` costs **+3 places × 3 packages**
(`cryptography`, `_cffi_backend`, `cffi`) plus 4–5 MB per platform, for something the OS already
provides at higher speed and zero size.

**Recommendation (fallback chain, in order):**

1. `sys.platform == 'darwin'` → `ctypes.CDLL('/usr/lib/system/libcommonCrypto.dylib')`,
   `CCCryptorCreateWithMode` + per-frame `CCCryptorReset` + one `CCCryptorUpdate` per access unit.
   Known-answer self-test at import.
2. `os.name == 'nt'` → `ctypes.WinDLL('bcrypt')`, `ChainingModeCTR`; on `STATUS_NOT_SUPPORTED`
   fall back to `ChainingModeECB` + manual counter increment.
3. else → `ctypes.CDLL` on `libcrypto.so.3` → `.so.1.1` → `/proc/self/maps` scan → `libgcrypt.so.20`.
4. Anything raises → keep today's pure-Python cipher and keep `CAST_STREAM_MAX_BITRATE = 4500000`.
   The cap becomes a *degraded-mode* constant, not a design constant.

---

## 2. Q2 — VideoToolbox low-latency tuning on macOS

### 2.1 The option list is much smaller than assumed **[measured: `ffmpeg -h encoder=h264_videotoolbox`, 9.0.2]**

Available: `-profile`, `-level`, `-coder cavlc|cabac`, `-a53cc`, `-constant_bit_rate` (macOS 13+),
`-max_slice_bytes`, `-allow_sw`, `-require_sw`, `-realtime`, `-frames_before`, `-frames_after`,
`-prio_speed`, `-power_efficient`, `-spatial_aq`, `-max_ref_frames`.

**Confirmed absent:** `-tune` (so `-tune zerolatency` genuinely does **not** exist for videotoolbox),
`-preset`, `-entropy`, `-forced-idr`. Threading capabilities: `none`.

Upstream `libavcodec/videotoolboxenc.c` is still moving on latency:
`c1dc2e2b7` (2025-09-10) *"ensure bitrate is set in low_delay mode"*,
`d87210745` (2025-09-06) *"allow low latency RC with HEVC"*.

### 2.2 VT's added latency is **time-based, not frame-count-based** [measured]

`-re` fed 1080p, first-byte time from `-f mpegts pipe:1`, two runs each:

| fps | stream copy | x264 ultrafast+zl | VT realtime | VT plain |
|---|---|---|---|---|
| 15 | — | 44.8 ms | 228.4 ms (3.4 frame periods) | 230.6 ms |
| 24 | — | 43.6 ms | 249.0 ms (6.0 fp) | 237.0 ms |
| 30 | 32.0–40.3 ms | 41.4 ms | 242.1 ms (7.3 fp) | 234.8 ms |
| 60 | — | 40.7 ms | 221.9 ms (13.3 fp) | 234.0 ms |

Flat **~220–250 ms regardless of frame rate** ⇒ it is fixed pipeline depth inside the
`VTCompressionSession`, not N frames of lookahead. Steady-state inter-frame gaps tracked the ideal
for every config, so nothing was throughput-starved.

Per-option first-byte at 1080p30 / 8 Mbps [measured, previous session]:
copy **32.8**; x264 ultrafast+zl **48.7**; veryfast 50.8; medium 59.1; slow 59.6;
**x264 ultrafast *without* zerolatency 75.5** (`-tune zerolatency` is worth 27 ms);
VT variants **200.9–253.1** in every combination: realtime 219.9, plain 208.5, `+constant_bit_rate`
252.0, `+max_ref_frames 1` 202.0, `+allow_sw` 253.1, `+bf 0` 200.9, `+coder cabac` 218.4.

⇒ **No VideoToolbox option removes the ~190 ms.** `-realtime`, `-prio_speed`, `-bf 0`,
`-max_ref_frames 1`, `-frames_before/after 0` all fail to move it.

### 2.3 Throughput and CPU cost [measured, 30 fps target, 20 Mbps]

> **这张表量的是离线吞吐，不是投屏的成本 —— 2026-10-02 补记。**
> 表里的源是"能喂多快就编多快"，所以 `fps` 一列是编码器的**上限**，`% of one core`
> 是跑在那个上限时的 CPU。它被误读成"实时镜像桌面时的开销"过一次，误读的结果
> （"x264 吃约 4.5 核 / VideoToolbox 约 0.2 核"）写进了 AGENTS.md §4.8，
> 于是硬件编码看起来便宜了二十倍。实时路径的同一对数字是 **0.59 核 vs 0.41 核**
> （见下面 §2.3b）。**两组数都是真的，回答的不是同一个问题**：
> 这组回答"还有多少余量"，那组回答"镜像的时候风扇转不转"。
>
> 复核（2026-10-02，`-f lavfi -i testsrc2=size=…:rate=30:duration=6`、`-f null -`、
> 与本表同样的 x264 argv）：1920×1080 **486 fps / 4.20 核**、2560×1600 **295 fps / 4.63 核**、
> 3456×2234 **171 fps / 4.94 核** —— 与下表 536.9 / 315.9 / 185.3 fps、436% / 450% / 483%
> 逐档对得上（差异是本机负载与取样时长）。所以**下表不是重复帧假象**；
> 同一趟里"重复帧假象"另有其人，是**实时采集**在缺输出 `-r` 时的 442 fps，
> 见 `scripts/encoder_latency_probe.py` 的 `insane`。

| resolution | config | fps | × realtime | % of one core | ms/frame |
|---|---|---|---|---|---|
| 1920×1080 | x264 ultrafast+zl | **536.9** | **17.90×** | 436.0 | 8.12 |
| | x264 superfast | 270.6 | 9.02× | 348.2 | 12.87 |
| | x264 veryfast | 224.3 | 7.48× | 350.2 | 15.61 |
| | VT realtime | 87.6 | 2.92× | **19.8** | 2.26 |
| | VT plain | 153.5 | 5.12× | 31.5 | 2.06 |
| | hevc_videotoolbox rt | 81.4 | 2.71× | 21.8 | 2.68 |
| 2560×1600 | x264 ultrafast+zl | 315.9 | 10.53× | 450.3 | 14.25 |
| | VT realtime | 80.2 | 2.67× | 29.5 | 3.67 |
| | hevc_videotoolbox rt | 75.0 | 2.50× | 28.5 | 3.80 |
| 3456×2234 | x264 ultrafast+zl | 185.3 | 6.18× | 482.7 | 26.06 |
| | VT realtime | 50.8 | 1.69× | 31.5 | 6.20 |
| | hevc_videotoolbox rt | 49.3 | 1.64× | 31.0 | 6.28 |
| 3840×2160 | x264 ultrafast+zl | 169.5 | 5.65× | 480.4 | 28.34 |
| | x264 superfast | 125.8 | 4.19× | 399.4 | 31.75 |
| | VT realtime | 47.9 | 1.60× | 30.5 | 6.37 |
| | VT plain | 49.2 | 1.64× | 30.7 | 6.24 |
| | hevc_videotoolbox rt | 46.7 | 1.56× | 27.0 | 5.78 |

⇒ **x264 ultrafast+zerolatency sustains 5.6–17.9× realtime at every resolution up to 4K**, but burns
~4.5 cores *while doing so*. VT uses ~0.2–0.3 of a core with only 1.6–5.1× headroom.
The repo's premise that x264 cannot sustain Retina is **false on throughput** and only true on
CPU/thermal grounds *at the throughput ceiling*. The real trade is **latency (x264 wins by ~190 ms)
vs CPU (VT wins, but by 0.18 of a core on the live path, not by 15×)**.

### 2.3b What the live path actually costs [measured 2026-10-02, real desktop capture, 24 fps, 4 Mbps, 8 s, three runs each]

§2.3 answers "how fast can it go". Mirroring never asks that: the capture hands over 24 fps and the
encoder waits for the next one, so its CPU is the cost of encoding 24 fps and nothing more.
`scripts/encoder_latency_probe.py` measures that shape (`cores = cpu_seconds / wall_seconds`, the
clock stopped *before* `terminate()` so VideoToolbox's slow death does not inflate `wall`):

| variant (1080p, native 3456×2234 capture scaled down) | first byte ms | cores | bytes | realtime |
|---|---|---|---|---|
| x264 ultrafast+zerolatency | 528.9 / 509.4 / 519.9 | **0.59** | 4,915,200 | 0.94× |
| h264_videotoolbox | 753.4 / 706.5 / 734.0 | **0.41** | 2,949,120 | 0.94× |
| hevc_videotoolbox | 720.2 / 760.9 / 726.9 | 0.42 | 2,621,440 | 0.94× |
| libx265 ultrafast | 574.8 / 544.9 / 562.1 | 1.67 | 4,980,736 | 0.94× |
| x264 zl + `mpdecimate`/vfr | 532.9 / 522.3 / 534.1 | 0.52 | 3,538,944 | 0.68× |
| h264_videotoolbox + `mpdecimate`/vfr | 761.7 / 768.4 / 738.4 | 0.41 | 2,293,760 | 0.70× |
| x264 **without** `-tune zerolatency` + `mpdecimate`/vfr | 1353.5 / 1419.2 / 1395.9 | 0.47 | 3,145,728 | 0.56× |

`first byte` includes ~0.5 s of ffmpeg startup that every row pays, so **the number to read is the
difference between rows, not any row**. At 2160p: x264 zl 541.4 ms / 1.12 cores / 0.94×;
hevc_videotoolbox 800.1 ms / 0.64 cores / **0.82× (it fell behind)**; libx265 646.2 ms / **4.17 cores**.

Five conclusions, each of which killed or deferred a planned change:

1. **The CPU argument for `auto`→hardware is much weaker than the repo said.** 0.59 vs 0.41 cores,
   i.e. VT saves 0.18 of a core and costs ~200 ms of first byte. The ~190–250 ms VT penalty in §2.2
   is confirmed; the "4.5 cores is a fan and a battery" framing is not, because 4.5 cores is the
   offline ceiling. Changing the `auto` default is a user decision, not a measurement — it is
   deferred, and `ENCODER_TRADEOFF` (user-visible, bound by suite cases) still says hardware.
2. **HEVC buys bandwidth, not latency.** hevc_vt vs h264_vt first byte across three runs:
   −33 / +54 / −7 ms ⇒ indistinguishable. Bytes 11–13% smaller, cores identical. At 2160p HEVC
   produced *more* bytes than x264 and fell behind (0.82×). ⇒ the multi-codec OFFER moves to
   batch 3, where codec negotiation is needed anyway (WebRTC). No television on this LAN to verify
   HEVC acceptance in a Cast Streaming OFFER either.
3. **`mpdecimate` buys bandwidth, not latency.** vs plain x264-zl: first byte +3.9/+12.9/+14.2 ms
   (≈free), bytes **−26 to −28%**, cores 0.52 vs 0.59. That is the *best* case (a static desktop);
   with video playing it drops almost nothing. It also collides with two shipped things: CFR
   (`-r`) duplicates back every dropped frame, and the DLNA fake-file shape derives its
   `Content-Length` from a nominal bitrate, so producing fewer bytes can drain the television's
   buffer. ⇒ not shipped.
4. **Software HEVC is strictly worse and the question is closed.** libx265 ultrafast: +42 ms first
   byte at 1080p for 1.67 cores (2.8× x264, 4× VT) and *no* byte saving (+1.3%); 4.17 cores at 2160p.
5. **Dropping `-tune zerolatency` costs +743 to +910 ms of first byte** — the single largest latency
   term in the table, and an independent confirmation of the choice the plugin already makes.

### 2.3c Two zero-frame bugs this measurement found in the shipped argv [measured 2026-10-02]

Neither is a shape question, which is why the stub ffmpeg in Parts 21/22/23 answered both happily.
Both end in the same wrong place: no frames, and a `PERMISSION_DOOR` message telling the user to
fix 系统设置 → 隐私与安全性 → 屏幕录制 for a number we typed ourselves.

**(a) `-level 42` was hardcoded for `h264_videotoolbox`.** Level 4.2 caps a frame at 8704
macroblocks (2208×1242); a 3456×2234 desktop is 30384 of them. Real ffmpeg, real capture:

| argv | result |
|---|---|
| 1080p + `-level 42` + maxrate/bufsize (**shipped**) | works, 3/3 |
| native 3456×2234 + `-level 42` | **exit 187, 2068 bytes, 2/2** |
| native + `-level 51` | works, 1,797,725 bytes |
| native + no `-level` | works, 1,799,372 bytes |
| 2160p + `-level 42` | exit 187 |
| 2160p + no `-level` | works, 2,130,382 bytes, 67 frames, 0.959× |
| 1080p + `-level 42`, no maxrate/bufsize | fails |
| no `-level`, VT's own choice at 1080p | **level 4.0**, High, 1920×1080 |

Reproduced a second time with a *file* source, so the capture is not involved at all:
`testsrc2` at 2560×1600 and 3456×2234 both give **0 frames** under `VT + -level 42`, while
1920×1080 gives 180. Since `auto` picks hardware on a Mac, the shipped default for the 原画
preset was "produce nothing". Fix: `vt_level(height)` — 42 up to 1080 lines, 51 up to 2160,
**omitted when the size is unknown or larger** (`height == 0` is 原画). Omitting is not a shrug:
VT computes a level matching the real picture at every size tried, and at 1080p it picks 4.0,
*more* conservative than the 4.2 we pinned. `caststream` still gets 42 because
`cast_stream_shape` has already replaced the request with the 1920×1080 the OFFER promised —
there the level is a real promise. `has_hardware_encoder` cannot catch this class: it greps
`ffmpeg -encoders`, which lists an encoder that then refuses the picture.

**(b) The live shapes passed no output `-r`.** When avfoundation cannot estimate the rate it
prints `Configuration of video device failed, falling back to default` then
`Stream #0: not enough frames to estimate rate`, and the input arrives with a degenerate timebase.
x264 multiplies the frame's macroblock count by something near 1e6 and answers
`MB rate (14400000000) > level limit (16711680)` with **zero bytes**. One variable changed, same
environment: no `-r` ⇒ 0 bytes; `-r 24` ⇒ **3,932,160 bytes**. VideoToolbox tolerates the same
input and emits bytes, which is exactly why `auto` on a Mac hid it — the fallback to software
happens when the capture is already unhappy. `build_dlna_command` has always passed
`-r profile.fps`; the other three shapes were the ones left guessing.

After both fixes, all seven shapes produce real bytes at 原画 (cast/browser/caststream ×
software/hardware, plus dlna), verified by running the plugin's own `build_ffmpeg_command` against
the real capture. `-r` and `-fps_mode vfr` are mutually exclusive and a suite case now says so,
which is the constraint conclusion 3 above would have hit.


### 2.4 Quality per bit — VMAF [measured, 1080p `testsrc2`, 8 s, `-g 30`]

| config | 2 Mbps target | 4 Mbps target | 8 Mbps target |
|---|---|---|---|
| x264 ultrafast + zl | 2112 kbit / **83.74** | 4216 / 90.78 | 8420 / 96.07 |
| x264 veryfast + zl | 2107 / 86.41 | 4162 / 92.06 | 8249 / 96.73 |
| x264 medium + zl | 2095 / 89.40 | 4120 / 94.08 | 8294 / 97.78 |
| x264 slow + zl | 2087 / 89.48 | 4137 / 94.31 | 8307 / 97.96 |
| x264 ultrafast (no zl) | 2064 / 85.57 | 4069 / 92.01 | 8135 / 96.15 |
| **VT realtime** | **1400 / 82.79** | **2802 / 91.47** | **5597 / 95.30** |
| VT plain | 1400 / 82.79 | 2802 / 91.47 | 5597 / 95.30 |
| VT + `constant_bit_rate` | 1335 / 83.46 | 2668 / 91.08 | 5329 / 95.10 |

Findings:
- **VT undershoots the target by ~30–33%.** This is rate-control conservatism, *not* a quality loss:
  at 4 Mbps VT scores **91.47 VMAF at 2802 kbit** versus x264 ultrafast+zl's **90.78 at 4216 kbit** —
  better picture at **two-thirds the bitrate**.
- ⇒ **If you keep VT, request ~1.5× the wire bitrate you actually want.** That is a one-line change
  and buys ~1.3 VMAF for free at the same bandwidth.
- VT `realtime` and VT `plain` are **identical** in both size and quality (and `-prio_speed` produced
  byte-identical output in the earlier session). `-realtime 1` in Macast's `encoder_args()` buys
  nothing measurable on this hardware — it only selects the low-delay RC path.
- `-tune zerolatency` costs ~1.2–1.8 VMAF at 2/4 Mbps and only ~0.1 at 8 Mbps, in exchange for 27 ms.
  At 8 Mbps+ it is essentially free.
- **Zero B-frames from every config tested** (8 I + 232 P in all 24 runs) ⇒ Macast does not need
  `-bf 0`, and the existing `first_mb_in_slice == 0` multi-slice handling is doing the right thing.

---

## 3. Q3 — HEVC / AV1 hardware encoders and what TVs actually decode

### 3.1 `av1_videotoolbox` **does not exist** in FFmpeg

`libavcodec/allcodecs.c` (master) registers only `ff_h264_videotoolbox_encoder`,
`ff_hevc_videotoolbox_encoder`, `ff_prores_videotoolbox_encoder`. AV1 + VideoToolbox is
**decode-only**: `ff_av1_videotoolbox_hwaccel`, `videotoolbox_av1.c`, `CONFIG_AV1_VIDEOTOOLBOX_HWACCEL`.
Local `ffmpeg -encoders` agrees: `libsvtav1, libx264, libx264rgb, h264_videotoolbox, libx265,
hevc_videotoolbox, prores_videotoolbox, libvpx, libvpx-vp9`.

AV1 *encoders* that do exist upstream: `ff_av1_nvenc_encoder`, `ff_av1_qsv_encoder`,
`ff_av1_amf_encoder`. Plus `ff_vp9_qsv_encoder` and the `h264/hevc_{nvenc,qsv,amf}` families.

### 3.2 Genuine low-latency modes, confirmed from FFmpeg source

| backend | low-latency recipe | source |
|---|---|---|
| **NVENC** | `-preset p1` (`"fastest (lowest quality)"`, `PRESET_P1`) + `-tune ull` (`NV_ENC_TUNING_INFO_ULTRA_LOW_LATENCY`) + `-delay 0` | `libavcodec/nvenc_h264.c` lines 29–44; `libavcodec/nvenc.c` lines 1715–1717 select `init_encode_params.tuningInfo` from it, and `nvenc.c:1062` maps `-ldkfs` → `rcParams.lowDelayKeyFrameScale` |
| **QSV** | `-preset veryfast -async_depth 1`, `-low_power 1` (VDENC), `-low_delay_brc 1`, keep `look_ahead` off | `libavcodec/qsvenc.c` lines 806, 1146–1162, 1649, 2364–2379 |
| **AMF** | `-usage ultralowlatency` (`AMF_VIDEO_ENCODER_USAGE_ULTRA_LOW_LATENCY`), `-latency 1`, `-quality speed` | `libavcodec/amfenc_h264.c` lines 35–41, 73, 401 |
| **VideoToolbox** | `-realtime 1` (+ `-prio_speed 1`) — but see §2.2, it does not reduce the ~190 ms | `ffmpeg -h encoder=h264_videotoolbox` |
| **VAAPI** | `-low_power 1` | Google's reference sender |

**Google's own reference Cast sender** (`chromium/openscreen`
`cast/standalone_sender/streaming_ffmpeg_encoder.cc`) sets, for *every* backend:

```cpp
context->flags      |= AV_CODEC_FLAG_LOW_DELAY;
context->max_b_frames = 0;
context->thread_type  = FF_THREAD_SLICE;
context->pix_fmt      = AV_PIX_FMT_YUV420P;
context->framerate    = AVRational{30, 1};
context->bit_rate     = target_bitrate;
context->rc_max_rate  = target_bitrate;
context->rc_buffer_size = target_bitrate / 2;   // "500 ms of target bitrate"
context->gop_size   = 999999;                   // no periodic keyframes; IDR on demand
context->keyint_min = 999999;
```
with per-backend privates: `libx264/libx265` → `preset=ultrafast, tune=zerolatency, forced-idr=1`
(x265 also `frame-threads=1`); `VideoToolbox` → `realtime=1, prio_speed=1`; `Nvenc` → `preset=p1,
tune=ull, delay=0`; `QSV` → `preset=veryfast, async_depth=1`; `VAAPI` → `low_power=1`.

⇒ **This is near-exact validation of Macast's current software path** (`libx264 -preset ultrafast
-tune zerolatency -profile:v high`), with three deltas worth adopting: `-flags +low_delay`,
`-thread_type slice` (Macast gets sliced threads implicitly from `-tune zerolatency`),
and `rc_buffer_size = bitrate/2` rather than a fixed `2304k`.

### 3.3 Which codecs do real Cast receivers decode? — **Don't hardcode a table. Negotiate.**

This is the single most useful finding in Q3. The Cast Streaming protocol makes the *receiver* pick.

`chromium/openscreen` `cast/streaming/impl/streaming_session_protocol.md`:
> *"Sender only includes codecs it supports and the order of the stream objects shows the sender's
> preference. Receiver can choose any stream it prefers, or the first stream it supports if it
> doesn't have any preferences. Receiver informs sender about the selected stream objects in the
> `sendIndexes` of the ANSWER object."*

> *"To be compliant, cast receivers must at least implement `opus`, if they support audio, and `vp8`
> if they support video. Senders must implement at least the baseline codecs (`h264` or `vp8`, and
> `aac` or `opus`)."*

> *"The set of codec profiles supported for Cast playback / remoting is notably larger than the codec
> profiles supported for **mirroring** streams."*

> ⚠ *"Some legacy receivers may report `vp9` and `hevc` in their `mediaCaps` response, even if they
> cannot **remote** these codecs."* — i.e. do not infer mirroring capability from `/setup/eureka_info`
> `mediaCaps`. Offer and let the ANSWER decide.

`codecParameter` is an RFC 6381 media-type string, e.g. `avc1.64002A` (H.264 level 4.2) or
`hev1.1.6.L150.B0` (HEVC Main 5.0).

`VideoCodec` enum in `cast/streaming/public/constants.h` = `{ kH264, kVp8, kHevc, kNotSpecified, kVp9, kAv1 }`.

**Chrome's sender** (`chromium/chromium components/mirroring/service/openscreen_session_host.cc`)
offers in this priority order:
```cpp
constexpr std::array kSupportedVideoCodecs{
    media::VideoCodec::kHEVC, media::VideoCodec::kAV1, media::VideoCodec::kVP9,
    media::VideoCodec::kH264, media::VideoCodec::kVP8 };
```
filtered by `encoding_support` / `IsHardwareEnabled`. And
`components/mirroring/service/README.md` documents a **two-stage negotiation**:
> *"1. Hardware-First Offer: … only hardware-accelerated video codecs.
> 2. Software Fallback: If the receiver rejects this initial offer (indicated by a
> `kNoStreamSelected` error from Open Screen), the Mirroring Service gracefully falls back by sending
> a second OFFER that includes software-based video codecs."*
On failure it calls `MaybeDenylistHardwareCodecAndRenegotiate(codec)` and logs
*"No stream selected for ideal, hardware-accelerated codecs. Attempting fallback to software codecs."*

**⇒ Verdict for Macast.** Macast's `parse_answer()` currently does:
```python
if 0 not in indexes:
    raise RuntimeError('接收端没有接受这条视频流')
```
That hardcodes a single-stream, index-0, H.264-only offer. To exploit HEVC you must (a) emit
multiple `supportedStreams` entries in preference order, (b) accept whatever index comes back in
`sendIndexes` and read that entry's `codecName`, (c) on `kNoStreamSelected` re-OFFER with H.264 only.
This is exactly Chrome's algorithm and exactly what the spec prescribes.

**Is HEVC worth it?** Yes for quality-per-bit on receivers that take it (`hevc_videotoolbox` costs
~2.7 GB/s less… actually: 81.4 fps @ 21.8% CPU vs `h264_videotoolbox` 87.6 fps @ 19.8% CPU —
essentially the same CPU, and HEVC typically buys ~30–40% bitrate at equal quality). **AV1 is not
worth it**: there is no AV1 encoder on Apple hardware in FFmpeg, and openscreen's own standalone
sender CLI defaults to `vp8` and merely *accepts* `vp8, vp9, av1, h264, hevc`.

**DLNA:** [unverified from primary sources — `developers.google.com`, `en.wikipedia.org`,
`flatpanelshd.com` all unreachable]. From what is in-repo: Macast's `build_dlna_command` pins
profile/codec/container per a compatibility profile for "a renderer of ten years ago". HEVC in
MPEG-TS is not reliably decodable on such devices and Macast already has an H.264-baseline DLNA path.
**Recommendation: leave DLNA on H.264; the codec negotiation win is on the Cast path only.**

---

## 4. Q4 — Capture latency and alternatives to ffmpeg's `avfoundation`

### 4.1 FFmpeg has no ScreenCaptureKit, no Windows.Graphics.Capture, no PipeWire input

`libavdevice/` (master) contains exactly:
`alsa, android_camera, audiotoolbox.m, avfoundation.m, caca, ccfifo, decklink, dshow*, fbdev,
gdigrab.c, iec61883, jack, kmsgrab.c, lavfi, libcdio, libdc1394, openal, oss, pulse_audio*,
reverse, sndio, v4l2*, vfwcap, xcbgrab.c, xv`.
`doc/indevs.texi` sections: alsa, android_camera, avfoundation, decklink, dshow, fbdev, gdigrab,
iec61883, jack, **kmsgrab**, lavfi, libcdio, libdc1394, openal, oss, pulse, sndio, v4l2, vfwcap,
x11grab.

⇒ Confirms the in-repo comment "the screencapturekit demuxer never shipped". There is **no
`-f screencapturekit`**, no `-f wgc`, no `-f pipewire`.

### 4.2 `avfoundation.m` is *not* abandoned — it is actively maintained in 2026

It still uses `AVCaptureScreenInput initWithDisplayID:` (lines 1167, 1222) with `capturesCursor`,
`capturesMouseClicks`, `minFrameDuration`, but recent upstream commits include:
`ddf8f4030` 2026-06-18 *"wait for frame consumption to avoid dropping A/V frames"*,
`c2802e520` 2026-07-11 *"add device selection by USB serial and unique ID"*,
`d65fd5581` 2026-08-03 *"keep format and frame rate range paired"*,
`a81b23a85` 2026-08-06 *"fix build with deployment targets below macOS 12"*,
`0bffa4a84` / `3a9920690` 2026-08-25 iOS/IOKit fixes.
So the deprecated-API risk is real but not imminent.

### 4.3 ScreenCaptureKit — what it actually offers (Apple primary docs, `.md` endpoints)

The HTML pages at `developer.apple.com` are JS-only placeholders (`WebFetch` sees nothing); Apple
serves machine-readable markdown at the same URL with `.md` appended. That route works.

`SCStreamConfiguration` (macOS 12.3+):
- **`capturesAudio: Bool` — availability `macOS: 13.0.0 -` [confirmed this session]** —
  *"A stream doesn't capture audio by default. Set this value to `true` if you require audio
  capture."* ⇒ **ScreenCaptureKit captures system audio natively on macOS 13+, which can retire
  BlackHole and the CoreAudio aggregate device entirely.**
- `minimumFrameInterval: CMTime` — *"The default value is `0`, which indicates that the system uses
  the maximum supported frame rate… to configure the stream to capture at 60 fps, specify a minimum
  frame interval equal to `1/60`."*
- `queueDepth: Int` — *"the system sets the queue depth to its minimum value of three frames…
  **Don't exceed a queue depth of eight frames.**"*
- `sampleRate` — 8000 / 16000 / 24000 / **48000 (default)**; also `channelCount`,
  `excludesCurrentProcessAudio`.
- `width/height/scalesToFit/sourceRect/destinationRect/preservesAspectRatio`, `pixelFormat`,
  `colorMatrix`, `colorSpaceName`, `backgroundColor`, `showsCursor`, `shouldBeOpaque`,
  `capturesShadowsOnly`, `ignoreShadows*`, `captureResolution` + `SCCaptureResolutionType`.

`SCStreamFrameInfo` keys (from `CMSampleBuffer` attachments): **`dirtyRects`**, `status`,
`scaleFactor`, `contentRect`, `presenterOverlayContentRect`.

**`dirtyRects`** — *"A key to retrieve the areas of a video frame that contain changes. The
associated value is an array of rectangles that represents **a union of the rectangles redrawn and
moved**."* ⇒ **This is the damage/dirty-region information Q4 asks for, and it exists.** No ffmpeg
input exposes it — you get it out-of-band, in the sample buffer attachments.

**`SCFrameStatus`** (<https://developer.apple.com/documentation/ScreenCaptureKit/scframestatus>) —
the change signal:
- `complete` — *"the system successfully generated a new frame"*
- **`idle` — *"the system didn't generate a new frame because the display didn't change"***
- `blank` — *"…because the display is blank"*
- `suspended` — *"…because you suspended updates"*
- `started` — first frame after stream start

`SCStreamOutputType` = `{ screen, audio, microphone }`; frames arrive via
`stream(_:didOutputSampleBuffer:of:)`.

### 4.4 How two production apps use it (primary source)

**OBS** — `plugins/mac-capture/mac-sck-video-capture.m`:
```objc
[sc->stream_properties setQueueDepth:8];
[sc->stream_properties setShowsCursor:!sc->hide_cursor];
[sc->stream_properties setColorSpaceName:kCGColorSpaceDisplayP3];
[sc->stream_properties setBackgroundColor:kCGColorClear];
CMTime frameTimeInterval = CMTimeMake(video_info.fps_den, video_info.fps_num);
[sc->stream_properties setMinimumFrameInterval:CMTimeMultiplyByFloat64(frameTimeInterval, 0.9)];
if (@available(macOS 15.0, *)) [sc->stream_properties setPixelFormat:kCVPixelFormatType_64RGBAHalf];
else                           [sc->stream_properties setPixelFormat:kCVPixelFormatType_32BGRA];
if (@available(macOS 13.0, *)) {
    [sc->stream_properties setCapturesAudio:YES];
    [sc->stream_properties setExcludesCurrentProcessAudio:YES];
    [sc->stream_properties setChannelCount:2];
}
```
Note: `minimumFrameInterval` is set to **0.9× the target interval** (slightly faster than the output
fps, letting the app's own frame-dropping throttle), and the audio path is gated on **macOS 13.0** —
independently confirming §4.3. OBS does **not** read `dirtyRects`.

**WebKit / Safari screen capture** — `Source/WebCore/platform/mediastream/cocoa/ScreenCaptureKitCaptureSource.mm`:
```objc
if (m_frameRate)
    [m_streamConfiguration setMinimumFrameInterval:CMTimeMakeWithSeconds(1/m_frameRate, 1000)];
...
switch (*status) {
case SCFrameStatusStarted:
case SCFrameStatusComplete: break;
case SCFrameStatusIdle:
case SCFrameStatusBlank:
case SCFrameStatusSuspended:
case SCFrameStatusStopped:
    return;          // <-- sample buffer dropped, NO frame delivered
}
```
⇒ **A production browser screen-share stack goes completely silent when the display is static.** It
does not synthesise duplicate frames. Strong evidence that variable-frame-rate screen sharing is
normal and well-tolerated downstream (see §5.3).

### 4.5 pyobjc binding — cost

`pyobjc-framework-ScreenCaptureKit` **v12.2.2**, `requires_python >=3.10`, universal2 wheel
**~0.01 MB (~10 KB)**, 23 releases from 2022-03-07 to 2026-08-11; cp312 wheel is
`macosx_10_13_universal2`. Its `_metadata.py` (20,589 B) exports
`SCFrameStatusIdle/Blank/Complete/Started/Stopped/Suspended`,
`SCStreamFrameInfoDirtyRects/Status/ContentRect/ScaleFactor/DisplayTime/BoundingRect/ScreenRect/
ContentScale/PresenterOverlayContentRect`, `capturesAudio`, `minimumFrameInterval`.
It is a pure-Python metadata package — the actual framework comes from the OS.

Against Macast's surface this is: **+1 line in `requirements/darwin.txt`** (right next to the
already-declared `pyobjc-framework-Cocoa`), **+1 entry in `setup_py2app.py`'s `packages` or
`includes`** (line 474 / 494), and **+1 `--hidden-import` in each of the PyInstaller jobs** in
`.github/workflows/build.yml` (lines 324–347 and 423–426 — note the macOS job uses py2app, so the
hidden-import entries are for Win/Linux where SCK is not used; check whether a macOS-only dep needs
any CI change at all). Small, but it *is* the 3-place drift the repo has been burned by.

**What it would take to capture via SCK and pipe to ffmpeg:** an `SCStream` + delegate on a
`DispatchQueue`, read `CMSampleBuffer` → `CVPixelBuffer`, convert BGRA→I420 (or hand ffmpeg
`-pix_fmt bgra` rawvideo and let it convert), write to ffmpeg's stdin as
`-f rawvideo -video_size WxH -pixel_format bgra -framerate <measured> -i -` with
**`-fps_mode passthrough`** so dropped/idle frames are not re-duplicated. The hard parts are the
Objective-C delegate + GCD queue under pyobjc (Macast already does CoreAudio aggregate-device calls
through `Foundation`/`objc`, per `requirements/darwin.txt`'s own comment, so the pattern is
established) and `CVPixelBuffer` → bytes without a copy per frame.

**Screen-recording permission:** [unverified] whether SCK avoids the TCC quirks. SCK uses the same
Screen Recording TCC prompt as `AVCaptureScreenInput`; the practical difference is that SCK is the
*supported* API and AVCaptureScreenInput is deprecated, so the risk profile is better, not the
permission flow.

### 4.6 Windows

- `gdigrab` — `libavdevice/gdigrab.c`, GDI `BitBlt`. Copies through system memory; the ceiling on a
  modern desktop.
- **`ddagrab`** — this is a **libavfilter source, not a device**: `libavfilter/vsrc_ddagrab.c` +
  `vsrc_ddagrab_shaders.h`. Used as `-f lavfi -i ddagrab=output_idx=0:draw_mouse=1` and it yields
  D3D11 textures you can `hwmap` straight into a hardware encoder with no CPU round-trip.
  First commit `f61125548` (2022-07-08, *"avfilter: add vsrc_ddagrab"*);
  `gh api compare n6.0...f61125548` → the commit is an **ancestor** of `n6.0`, while
  `n5.1...` → **diverged**. ⇒ **first shipped in FFmpeg 6.0.** This is DXGI Desktop Duplication,
  i.e. the same API Windows.Graphics.Capture sits on top of for game capture, and it is the
  single biggest Windows-side win available to Macast — **landed in v0.20 of the
  `screen_mirror` plugin** (ddagrab first, gdigrab as the runtime fallback).
- **Windows.Graphics.Capture (WGC):** no ffmpeg input exists. OBS uses it
  (`plugins/win-capture/game-capture.c`). Reaching it from Python would mean WinRT bindings — a
  much heavier dependency than `ddagrab` is. **Use `ddagrab`.**
- **Measured on a real Windows host (2026-10-02, Windows 11 build 26300, ffmpeg 8.1.2):**
  `ddagrab=output_idx=N` attaches per DXGI output — outputs 0 and 1 both report 2560×1440
  (`wrapped_avframe, bgra`), output 2 is refused verbatim (`Failed to enumerate DXGI output 2`
  + `Error opening input file ddagrab=output_idx=2:…`) and the walk stops at the first refusal.
  A full production-shape run (shipped browser argv + loopback dshow audio, 4 s) yielded a
  4,176,319-byte MP4: h264 Constrained Baseline 2560×1440 yuv420p, r_frame_rate 24/1,
  93 frames / 4.021 s, AAC audio, clean local decode. The gdigrab path on the same box
  captures the composite virtual desktop at **4000×2571** — an odd height — and x264 refuses
  it (`height not divisible by 2`), i.e. the source-resolution preset used to produce
  **zero bytes** on that box. **Not measured:** any WGC-vs-gdigrab-vs-ddagrab **latency/fps**
  comparison, CPU cost on Windows, or behavior inside an RDP session.

### 4.7 Linux

- `x11grab` (`libavdevice/xcbgrab.c`) — X11 only. **Wayland is still unsupported by ffmpeg itself.**
- **`kmsgrab`** — documented, but the docs say it plainly: *"Requires either DRM master or
  CAP_SYS_ADMIN to run. If you don't understand what all of that means, you probably don't want
  this."* And: *"framerate … is not synchronised to any page flipping or framebuffer changes — it
  just defines the interval at which the framebuffer is sampled. Sampling faster than the framebuffer
  update rate will generate independent frames with the same content."* ⇒ **Not usable from a
  menu-bar app, and it produces duplicate frames by construction. Dead end.**
- **PipeWire / xdg-desktop-portal** — the real Wayland path, and ffmpeg does not have it. **OBS does**:
  `plugins/linux-pipewire/screencast-portal.c` (+ `camera-portal.c`) implements the portal
  ScreenCast handshake and consumes the PipeWire node. `pipewiresrc` is a **GStreamer** element, not
  an ffmpeg one. Reaching it from Python would mean `pygobject` + GStreamer, or shelling out to
  `gst-launch-1.0 pipewiresrc ! … ! fdsink` and piping into ffmpeg — a large new surface.
- **Damage/dirty regions on Linux:** nothing ffmpeg exposes. X11 has the DAMAGE extension
  (`xcbgrab` does not surface it); Wayland/PipeWire has no per-region damage in the portal API.
- ⇒ **Linux verdict: stay on `x11grab` (Macast already returns `None` without `$DISPLAY`, which is
  the honest behaviour). Wayland support is a separate, much larger project.**

---

## 5. Q5 — Content-adaptive encoding for screen content

### 5.1 x264 has **no** screen-content tune — complete list from source

`x264 common/base.c:611 param_apply_tune()` — the entire tune vocabulary is:
**`film, animation, grain, stillimage, psnr, ssim, fastdecode, zerolatency, touhou`**.
(mirror HEAD 2024-05-12; x264 upstream is near-frozen, so 2026 has not added one.)

Exact mutations:
- `stillimage` → `deblock -3:-3`, `psy_rd 2.0`, `psy_trellis 0.7`, `aq_strength 1.2`.
  This is a **psy** tuning — it biases toward perceptual crispness of still detail, it is *not*
  screen-content coding. Because only one psy tuning may be active, `-tune stillimage,zerolatency`
  **is legal** and worth an A/B on text-heavy desktops.
- `zerolatency` → `rc.i_lookahead = 0`, `i_sync_lookahead = 0`, `i_bframe = 0`,
  `b_sliced_threads = 1`, **`b_vfr_input = 0`**, `rc.b_mb_tree = 0`.
- `fastdecode` → no deblock, CAVLC, no weighted prediction.
- `grain` → keeps the noise floor (wrong for a desktop; do not use).

⚠ **`b_vfr_input = 0` matters:** x264's zerolatency tune assumes **constant frame rate** input. If
you move to change-driven VFR you should either drop `-tune zerolatency` and set the individual
params yourself (omitting `b_vfr_input=0`), or feed x264 explicit PTS via `-fps_mode passthrough`.

### 5.2 Screen-content coding tools

- **H.264:** no SCC extensions exist in the standard. The only relevant tool is
  `-x264-params ict=1` (intra-chroma transform) — marginal for RGB-ish desktop content, and it is
  not in the tune table.
- **HEVC SCC extensions** (IBC / palette mode / adaptive colour transform): real in the standard,
  but **no consumer hardware encoder implements them** — `hevc_videotoolbox`, `hevc_nvenc`,
  `hevc_qsv`, `hevc_amf` expose no SCC option in FFmpeg. `libx265` is the only software path and I
  could not reach its `param.cpp` to confirm (`mirror/x265` 404s; canonical upstream is on
  Bitbucket). **[unverified]**
- ⇒ **Practical answer: hardware HEVC-SCC does not exist for a Cast sender. Do not plan around it.**

### 5.3 Variable frame rate on demand — **viable, and Macast's Cast path is already tolerant**

The decisive fact is in Macast's own sink (`screen_mirror.py:5850`):
```python
timestamp = int((now - self._t0) * VIDEO_CLOCK) & 0xFFFFFFFF
```
The RTP timestamp is derived from **wall-clock at send time**, not from a 1/fps cadence, and the
output is `-f h264 pipe:1` (`OUTPUTS['caststream']`, line 294) — a raw Annex-B stream with **no
container timestamps at all**. So:
- the receiver plays out against absolute wall-clock-derived timestamps;
- skipping a frame when nothing changed does not desynchronise anything;
- `referenced_frame_id = frame_id - 1` for non-key frames (line 5853) keeps a strictly linear P-chain,
  which VFR preserves as long as `frame_id` increments only for frames actually sent.

Protocol side: `max_frame_rate` is a **`SimpleFraction` ceiling**, not a fixed rate —
`cast/streaming/public/offer_messages.h` line 32: *"be set to `kDefaultMaxFrameRate`"* with
`kDefaultMaxFrameRate = 30`; `sender_session.cc:226` only validates
`config.max_frame_rate.is_positive()`. Nothing in openscreen requires a frame every 1/30 s.

Implementation side, **zero new dependencies**: ffmpeg already ships `mpdecimate`
(`doc/filters.texi`): *"Drop frames that do not differ greatly from the previous frame in order to
reduce frame rate."* Options `hi` (default `64*12`), `lo` (default `64*5`), `frac` (default `0.33`),
`max`, `keep`, `mode`. Combined with **`-fps_mode vfr`** (or `passthrough`) — confirmed in
`doc/ffmpeg.texi`:
> `passthrough` — *"Each frame is passed with its timestamp from the demuxer to the muxer."*
> `vfr` — *"Frames are passed through with their timestamp or dropped so as to prevent 2 frames from
> having the same timestamp."*
> `cfr` — *"Frames will be duplicated and dropped to achieve exactly the requested constant frame rate."*
> `auto` — *"Chooses between cfr and vfr depending on muxer capabilities. This is the default."*

Caveats: `mpdecimate` still runs the differ on every captured frame, so it saves **encoder and
network** work, not capture work. The real saving on capture comes from SCK's `SCFrameStatusIdle`
(§4.3), where the *system* tells you nothing changed and no pixels move at all.
Also: `TARGET_DELAY_MS = 200` and the receiver's playout clock assume a steady stream; a long static
gap followed by a burst is exactly what Chrome's `low_latency_mode_` logic handles by dropping
rather than buffering (§6).

**What real screen-share implementations do when static:**
- **Safari/WebKit (WebRTC getDisplayMedia):** drops `SCFrameStatusIdle` buffers and emits nothing
  (§4.4). Truly variable.
- **Chrome Cast mirroring:** fixed capture rate, but `VideoSender::ShouldDropNextFrame()` drops
  under congestion rather than growing the buffer (§6).
- **OBS:** fixed fps by design (`setMinimumFrameInterval` from the output fps), no idle skipping.
- **[unverified]** Parsec's and Sunshine's exact static-frame policy — I read Sunshine's `video.cpp`
  (3,749 lines) and `stream.cpp` (2,384 lines) for FEC but found no static-content / frame-skipping
  logic; Sunshine appears to encode every captured frame.

---

## 6. Q6 — RTP packetization, FEC and retransmission

### 6.1 **Macast's "no retransmission by design" is contradicted by the protocol it implements**

`chromium/openscreen` `cast/streaming/impl/sender_impl.cc` contains
`OnReceiverIsMissingPackets(std::vector<PacketNack> nacks)` — a full **NACK-driven retransmission
path**, gated by an RTT-based threshold (*"a Receiver's NACK may have been issued while the needed
packet was in-flight"*), counting `retransmitted_count`, logging `"RETRANSMITTING frame …"`.
⇒ Cast Streaming **does** have retransmission. Macast omitting it is an implementation choice, not a
protocol limit. On a LAN with ~1 ms RTT the feedback loop is nearly free.

> **已落地（2026-10-02，plugin v0.19）。** 这一节是研究当时的结论，那时 Macast 确实没有重传路径，
> 而且 AGENTS.md 里还写着"没有重传路径（设计如此）"—— 那句话就是被本节证伪的。现在
> `_retransmit` 按 openscreen 的规则实现了：staleness 判定（发出不到一个 RTT 的包不算丢）、
> `ALL_PACKETS_LOST = 0xffff` 时不读位图、否则第 i 位（LSB 起）补 `packet_id + 1 + i`、
> 修复缓冲 8 MiB（最老的先换成只留元数据的空壳）、一次反馈上限 64 包（**故意不转写 openscreen
> 的 router 节流**，用这个上限夹住）。补发复用原密文、只换 RTP 序号。红线在 AGENTS.md §4.8 的 ⑧，
> 回归用例在 Part 24（含真 UDP socket 上的一条）。**仍未在任何真电视上验证过** ——
> 本机局域网没有 Chromecast，Part 24 的假设备与发送端共用同一张字段表，只能证明自洽（§4.9）。
> §6.2 的时长窗口同样已落地（v0.18）。原文保留不动，因为可复查的是它的取证过程
> （读的是 openscreen 的哪一处、哪一句），不是它的结论。

### 6.2 The in-flight limit is **duration-based**, and 12 frames is 10× tighter than the protocol allows

`sender_impl.cc GetMaxInFlightMediaDuration()`:
```cpp
max_in_flight = max(kMinSenderInFlight, target_playout_delay_ / 3);
return clamp(round_trip_time_ * 2, kMinSenderInFlight, max_in_flight);
```
> *"The Sender keeps only enough media in-flight to drive the loss-detection and retransmit feedback
> loop, which takes on the order of two network round-trips (one to detect a loss via NACK, one to
> retransmit)… capped at a third of the playout delay window so that the majority of the budget is
> reserved for the Receiver, which needs buffer to absorb NACK retransmissions. Bounding the Sender
> this way also makes it drop frames earlier during congestion (saving bandwidth and CPU)."*

`cast/streaming/public/constants.h`:
```cpp
inline constexpr std::chrono::milliseconds kDefaultTargetPlayoutDelay(400);
inline constexpr int kDefaultCastStreamingPort = 2344;
inline constexpr int kDefaultCastPort         = 8010;
inline constexpr int kMaxUnackedFrames        = 120;   // "fits in the 8-bits range for frame IDs"
inline constexpr int kRequiredNetworkPacketSize = 256;
inline constexpr int kDefaultFrameRate        = 30;
inline constexpr int kDefaultVideoMinBitRate  = 300 * 1000;
inline constexpr int kDefaultVideoMaxBitRate  = 10 * 1000 * 1000;   // <-- 10 Mbps
inline constexpr int kDefaultAudioMinBitRate  = 32 * 1000;
inline constexpr int kDefaultAudioMaxBitRate  = 256 * 1000;
inline constexpr std::chrono::milliseconds kDefaultMaxDelayMs(1500);
```
and `cast/streaming/public/capture_recommendations.h`:
```cpp
// "Default maximum delay for both audio and video. Used if the sender fails to provide any constraints."
inline constexpr std::chrono::milliseconds kDefaultMaxDelayMs(400);
```

⇒ **Three concrete corrections available to Macast:**

| Macast today | Protocol | Comment |
|---|---|---|
| `MAX_IN_FLIGHT_FRAMES = 12` (line 5496) | `2 × RTT`, clamped to `[kMinSenderInFlight, target_playout_delay/3]`; hard cap `kMaxUnackedFrames = 120` | On a LAN with 1 ms RTT, `2×RTT` is tiny — the *floor* is what binds. Either way 12 frames is arbitrary and ~10× below the protocol's hard cap. Exceeding the frame-ID span returns `REACHED_ID_SPAN_LIMIT`. |
| `CAST_STREAM_MAX_BITRATE = 4500000` (line 5510) | `kDefaultVideoMaxBitRate = 10 Mbps`; Chrome's own sender defaults to `kStartVideoBitrate = 5000000`, `GetMaxVideoBitrate() = 5 Mbps`, min `300000` | The 4.5 Mbps cap is *below* Google's own 5 Mbps default, and the comment attributes it to a crypto limit that §1 disproves by ~7,900×. **Raise to at least 8–10 Mbps for 1080p once CommonCrypto lands.** |
| `TARGET_DELAY_MS = 200` (line 5486) | `kDefaultTargetPlayoutDelay = 400`; `capture_recommendations::kDefaultMaxDelayMs = 400`; ANSWER's `maxDelay` defaults to **1200 ms** if omitted; protocol example offers use `"targetDelay": 400` | 200 ms is 2× more aggressive than Google's default and legal, but it leaves the receiver very little NACK-absorption buffer. If you add retransmission, consider 300–400 ms; if you don't, 200 ms is fine and is a genuine latency win. |

Also: `kRequiredNetworkPacketSize = 256`, `kMaxRtpPacketSizeForIpv4UdpOnEthernet = 1500-20-8`.

### 6.3 Keyframes: on-demand, never periodic

Google's reference sender sets `gop_size = keyint_min = 999999` — *"Disable periodic keyframes;
on-demand by Cast."* `sender_impl.cc NeedsKeyFrame()` is
`last_enqueued_key_frame_id_ <= picture_lost_at_frame_id_`, i.e. **PLI-driven IDR**.

Macast's software path instead forces `keyint=FPS:min_keyint=FPS:scenecut=0` (a 1-second GOP,
line 1630) and the VT path gets no GOP override at all. A 1 s GOP is a **~3–5% bitrate tax** and it
is only there to make the PLI path survivable. With NACK retransmission (or even just a
PLI-triggered `-forced_idr`), the periodic keyframe can go away.

Chrome's PLI throttling for reference (`media/cast/sender/video_sender.cc`):
`kMinKeyFrameRequestInterval = 500 ms`, `kMinKeyFrameRequestFrameInterval = 6` frames.

### 6.4 Adaptive bitrate — Chrome does have one, and it is simple

`media/cast/sender/video_bitrate_suggester.cc`: exponential increase/decrease factors
(`kCastStreamingExponentialVideoBitrateAlgorithm{Increase,Decrease}Factor`) and the rule
**"If more than 2% of frames were dropped, decrease the bitrate."** Applied with **5% hysteresis**
(`kBitrateThreshold = 0.05` — *"To avoid thrashing the encoder, which can cause dropped frames, only
update the encoder if the suggested bitrate has changed by a significant amount"*).
Sender targets **`kTargetUtilizationPercentage = 75`** of the measured link.

Playout-delay adaptation (`media/cast/sender/video_sender.cc`):
```cpp
constexpr int kRoundTripsNeeded = 4;
constexpr int kConstantTimeMs   = 75;
...
base::TimeDelta new_target_delay = std::min(
    frame_sender_->CurrentRoundTripTime() * kRoundTripsNeeded + base::Milliseconds(kConstantTimeMs),
    max_playout_delay_);
// "In case of low latency mode, we prefer frame drops over increasing playout time."
if (!low_latency_mode_ && new_target_delay > frame_sender_->TargetPlayoutDelay()) { ... }
```
`low_latency_mode_` flips on when input events are flowing (interactive use); on entering it, Chrome
pins the playout delay to `min_playout_delay_` and prefers dropping frames to buffering.
⇒ **The right LAN answer is: drop frames + adapt bitrate + keep playout delay small.** Which is
broadly what Macast does — but Macast's drop trigger is a *count* (12 in-flight frames) rather than
a *duration* (2×RTT), and it has no bitrate adaptation at all.

### 6.5 Sunshine / Moonlight: **FEC, not ARQ**

Sunshine (`LizardByte/Sunshine`) — `src/stream.cpp`, `namespace fec`:
```cpp
static fec_t encode(std::string_view payload, size_t blocksize, size_t fecpercentage,
                    size_t minparityshards, size_t prefixsize) {
  auto data_shards = payload_size / blocksize;
  auto parity_shards = (data_shards * fecpercentage + 99) / 100;
  // "increase the FEC percentage for this frame if the parity shard minimum is not met"
  if (parity_shards < minparityshards && fecpercentage != 0) { parity_shards = minparityshards; ... }
  rs_t rs { reed_solomon_new((int)data_shards, (int)parity_shards) };
  reed_solomon_encode(rs.get(), shards_p.begin(), (int)nr_shards, (int)blocksize);
}
```
with `fecPercentage = config::stream.fec_percentage`, `blocksize = packetsize + MAX_RTP_HEADER_SIZE`,
`DATA_SHARDS_MAX`, and `MAX_FEC_BLOCKS = 4` (*"There are 2 bits for FEC block count"*).
Sunshine also emits `audio_fec_packet_t` / `AUDIO_FEC_HEADER` for audio, and logs
`"Network: each FEC block latency"`.

Moonlight (`moonlight-stream/moonlight-common-c`) — `src/RtpVideoQueue.c` is the receiver half:
per-frame Reed-Solomon recovery (`reed_solomon_new(bufferDataPackets, bufferParityPackets)`,
`LC_ASSERT(queue->missingPackets <= queue->bufferParityPackets)`), multi-FEC-block support gated on
`APP_VERSION_AT_LEAST(7,1,431)`, and **telemetry back to the sender**:
```c
static void reportFinalFrameFecStatus(PRTP_VIDEO_QUEUE queue) {
  fecStatus.missingPacketsBeforeHighestReceived = BE16(queue->missingPackets);
  fecStatus.totalDataPackets   = BE16(queue->bufferDataPackets);
  fecStatus.totalParityPackets = BE16(queue->bufferParityPackets);
  fecStatus.fecPercentage      = (uint8_t)queue->fecPercentage;
  ...
  connectionSendFrameFecStatus(&fecStatus);   // sender tunes fec_percentage from this
}
```
**And there is no retransmission at all** — `VideoDepacketizer.c` calls `LiRequestIdrFrame()` at six
sites on unrecoverable loss. So the GameStream model is: **proactive RS-FEC sized by a closed feedback
loop, plus IDR-on-total-loss.** Never ARQ.

⇒ **For LAN screen sharing the honest trade:** FEC costs a fixed ~20% bandwidth overhead (Sunshine's
default) and *zero* extra latency; NACK/ARQ costs ~2×RTT extra latency on loss and *zero* bandwidth
when the network is clean. On a wired/clean-WiFi LAN ARQ wins; on lossy 2.4 GHz WiFi FEC wins.
**Cast gives you ARQ for free (it is in the protocol, §6.1); implementing RS-FEC would mean writing
a Galois-field encoder in Python, which is a real cost.** Recommended order: (1) implement NACK
retransmission, (2) make the in-flight window duration-based, (3) only then consider FEC.

> **进度（2026-10-02）**：(1) 与 (2) 都已落地（v0.19 / v0.18）。(3) RS-FEC **没做，也不建议现在做**：
> 在 Python 里写伽罗华域编码器的代价，只有在 2.4 GHz 那种丢包链路上才换得回来，而要不要付这个代价
> 现在是个**可以量的问题**了 —— 设置页「统计信息」上的 `补发` 就是那条链路的丢包率读数
> （`补发` 非零而 `丢块` 为零 = 无线在丢包）。先拿到真电视上的这个数，再谈 FEC。
> 上面那句 "there is no retransmission at all" 说的是 **Moonlight/GameStream 的模型**（RS-FEC + IDR，
> 从不 ARQ），不是 Macast，别读成对我们的描述。

### 6.6 Jitter buffer latency

| endpoint | delay budget | source |
|---|---|---|
| Chromecast (Cast mirroring) | `targetDelay` offered by sender; `maxDelay` **defaults to 1200 ms** if the ANSWER omits it; protocol examples use `"targetDelay": 400`; openscreen default `kDefaultTargetPlayoutDelay = 400 ms`; Chrome's adaptive value `min(4×RTT + 75 ms, max_playout_delay)` | `streaming_session_protocol.md`, `constants.h`, `video_sender.cc` |
| Macast today | `TARGET_DELAY_MS = 200` | `screen_mirror.py:5486` |
| Browser MSE player | **not verifiable from primary sources here** — MSE has no defined jitter buffer; playout follows `MediaSource`'s `timestampOffset` + the element's own buffering heuristics, which are implementation-defined | [unverified] |
| WebRTC | `playoutDelayHint` / RTP `abs-capture-time` + `playout-delay` header extension; Chrome's typical target is 0–~200 ms for interactive | [unverified — could not reach WebRTC docs] |

⇒ Macast's 200 ms is **more aggressive than Google's own 400 ms default** and comfortably inside the
protocol. It is a genuine latency advantage, but it is also why the sender has no room to absorb
NACK retransmissions — the openscreen comment is explicit that the receiver needs *"buffer to absorb
NACK retransmissions"*, which is why the sender's own window is capped at `target_playout_delay/3`.
If you add retransmission, expect to move `TARGET_DELAY_MS` to ~300–400 ms or accept a higher
frame-drop rate.

---

## 7. What could **not** be verified

**Network reachability from this host** (all failed, so the following rest on GitHub mirrors or are
open):
- `developers.google.com`, `support.google.com`, `chromecast.google.com`, `developer.android.com`
  → `curl: (28) Connection timed out`. So **no first-party Google Cast device capability table**, and
  no `developers.google.com/cast/docs/media` codec list. Everything Cast-related above comes from
  `chromium/openscreen` and `chromium/chromium` via `gh api`.
- `en.wikipedia.org`, `reddit.com`, `news.ycombinator.com`, `duckduckgo.com`, `brave.com`,
  `startpage.com`, `r.jina.ai`, `archive.org`, `raw.githubusercontent.com`, `gist.github.com`
  → all unreachable. `bitmovin.com`, `stackoverflow.com`, `flatpanelshd.com` → HTTP 403.
  `mojeek.com` → 403 *"your network appears to be sending automated queries"*.
- The documented local Clash proxy on `127.0.0.1:9565` / `:7890` → `curl: (35) LibreSSL SSL_connect:
  SSL_ERROR_SYSCALL`; `:8888` → *"Couldn't connect"*.
- Workarounds that **did** work: `gh api repos/<o>/<r>/contents/<path> --jq '.content' | base64 -d`
  for all GitHub source; `curl https://developer.apple.com/documentation/<path>.md` for Apple (the
  HTML pages are JS-only and useless); `learn.microsoft.com` for most Win32 docs; `docs.nvidia.com`;
  `pypi.org/pypi/<pkg>/json`; `WebSearch` for snippets only.

**Specific gaps:**
1. **bcrypt.dll and libcrypto ctypes paths are signature-verified against primary docs but
   never executed or benchmarked** — a Windows host became reachable on 2026-10-02 (it carried
   the capture measurements in §4.6) but no cipher benchmark has been run there, and there is
   still no Linux host. All throughput numbers are macOS only.
2. **The minimum Windows version for `BCRYPT_CHAIN_MODE_CTR`** — the authoritative Microsoft CNG
   property-identifiers page 404s from here. Widely reported as Windows 8 / Server 2012. Must be
   probed at runtime (`BCryptSetProperty` → `STATUS_NOT_SUPPORTED`).
3. **AES-NI / ARMv8 Crypto Extension usage** — inferred from throughput (10.5 GB/s single-threaded),
   not confirmed by disassembly or by any vendor statement I could reach.
4. **CommonCrypto CTR counter width** — *was* verified (§1.1: low-64-bit increment, high-64-bit fixed
   nonce), but only against LibreSSL 3.6.4 on macOS 15-era Apple Silicon. Older macOS / Intel Macs
   were not tested.
5. **HEVC SCC in `libx265`** — `mirror/x265` 404s and canonical x265 is on Bitbucket, unreachable.
6. **Chromecast-with-Google-TV HEVC/AV1 support by generation** — no reachable primary source. The
   negotiation approach in §3.3 makes this moot, which is the point.
7. **DLNA HEVC-in-MPEG-TS reality** — no reachable primary source.
8. **WGC-vs-gdigrab latency/fps numbers**, **PipeWire portal latency numbers**,
   **measured SCK-vs-avfoundation latency** — none measured. A real Windows host is now
   available (§4.6 carries its attach/output measurements) but no *latency* comparison was run
   there; PipeWire and SCK still need hardware not available. The SCK *capability* claims are
   from Apple docs and from OBS/WebKit source, but I have **no measured latency delta** between
   `avfoundation` and ScreenCaptureKit. That number is the one I would most want before
   committing to the SCK rewrite.
9. **Browser MSE and WebRTC jitter-buffer latencies** — no primary source reached.
10. **Sunshine/Parsec static-content frame-skipping policy** — searched, not found; Sunshine appears
    to encode every frame, Parsec is closed source.
11. `SCStreamConfiguration`'s own availability block was fetched, but I did not separately confirm
    every property's minimum macOS version — only `capturesAudio` (**macOS 13.0.0**, confirmed both
    from Apple's availability metadata and from OBS's `@available(macOS 13.0, *)` guard).

---

## 8. Source index

**Local measurement scripts (in `/tmp`, not in the repo):** `cc_bench.py`, `vmaf.py`, `vtlat.py`;
raw output `vmaf_out.txt`, `vtlat_out.txt`. Scratch YUV (`/tmp/lat2`, `/tmp/lat3`, 8.9 GB) deleted.

**Apple:**
- <https://developer.apple.com/documentation/ScreenCaptureKit/SCStreamConfiguration/capturesAudio.md> (`macOS: 13.0.0 -`)
- <https://developer.apple.com/documentation/ScreenCaptureKit/SCStreamConfiguration/minimumFrameInterval.md>
- <https://developer.apple.com/documentation/ScreenCaptureKit/SCStreamConfiguration/queueDepth.md>
- <https://developer.apple.com/documentation/ScreenCaptureKit/SCStreamFrameInfo/dirtyRects.md>
- <https://developer.apple.com/documentation/ScreenCaptureKit/scframestatus.md>
- <https://developer.apple.com/documentation/ScreenCaptureKit/scstreamoutputtype.md>

**Microsoft:**
- <https://learn.microsoft.com/en-us/windows/win32/api/bcrypt/nf-bcrypt-bcryptencrypt>
- <https://learn.microsoft.com/en-us/windows/win32/api/bcrypt/nf-bcrypt-bcryptsetproperty>

**Google / Cast (via GitHub mirrors):**
- `chromium/openscreen`: `cast/streaming/impl/streaming_session_protocol.md`,
  `cast/streaming/public/constants.h`, `cast/streaming/public/capture_recommendations.h`,
  `cast/streaming/public/offer_messages.h`, `cast/streaming/public/receiver_session.cc`,
  `cast/streaming/public/sender_session.cc`, `cast/streaming/public/session_config.h`,
  `cast/streaming/impl/sender_impl.cc`,
  `cast/standalone_sender/streaming_ffmpeg_encoder.cc`, `cast/standalone_sender/constants.h`
- `chromium/chromium`: `components/mirroring/service/README.md`, `mirror_settings.cc`,
  `openscreen_session_host.cc`, `video_capture_client.cc`,
  `media/cast/sender/video_sender.cc`, `media/cast/sender/video_bitrate_suggester.cc`

**FFmpeg:** `libavcodec/allcodecs.c`, `videotoolboxenc.c`, `nvenc.c`, `nvenc.h`, `nvenc_h264.c`,
`qsvenc.c`, `amfenc_h264.c`, `amfenc.h`, `libavdevice/` (listing), `libavfilter/vsrc_ddagrab.c`,
`doc/indevs.texi`, `doc/filters.texi` (mpdecimate), `doc/ffmpeg.texi` (fps_mode).

**Other:** `mirror/x264` `common/base.c:611`; `obsproject/obs-studio`
`plugins/mac-capture/mac-sck-video-capture.m`, `plugins/linux-pipewire/screencast-portal.c`,
`plugins/win-capture/game-capture.c`; `WebKit/WebKit`
`Source/WebCore/platform/mediastream/cocoa/ScreenCaptureKitCaptureSource.mm`;
`ronaldoussoren/pyobjc` `pyobjc-framework-ScreenCaptureKit/Lib/ScreenCaptureKit/_metadata.py`;
`LizardByte/Sunshine` `src/stream.cpp`; `moonlight-stream/moonlight-common-c`
`src/RtpVideoQueue.c`, `src/VideoDepacketizer.c`, `src/Video.h`;
`openssl/openssl` `doc/man3/EVP_aes_128_gcm.pod`; `wine-mirror/wine` `include/bcrypt.h`;
`user-unfold/HLPlayer` `src/crypto/AesCtr256.h`; pypi.org JSON for
`cryptography`, `pycryptodome`, `pynacl`, `pyopenssl`, `oscrypto`,
`pyobjc-framework-ScreenCaptureKit`.

**Macast files read (never modified):**
`/Users/pavia/githome/Macast/macast/plugins/renderer/screen_mirror.py`,
`/Users/pavia/githome/Macast/requirements/common.txt`,
`/Users/pavia/githome/Macast/requirements/darwin.txt`,
`/Users/pavia/githome/Macast/scripts/setup_py2app.py`,
`/Users/pavia/githome/Macast/scripts/build_macos_arm.sh`,
`/Users/pavia/githome/Macast/.github/workflows/build.yml`.
