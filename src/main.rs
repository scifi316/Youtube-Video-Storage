mod crc_24_openpgp;
mod error;
mod frame;
mod nnscale;

use error::{Error, Result};
use frame::Mode;
use std::fs;

const USAGE: &str = "\
Usage:
  youtube_video_storage encode <input> <output.png> [--rgb] [--size WxH] [--scale N]
  youtube_video_storage decode <input.png> <output> [--scale N]

encode: stores <input> in a PNG frame (default: grayscale, 240x135, scale 1).
        Fails if the data does not fit in the frame.
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

    let (mut mode, mut size, mut scale) = (Mode::Gray, (240, 135), 1u32);
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
            other => return Err(bad(&format!("unknown option '{other}'"))),
        }
    }

    match cmd.as_str() {
        "encode" => {
            let data = fs::read(input)?;
            let mut img = frame::encode(&data, size.0, size.1, mode)?;
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
            let data = frame::decode(&img)?;
            fs::write(output, &data)?;
            println!("Decoded {} bytes to {output}", data.len());
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
