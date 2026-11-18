mod container;
mod crc_24_openpgp;
mod error;
mod frame;
mod nnscale;
mod video;

use error::{Error, Result};
use frame::{Mode, Params};
use std::fs;
use std::path::Path;
use container::Redundancy;
use video::{Codec, VideoOpts};

const USAGE: &str = "\
Usage:
  youtube_video_storage encode <input> <output.png> [options]       one frame
  youtube_video_storage decode <input.png> <output> [--scale N]
  youtube_video_storage encode-video <input> <output.mp4> [options]  any size, many frames
  youtube_video_storage decode-video <input.mp4> <output> [--scale N]
  youtube_video_storage capacity [options]                          bytes per frame

Frame options:
  --rgb               3 symbols per pixel instead of 1 (more data, less robust to video)
  --size WxH          frame size before scaling
  --scale N           enlarge each pixel to an NxN block (decode: omit to auto-detect)
  --levels L          intensity levels: 2, 4, 16 or 256. Fewer survive compression better
  --ecc N             Reed-Solomon parity bytes per block, 0-200 (default 32)
  --youtube           preset for lossy video: gray, 2 levels, 480x270 at scale 4 (1920x1080,
                      ~14 KB per frame). Later options override it.
Video options (encode-video):
  --codec x264|x265   (default x264)     --bitrate 8M     --fps 30
  --hold N            show every frame N times so a damaged copy can be replaced (default 1)
  --group G --parity P  add P parity frames per G data frames (default 10 and 1); any P lost
                      frames per group are rebuilt. --parity 0 turns it off

Defaults: `encode` stores a single lossless 240x135 grayscale frame. `encode-video` starts
from the --youtube preset. `decode` and `decode-video` read everything else from the frames.";

struct Opts {
    params: Params,
    size: (u32, u32),
    scale: Option<u32>,
    codec: Codec,
    bitrate: String,
    fps: u32,
    hold: u32,
    red: Redundancy,
}

fn bad(m: &str) -> Error {
    Error::InvalidParameter(format!("{m}\n\n{USAGE}"))
}

fn parse_size(s: &str) -> Option<(u32, u32)> {
    let (w, h) = s.split_once('x')?;
    Some((w.parse().ok()?, h.parse().ok()?))
}

fn parse_opts(args: &[String], youtube_default: bool) -> Result<Opts> {
    let mut o = Opts {
        params: Params { mode: Mode::Gray, ecc: frame::DEFAULT_ECC, bits: frame::DEFAULT_BITS },
        size: (240, 135),
        scale: None,
        codec: Codec::X264,
        bitrate: "8M".into(),
        fps: 30,
        hold: 1,
        red: Redundancy { group: 10, parity: 1 },
    };
    let youtube = |o: &mut Opts| {
        // Settings that survived H.264/H.265 re-encoding down to 1-3 Mbit/s in testing.
        o.params = Params { mode: Mode::Gray, ecc: frame::DEFAULT_ECC, bits: 1 };
        o.size = (480, 270);
        o.scale = Some(4);
    };
    if youtube_default {
        youtube(&mut o);
    }
    let mut it = args.iter();
    while let Some(opt) = it.next() {
        let mut value = |what: &str| it.next().ok_or_else(|| bad(&format!("{opt} needs {what}")));
        match opt.as_str() {
            "--youtube" => youtube(&mut o),
            "--rgb" => o.params.mode = Mode::Rgb,
            "--size" => o.size = parse_size(value("WxH")?).ok_or_else(|| bad("--size needs WxH"))?,
            "--scale" => o.scale = Some(value("a number")?.parse().map_err(|_| bad("--scale needs a number"))?),
            "--ecc" => o.params.ecc = value("a number 0-200")?.parse().map_err(|_| bad("--ecc needs a number 0-200"))?,
            "--fps" => o.fps = value("a number")?.parse().map_err(|_| bad("--fps needs a number"))?,
            "--hold" => o.hold = value("a number")?.parse().map_err(|_| bad("--hold needs a number"))?,
            "--group" => o.red.group = value("a number")?.parse().map_err(|_| bad("--group needs a number 1-255"))?,
            "--parity" => o.red.parity = value("a number")?.parse().map_err(|_| bad("--parity needs a number"))?,
            "--bitrate" => o.bitrate = value("a rate like 5M")?.clone(),
            "--codec" => {
                o.codec = match value("x264 or x265")?.as_str() {
                    "x264" => Codec::X264,
                    "x265" => Codec::X265,
                    _ => return Err(bad("--codec must be x264 or x265")),
                }
            }
            "--levels" => {
                o.params.bits = match value("2, 4, 16 or 256")?.as_str() {
                    "2" => 1,
                    "4" => 2,
                    "16" => 4,
                    "256" => 8,
                    _ => return Err(bad("--levels must be 2, 4, 16 or 256")),
                }
            }
            other => return Err(bad(&format!("unknown option '{other}'"))),
        }
    }
    Ok(o)
}

fn run(args: &[String]) -> Result<()> {
    let cmd = args.first().ok_or_else(|| bad("missing command"))?;
    if matches!(cmd.as_str(), "-h" | "--help" | "help") {
        println!("{USAGE}");
        return Ok(());
    }
    let positional = if cmd == "capacity" { 0 } else { 2 };
    if args.len() < 1 + positional {
        return Err(bad("missing input/output"));
    }
    let (input, output) = (args.get(1).map(String::as_str).unwrap_or(""), args.get(2).map(String::as_str).unwrap_or(""));
    let rest = &args[1 + positional..];
    let video_cmd = cmd == "encode-video";
    let o = parse_opts(rest, video_cmd)?;

    match cmd.as_str() {
        "capacity" => {
            println!("{}", frame::capacity(o.size.0, o.size.1, &o.params)?);
        }
        "encode" => {
            let data = fs::read(input)?;
            let scale = o.scale.unwrap_or(1);
            let mut img = frame::encode(&data, o.size.0, o.size.1, &o.params)?;
            if scale > 1 {
                img = nnscale::rescale(&img, Some(scale), "nearest")?;
            }
            img.save(output)?;
            println!("Encoded {} bytes into {output} ({}x{})", data.len(), img.width(), img.height());
        }
        "decode" => {
            let mut img = image::open(input)?.to_rgb8();
            let scale = match o.scale {
                Some(s) => s,
                None => frame::detect_scale(&img).ok_or_else(|| {
                    Error::BadHeader("no valid frame found at any scale; try --scale N".into())
                })?,
            };
            if scale > 1 {
                img = nnscale::downscale(&img, scale)?;
            }
            let d = frame::decode_report(&img)?;
            fs::write(output, &d.data)?;
            println!(
                "Decoded {} bytes to {output} ({} byte(s) repaired, {} damaged column(s))",
                d.data.len(), d.corrected_bytes, d.bad_columns.len()
            );
        }
        "encode-video" => {
            let data = fs::read(input)?;
            let vo = VideoOpts {
                params: o.params,
                size: o.size,
                scale: o.scale.unwrap_or(1),
                codec: o.codec,
                bitrate: o.bitrate.clone(),
                fps: o.fps,
                hold: o.hold,
                redundancy: o.red,
            };
            let frames = video::plan(data.len(), &vo)?;
            println!(
                "{} bytes -> {frames} frame(s) ({} parity) of {}x{} at {} fps ({:.1}s of video)",
                data.len(), frames - container::frames_needed(data.len(), o.size.0, o.size.1, &o.params, Redundancy::NONE)?, o.size.0 * vo.scale, o.size.1 * vo.scale, o.fps,
                (frames as f64 * o.hold as f64) / o.fps as f64
            );
            video::encode_video(&data, Path::new(output), &vo)?;
            println!("Wrote {output}");
        }
        "decode-video" => {
            let (data, s) = video::decode_video(Path::new(input), o.scale)?;
            fs::write(output, &data)?;
            println!(
                "Decoded {} bytes to {output}: {} frame(s) from {} video frame(s) at scale {} \
                 ({} duplicate(s), {} unreadable, {} byte(s) repaired, {} frame(s) rebuilt from parity)",
                data.len(), s.accepted, s.video_frames, s.scale, s.duplicates, s.unreadable, s.corrected_bytes,
                s.recovered_frames
            );
        }
        other => return Err(bad(&format!("unknown command '{other}'"))),
    }
    Ok(())
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if let Err(e) = run(&args) {
        eprintln!("error: {e}");
        std::process::exit(1);
    }
}
