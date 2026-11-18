// Frame format
//
//   * Pixels are filled row-major. The last row holds one CRC-24 per column.
//   * The first HEADER_LEN pixels are always grey (r=g=b) and hold the header:
//       magic "YVS" | mode (0 = grayscale, 1 = RGB) | payload length u32 LE | payload CRC-32 u32 LE
//   * The payload follows: 1 byte per pixel in grayscale mode, 3 bytes per pixel in RGB mode.
//     Unused pixels are zero; the stored length distinguishes real data from padding.
use crate::crc_24_openpgp;
use crate::error::{Error, Result};
use crc::{Crc, CRC_32_ISO_HDLC};
use image::{Rgb, RgbImage};

pub const MAGIC: [u8; 3] = *b"YVS";
pub const HEADER_LEN: usize = 12;
const CRC_32: Crc<u32> = Crc::<u32>::new(&CRC_32_ISO_HDLC);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    Gray,
    Rgb,
}

impl Mode {
    fn id(self) -> u8 {
        match self {
            Mode::Gray => 0,
            Mode::Rgb => 1,
        }
    }
    fn from_id(id: u8) -> Option<Mode> {
        match id {
            0 => Some(Mode::Gray),
            1 => Some(Mode::Rgb),
            _ => None,
        }
    }
    fn bytes_per_pixel(self) -> usize {
        match self {
            Mode::Gray => 1,
            Mode::Rgb => 3,
        }
    }
}

fn check_dims(width: u32, height: u32) -> Result<usize> {
    if height < 2 || width == 0 {
        return Err(Error::BadDimensions(format!(
            "{width}x{height}: need width >= 1 and height >= 2 (last row is reserved for CRCs)"
        )));
    }
    let data_pixels = width as usize * (height as usize - 1);
    if data_pixels < HEADER_LEN {
        return Err(Error::BadDimensions(format!(
            "{width}x{height} cannot hold the {HEADER_LEN}-pixel header"
        )));
    }
    Ok(data_pixels)
}

/// Maximum payload size in bytes for a frame of the given size.
pub fn capacity(width: u32, height: u32, mode: Mode) -> Result<usize> {
    let data_pixels = check_dims(width, height)?;
    Ok((data_pixels - HEADER_LEN) * mode.bytes_per_pixel())
}

/// Encode `data` into a width x height frame. Fails instead of truncating when it doesn't fit.
pub fn encode(data: &[u8], width: u32, height: u32, mode: Mode) -> Result<RgbImage> {
    let cap = capacity(width, height, mode)?;
    if data.len() > cap || data.len() > u32::MAX as usize {
        return Err(Error::Overflow { capacity: cap, len: data.len() });
    }

    let mut img = RgbImage::new(width, height);
    let mut header = [0u8; HEADER_LEN];
    header[..3].copy_from_slice(&MAGIC);
    header[3] = mode.id();
    header[4..8].copy_from_slice(&(data.len() as u32).to_le_bytes());
    header[8..12].copy_from_slice(&CRC_32.checksum(data).to_le_bytes());

    let mut pixels = img.pixels_mut();
    for &b in &header {
        *pixels.next().unwrap() = Rgb([b, b, b]);
    }
    match mode {
        Mode::Gray => {
            for (px, &b) in pixels.zip(data) {
                *px = Rgb([b, b, b]);
            }
        }
        Mode::Rgb => {
            for (px, chunk) in pixels.zip(data.chunks(3)) {
                let mut c = [0u8; 3];
                c[..chunk.len()].copy_from_slice(chunk);
                *px = Rgb(c);
            }
        }
    }

    crc_24_openpgp::write_crcs(&mut img);
    Ok(img)
}

/// Decode a frame produced by `encode`. Verifies column CRCs, header and payload CRC.
pub fn decode(img: &RgbImage) -> Result<Vec<u8>> {
    let (width, height) = img.dimensions();
    let data_pixels = check_dims(width, height)?;
    crc_24_openpgp::verify(img)?;

    let mut pixels = img.pixels().take(data_pixels);
    let header: Vec<u8> = pixels.by_ref().take(HEADER_LEN).map(|p| p.0[0]).collect();
    if header[..3] != MAGIC {
        return Err(Error::BadHeader("magic bytes not found".into()));
    }
    let mode = Mode::from_id(header[3])
        .ok_or_else(|| Error::BadHeader(format!("unknown mode {}", header[3])))?;
    let len = u32::from_le_bytes(header[4..8].try_into().unwrap()) as usize;
    let expected = u32::from_le_bytes(header[8..12].try_into().unwrap());
    let cap = capacity(width, height, mode)?;
    if len > cap {
        return Err(Error::BadHeader(format!("length {len} exceeds capacity {cap}")));
    }

    let mut out = Vec::with_capacity(len + 3);
    for px in pixels {
        if out.len() >= len {
            break;
        }
        match mode {
            Mode::Gray => out.push(px.0[0]),
            Mode::Rgb => out.extend_from_slice(&px.0),
        }
    }
    out.truncate(len);

    let actual = CRC_32.checksum(&out);
    if actual != expected {
        return Err(Error::PayloadCrc { expected, actual });
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::nnscale::{downscale, rescale};

    fn sample(n: usize) -> Vec<u8> {
        (0..n).map(|i| (i * 31 % 251) as u8).collect()
    }

    #[test]
    fn roundtrip_both_modes() {
        for mode in [Mode::Gray, Mode::Rgb] {
            for n in [0, 1, 2, 3, 4, 100, 1000] {
                let data = sample(n);
                let img = encode(&data, 64, 32, mode).unwrap();
                assert_eq!(decode(&img).unwrap(), data, "{mode:?} len {n}");
            }
        }
    }

    #[test]
    fn trailing_zeros_preserved() {
        let data = vec![1, 2, 0, 0, 0, 0, 0];
        for mode in [Mode::Gray, Mode::Rgb] {
            assert_eq!(decode(&encode(&data, 32, 8, mode).unwrap()).unwrap(), data);
        }
    }

    #[test]
    fn exact_capacity_and_overflow() {
        for mode in [Mode::Gray, Mode::Rgb] {
            let cap = capacity(40, 10, mode).unwrap();
            let full = sample(cap);
            assert_eq!(decode(&encode(&full, 40, 10, mode).unwrap()).unwrap(), full);
            let too_big = sample(cap + 1);
            assert!(matches!(
                encode(&too_big, 40, 10, mode),
                Err(Error::Overflow { .. })
            ));
        }
    }

    #[test]
    fn rejects_tiny_frames() {
        assert!(encode(&[], 0, 10, Mode::Gray).is_err());
        assert!(encode(&[], 10, 1, Mode::Gray).is_err());
        assert!(encode(&[], 3, 3, Mode::Gray).is_err());
    }

    #[test]
    fn corrupted_pixel_reports_column() {
        let mut img = encode(&sample(500), 50, 20, Mode::Rgb).unwrap();
        let p = img.get_pixel_mut(7, 5);
        p.0[1] ^= 0x40;
        match decode(&img) {
            Err(Error::ColumnCrc(cols)) => assert_eq!(cols, vec![7]),
            other => panic!("expected ColumnCrc, got {other:?}"),
        }
    }

    #[test]
    fn non_frame_image_is_rejected() {
        let mut img = RgbImage::from_pixel(32, 8, Rgb([9, 9, 9]));
        crate::crc_24_openpgp::write_crcs(&mut img);
        assert!(matches!(decode(&img), Err(Error::BadHeader(_))));
    }

    #[test]
    fn scaled_roundtrip() {
        let data = sample(700);
        let img = encode(&data, 48, 24, Mode::Rgb).unwrap();
        let big = rescale(&img, Some(8), "nearest").unwrap();
        assert_eq!(big.dimensions(), (384, 192));
        assert_eq!(decode(&downscale(&big, 8).unwrap()).unwrap(), data);
    }

    #[test]
    fn downscale_tolerates_edge_noise() {
        let data = sample(300);
        let img = encode(&data, 48, 16, Mode::Gray).unwrap();
        let mut big = rescale(&img, Some(8), "nearest").unwrap();
        // Corrupt the outer ring of every block; the centre is what gets sampled.
        for (x, y, p) in big.enumerate_pixels_mut() {
            if x % 8 == 0 || y % 8 == 0 {
                *p = Rgb([255 - p.0[0], 0, 128]);
            }
        }
        assert_eq!(decode(&downscale(&big, 8).unwrap()).unwrap(), data);
    }

    #[test]
    fn scale_errors() {
        let img = RgbImage::new(4, 4);
        assert!(rescale(&img, Some(0), "nearest").is_err());
        assert!(rescale(&img, Some(2), "bicubic").is_err());
        assert!(downscale(&img, 3).is_err());
    }
}
