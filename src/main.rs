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
  youtube_video_storage decode <input.png> <output> [--scale N]

encode: stores <input> in a PNG frame (default: grayscale, 240x135, scale 1).
        --ecc N sets Reed-Solomon parity bytes per 255-byte block (default 32,
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
    let input = args.get(1).ok_or_else(|| bad("missing input"))?;
    let output = args.get(2).ok_or_else(|| bad("missing output"))?;

    let (mut mode, mut size, mut scale, mut ecc) = (Mode::Gray, (240, 135), 1u32, frame::DEFAULT_ECC);
    let mut opts = args[3..].iter();
    while let Some(opt) = opts.next() {
        match opt.as_str() {
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
            other => return Err(bad(&format!("unknown option '{other}'"))),
        }
    }

    match cmd.as_str() {
        "encode" => {
            let data = fs::read(input)?;
            let mut img = frame::encode(&data, size.0, size.1, mode, ecc)?;
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
