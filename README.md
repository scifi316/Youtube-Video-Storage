
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

CRC(Cyclc Redundancy Check) allows for verifying and help with troubleshooting corrupted information, its usage here is implemented in the form of CRC-24/OPENPGP. Each column's CRC is stored in the last row of the frame. CRC-24 *detects* corruption (and the decoder reports exactly which columns failed) but cannot correct it; error correction (e.g. Reed-Solomon) is future work.

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
 - [ ] Sequenced image of data
 - [ ] Image to video generation
 - [ ] FFmpeg sequencer
 - [ ] Other scaling algos
 - [ ] Some GUI integration through Rust

<!-- Links & Images -->
[Rust-lang.org]: https://www.rust-lang.org/static/images/rust-logo-blk.svg
[Rust-url]: https://www.rust-lang.org

## Usage

```
cargo run -- encode input.txt frame.png [--rgb] [--size WxH] [--scale N]
cargo run -- decode frame.png output.txt [--scale N]
```

Frame layout: 12-pixel header (magic, mode, payload length, payload CRC-32), payload (1 byte/pixel grayscale, 3 bytes/pixel RGB), and a per-column CRC-24 in the last row. Encoding fails rather than truncating if the data does not fit.
