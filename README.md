
# youtube_video_storage

## Forewarnings
This mainly highly experimental and not likely to be maintained, just a short project to test feasibility. So mainy things are likely to change or added/removed

## What's this for?

YouTube supports many video formats such as .MP4, .MOV, and .AVI just to name a few, all of which can be uploaded to the platform of any length without cost to the uploader.

YouTube recommends that a standard 1920x1080 MP4 file that plays on the platform, has a bitrate at 10Mbp/s for framerates at 30FPS or lower, while higher framerates like 60FPS need 15Mbp/s to meet recommendations.

This is how it breaksdown for videos using HDR: [(Why HDR?)](#why-hdr)

| Resolution    | Rec. Bitrate(30FPS)   | Rec. Bitrate(60FPS)   |Max. MB/sec|
| ------------- |:---------------------:|:---------------------:|:---------:|
| 720p          | 6.5 Mbp/s             | 9.5 Mbp/s             | 1.1875 MB |
| 1080p         | 10 Mbp/s              | 15 Mbp/s              | 1.875 MB  |
| 1440p         | 20 Mbp/s              | 30 Mbp/s              | 3.75 MB   |
| 4k            | 50 Mbp/s              | 75 Mbp/s              | 9.375 MB  |

More information from Google [here](https://support.google.com/youtube/answer/1722171?sjid=17386503199810077283-NA#zippy=%2Cbitrate) on recommended bitrates.

## Why HDR

HDR stands for High Dyanmic Range, which is used for media formats to encode a higher "range" of colors and luminosities dependent on the format to allow for greater fidelity than that of SDR formats.

YouTube supports such formats by allowing uploads in HEVC/H.265(an extension of H.264/MP4 videos). This broadens the allowable bitrate streaming that YouTube could provide on the player and reduced compression artifacts by allowing so. This is most useful when trying to maximize storage capabilities of the videos by extending the color range and leaving more bitrate to play with.

## Using CRC

CRC(Cyclc Redundancy Check) allows for verifying and help with troubleshooting corrupted information, its usage here is implemented in the form of CRC-24/OPENPGP. Each column's CRC is stored in the last row of the frame. CRC-24 *detects* corruption and the decoder reports exactly which columns failed. Repair is done by Reed-Solomon (see below), which uses those bad-column reports as known erasure positions.

## Reed-Solomon error correction

The payload is split into blocks of `255 - ecc` bytes, each given `ecc` parity bytes (default 32, set with `--ecc`, `0` disables). A block repairs up to `ecc/2` bad bytes at unknown positions, or up to `ecc` bad bytes at known positions (columns that fail their CRC-24). Codewords are byte-interleaved across the frame so a damaged row, column or smear is spread thinly over many blocks. The header is also RS-protected (6 bad bytes repairable), and a payload CRC-32 gives the final verdict. Cost: with the default, about 12.5% of capacity.

A later revision of the CRC may ultilize CRC-32, having the Alpha channel in an RGBA pixel store its signature as a [<u8>, <u8>, <u8>, <u8>] vector. The u32 from the CRC-32 will be modified in little-endian form.

## Acknowledgements

 - [![Rust][Rust-lang.org]][Rust-url]
 - [https://github.com/image-rs/image]
 - [https://github.com/mrhooray/crc-rs]

## Checklist

Basic proof of concept as:
 - [x] Grayscale image
 - [x] Grayscale image scaler
 - [x] Grayscale image decoder
 - [x] RGB image
 - [x] RGB image scaler
 - [x] RGB image decoder
 - [x] CRC checked image(RGB only)

Other features:
 - [x] Data overflow prevention
 - [x] Reed-Solomon error correction
 - [x] Sequenced image of data
 - [x] Image to video generation
 - [x] FFmpeg sequencer
 - [ ] Other scaling algos
 - [ ] Some GUI integration through Rust

<!-- Links & Images -->
[Rust-lang.org]: https://www.rust-lang.org/static/images/rust-logo-blk.svg
[Rust-url]: https://www.rust-lang.org

## Surviving lossy video compression

YouTube re-encodes everything, so exact 0-255 pixel values do not survive. Frames can instead draw each symbol as one of 2, 4, 16 or 256 evenly spaced intensity levels (`--levels`); the decoder averages the centre of each scaled block and rounds to the nearest level. The header is always drawn black/white, and the per-column CRC row is only used at 256 levels (it can't survive quantization).

`scripts/ffmpeg_roundtrip.py` measures this: it encodes random payloads, re-encodes them with ffmpeg (libx264/libx265 at fixed bitrates), decodes, and compares. Results at 1920x1080, every video frame new data, ECC 32, 8 frames per config (ok = frames recovered exactly):

| Setup (gray unless noted)         | Bytes/frame | x264 8M | x264 3M | x265 8M | x265 3M |
|-----------------------------------|:-----------:|:-------:|:-------:|:-------:|:-------:|
| 256 levels, any block size        | 28-338 K    | 0/8     | 0/8     | 0/8     | 0/8     |
| 16 levels, 4px blocks             | 56 K        | 8/8     | 0/8     | 1/8     | 0/8     |
| 4 levels, 8px blocks              | 7 K         | 8/8     | 8/8     | 8/8     | 8/8     |
| 4 levels, 4px blocks              | 28 K        | 8/8     | 8/8     | 8/8     | 1/8     |
| **2 levels, 4px blocks**          | **14 K**    | 8/8     | 8/8     | 8/8     | 8/8     |
| 2 levels, 2px blocks              | 57 K        | 6/8     | 6/8     | -       | 2/8     |
| RGB, 2 levels, 4px blocks         | 42 K        | 8/8     | 8/8     | 8/8     | 2/8     |
| RGB, 2 levels, 8px blocks         | 10 K        | 8/8     | 8/8     | 8/8     | 8/8     |

Takeaways: lossless 256-level frames never survive; 2 levels with 4px blocks is the best setting that passed every codec tested (even at 1 Mbit/s), and is available as `--youtube`. RGB triples the capacity but chroma is compressed harder, so it only holds with H.264 or large blocks. 2px blocks fail, mostly through the header, and more ECC doesn't rescue them. These are local ffmpeg tests, not YouTube itself, whose encoder settings are unknown; try a real upload before trusting a setting.

## Large files and video

`encode-video` splits a file across numbered frames. Each frame carries a small chunk header (frame index, total frames, file length, file CRC-32) inside its Reed-Solomon protected payload, so frames can arrive in any order, repeated frames are ignored, a missing or unreadable frame is reported by number, and the reassembled file is verified end to end. The block size is auto-detected when decoding. `--hold N` shows every frame N times in the video so the decoder can fall back on another copy. ffmpeg turns the frames into an H.264/H.265 MP4.

Frames are streamed through ffmpeg's stdin/stdout as raw RGB (both when encoding and decoding), so nothing is written to disk and the tool holds one frame at a time however large the file is. Decoding stops as soon as enough frames have arrived and kills ffmpeg rather than reading the rest. Needs `ffmpeg` and `ffprobe` on PATH. A 20 MB file became 1,561 frames (52 s of video) and round-tripped byte-identical.

Throughput is limited by what the compressor can pass through: each default frame is ~14 KB of incompressible noise, and carrying it takes about 12 Mbit/s of video, well above its 3.4 Mbit/s information rate. Fixed-bitrate encodes look fine on short clips (a 22-frame clip survived 3 Mbit/s) because the rate control has slack, but fail partway through longer ones: with 469 frames, x264 at 8 Mbit/s lost half the frames from frame ~200 onward, while 12 Mbit/s was fine. So `encode-video` defaults to constant quality (`--crf 23`, x264 and x265 both verified), and `--bitrate 8M` is available if you need a size cap. Expect a video roughly four times the file size. A video whose frames can't all be recovered fails with the list of missing frame numbers.

### Parity frames

Frames are grouped: every `--group G` data frames (default 10) are followed by `--parity P` parity frames (default 1). The parity frames are Reed-Solomon codewords taken across the group (byte *j* of every frame is one codeword), so any P lost or unreadable frames in a group are rebuilt exactly, whichever frames they are. The default costs 10% extra frames and survives one lost frame per ten; raise `--parity` if the video may be cut or damaged more heavily, or use `--parity 0` to turn it off. If a group loses more than P frames, decoding fails with the list of unrecoverable frame numbers. The last group may be shorter and still gets its full parity. Checked by dropping frames out of a re-encoded video: two lost frames in different groups were rebuilt, two in the same group were reported as missing.

## Usage

```
cargo run -- encode input.txt frame.png [--rgb] [--size WxH] [--scale N] [--ecc N] [--levels L] [--youtube]
cargo run -- decode frame.png output.txt [--scale N]

# a whole file as a video (needs ffmpeg on PATH); the --youtube preset is the default:
cargo run --release -- encode-video bigfile.zip data.mp4 [--codec x264|x265] [--crf 23 | --bitrate 12M] [--hold 2] [--group 10 --parity 1]
cargo run --release -- decode-video data.mp4 bigfile.zip

# a single frame meant for lossy video (1920x1080 frame, ~14 KB):
cargo run -- encode input.bin frame.png --youtube
cargo run -- decode frame.png output.bin --scale 4
```

Frame layout: 208-pixel RS-protected header drawn in black/white (magic, mode, ecc, bits per symbol, payload length, payload CRC-32), interleaved RS payload (1 byte/pixel grayscale, 3 bytes/pixel RGB), and a per-column CRC-24 in the last row. Encoding fails rather than truncating if the data does not fit.
