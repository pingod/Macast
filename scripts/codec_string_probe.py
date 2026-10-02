"""What codec string does each shipped shape *actually* emit?

The browser target used to hand MediaSource `avc1.640028` unconditionally, i.e.
High profile, level 4.0, no constraints. That is a claim about bytes the encoder
produces, and the encoder's level is decided by the picture and the rate control
-- not by that string. This probe reads the `avcC` box out of a real init
segment for each shape and prints what the stream says.

Run 2026-10-02 on macOS (ffmpeg 8.x, testsrc2, the argv below), and 2026-10-03
on the Windows box (ffmpeg 8.1.2 gyan.dev full build) -- the x264 rows came out
identical on both, which is worth knowing before quoting any of them:

    x264   1920x1080 3M/6M    avc1.42c028   avc1.42c028
    x264   3840x2160 6M/8M    avc1.42c033   avc1.42c033
    x264   1280x720  2M       avc1.42c01f
    VT     1920x1080 -level 42   avc1.64002a
    VT     3840x2160 -level 51   avc1.640033
    VT     1920x1080 no level    avc1.640028   <- the only row the claim fitted
    VT     3840x2160 no level    avc1.640033
    NVENC  1920x1080 6M       avc1.640028   (SPS 67640028ac2b200f)
    NVENC  3840x2160 9M       avc1.640033   (SPS 67640033ac2b2007)

x264 was *told* `high` and wrote 42c0 (Constrained Baseline) anyway, while NVENC
honoured `high` (0x64) and set the level from the picture. So the four bytes are
a property of the encoder, its level pin, and the size -- not of the argv, and
not of any one of the three. NVENC 1080p landing on `avc1.640028` is a coincidence
with the pinned default, and the same encoder at 2160p says `avc1.640033`: that
pair is the argument for reading the box off the wire (`avc_codec_string`) and
keeping the pinned default only for the window before the header exists.

The argv here is written by hand, next to the product's `encoder_args()`: if the
two ever diverge this probe measures a pipeline that does not ship, so re-check
that table against `encoder_args` before quoting a row.

Pass another ffmpeg as argv[1] to run it on a different machine. The encoder
branches are gated by `sys.platform`, not by what the binary offers: the VT rows
only mean something on macOS and the NVENC rows only on Windows, and each prints
an explicit skip line off its platform rather than a `??????` that reads like a
broken probe. So the full table above needs two runs, one per machine:

    python scripts/codec_string_probe.py C:\\path\\to\\ffmpeg.exe
"""
import subprocess
import sys

FF = sys.argv[1] if len(sys.argv) > 1 else '/opt/homebrew/opt/ffmpeg/bin/ffmpeg'


def avc1(data):
    i = data.find(b'avcC')
    if i < 0:
        return None
    p, c, l = data[i + 5:i + 8]
    return 'avc1.%02x%02x%02x' % (p, c, l)


def sps(data):
    """The first SPS, for cross-checking the three bytes `avc1` prints.

    `avcC` starts its record four bytes after the type field: version, profile,
    compatibility, level, then lengthSizeMinusOne, then numSPS.
    """
    i = data.find(b'avcC')
    if i < 0 or data[i + 4] != 1:
        return ''
    n = data[i + 9] & 0x1f
    ln = (data[i + 10] << 8) | data[i + 11]
    if n != 1:
        # The offsets below read one SPS; more than one is not this shape.
        return ''
    return data[i + 12:i + 12 + ln][:8].hex()


def run(tag, extra, size, level_args=()):
    cmd = [FF, '-hide_banner', '-loglevel', 'error', '-f', 'lavfi',
           '-i', 'testsrc2=size=%s:rate=24' % size, '-t', '1'] + extra + list(
        level_args) + [
        '-f', 'mp4', '-movflags',
        'frag_every_frame+empty_moov+default_base_moof', '-']
    out = subprocess.run(cmd, stdout=subprocess.PIPE,
                         stderr=subprocess.PIPE).stdout
    print('%-22s %s   avcC=%s' % (tag, avc1(out) or '??????',
                                  sps(out)))
    return avc1(out)


X264 = ['-c:v', 'libx264', '-preset', 'ultrafast', '-tune', 'zerolatency',
        '-profile:v', 'high', '-flags', '+low_delay', '-thread_type', 'slice',
        '-pix_fmt', 'yuv420p', '-r', '24', '-g', '12']
VT = ['-c:v', 'h264_videotoolbox', '-profile:v', 'high', '-flags',
      '+low_delay', '-realtime', '1', '-pix_fmt', 'yuv420p', '-r', '24',
      '-g', '12']
NV = ['-c:v', 'h264_nvenc', '-preset', 'p1', '-tune', 'll', '-rc', 'vbr',
      '-profile:v', 'high', '-pix_fmt', 'nv12', '-r', '24', '-g', '12']


def _profile(args, name):
    """`args` with its `-profile:v` value swapped, not a dangling option."""
    out = list(args)
    out[out.index('-profile:v') + 1] = name
    return out


print('the pinned fallback:    avc1.640028  (High, no constraint, level 4.0)')
for bits, size in ((3_000_000, '1920x1080'), (6_000_000, '1920x1080'),
                   (6_000_000, '3840x2160'), (8_000_000, '3840x2160'),
                   (2_000_000, '1280x720')):
    a = X264 + ['-b:v', str(bits), '-maxrate', str(bits * 3 // 2),
               '-bufsize', str(bits // 2)]
    run('x264 %s %dM' % (size, bits // 1_000_000), a, size)
if sys.platform == 'darwin':
    for bits, size in ((6_000_000, '1920x1080'), (9_000_000, '3840x2160')):
        run('VT   %s %dM' % (size, bits // 1_000_000),
            VT + ['-level', '51' if '2160' in size else '42',
                  '-b:v', str(bits)],
            size)
    run('VT   1920x1080 no level', VT + ['-b:v', '6000000'], '1920x1080')
    run('VT   3840x2160 no level', VT + ['-b:v', '9000000'], '3840x2160')
else:
    print('VT rows skipped: not macOS (they would print `??????`, which is a '
          'missing encoder, not a broken probe)')
# Does the profile pin change anything? It changes the request, not the answer:
# the row above is x264 told `high`, and this one the same encoder told
# `baseline`, so a difference here would mean the string can be inferred from
# the argv. It cannot, which is what `avc_codec_string` is for.
run('x264 told baseline 1080p',
    _profile(X264, 'baseline') + ['-b:v', '6000000'], '1920x1080')
if sys.platform == 'win32':
    # Both pictures, because the level is a property of the picture: the 1080p
    # row happens to match the pinned default, the 2160p one does not, and that
    # pair is the whole argument for reading the box.
    run('nvenc 1920x1080', NV + ['-b:v', '6000000'], '1920x1080')
    run('nvenc 3840x2160', NV + ['-b:v', '9000000'], '3840x2160')
else:
    print('nvenc rows skipped: this machine has no NVENC to read')

