mod crc_24_openpgp;
mod error;
mod frame;
mod nnscale;

use error::{Error, Result};
use frame::Mode;
use std::fs;

const USAGE: &str = "\
Usage:
  youtube_video_storage encode <input> <output.png> [--rgb] [--size WxH] [--scale N] [--ecc N]
  youtube_video_storage capacity [--rgb] [--size WxH] [--ecc N] [--levels L]
  youtube_video_storage decode <input.png> <output> [--scale N]

encode: --youtube is a preset for lossy video (gray, 2 levels, 480x270 at scale 4 =
        1920x1080, ~14 KB per frame); later options override it. Stores <input> in a PNG frame (default: grayscale, 240x135, scale 1).
        --levels L sets intensity levels per channel (2, 4, 16 or 256; fewer levels
        survive lossy video compression better but store less). --ecc N sets Reed-Solomon parity bytes per 255-byte block (default 32,
        repairs ~16 bad bytes per block; 0 disables). Fails if data does not fit.
decode: recovers the file; use the same --scale that was used to encode.
        Reports which columns failed their CRC check if the image is damaged.";

fn parse_size(s: &str) -> Option<(u32, u32)> {
    let (w, h) = s.split_once('x')?;
    Some((w.parse().ok()?, h.parse().ok()?))
}

fn run(args: &[String]) -> Result<()> {
    let bad = |m: &str| Error::BadDimensions(format!("{m}\n\n{USAGE}"));
    let cmd = args.first().ok_or_else(|| bad("missing command"))?;
    // `capacity` takes only options; the others take <input> <output> first.
    let skip = if cmd == "capacity" { 1 } else { 3 };
    let (input, output) = if cmd == "capacity" {
        (&String::new(), &String::new())
    } else {
        (
            args.get(1).ok_or_else(|| bad("missing input"))?,
            args.get(2).ok_or_else(|| bad("missing output"))?,
        )
    };

    let (mut mode, mut size, mut scale, mut ecc, mut bits) =
        (Mode::Gray, (240, 135), 1u32, frame::DEFAULT_ECC, frame::DEFAULT_BITS);
    let mut opts = args[skip.min(args.len())..].iter();
    while let Some(opt) = opts.next() {
        match opt.as_str() {
            "--youtube" => {
                // Settings that survived H.264/H.265 re-encoding down to 1-3 Mbit/s in testing.
                (mode, size, scale, ecc, bits) = (Mode::Gray, (480, 270), 4, frame::DEFAULT_ECC, 1);
            }
            "--rgb" => mode = Mode::Rgb,
            "--size" => {
                size = opts.next().and_then(|s| parse_size(s)).ok_or_else(|| bad("--size needs WxH"))?
            }
            "--scale" => {
                scale = opts.next().and_then(|s| s.parse().ok()).ok_or_else(|| bad("--scale needs a number"))?
            }
            "--ecc" => {
                ecc = opts.next().and_then(|s| s.parse().ok()).ok_or_else(|| bad("--ecc needs a number 0-200"))?
            }
            "--levels" => {
                bits = match opts.next().map(|s| s.as_str()) {
                    Some("2") => 1,
                    Some("4") => 2,
                    Some("16") => 4,
                    Some("256") => 8,
                    _ => return Err(bad("--levels must be 2, 4, 16 or 256")),
                }
            }
            other => return Err(bad(&format!("unknown option '{other}'"))),
        }
    }

    match cmd.as_str() {
        "capacity" => {
            let (w, h) = (size.0, size.1);
            println!("{}", frame::capacity(w, h, &frame::Params { mode, ecc, bits })?);
        }
        "encode" => {
            let data = fs::read(input)?;
            let mut img = frame::encode(&data, size.0, size.1, &frame::Params { mode, ecc, bits })?;
            if scale > 1 {
                img = nnscale::rescale(&img, Some(scale), "nearest")?;
            }
            img.save(output)?;
            println!("Encoded {} bytes into {output} ({}x{})", data.len(), img.width(), img.height());
        }
        "decode" => {
            let mut img = image::open(input)?.to_rgb8();
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
