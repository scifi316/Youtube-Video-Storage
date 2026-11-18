// Frame format
//
//   * Pixels are filled row-major. The last row holds one CRC-24 per column.
//   * The first HEADER_LEN pixels are always grey (r=g=b) and hold the header, itself a
//     Reed-Solomon codeword (13 data bytes + 12 parity bytes, so up to 6 damaged bytes are repaired):
//       magic "YVS" | mode (0 = gray, 1 = RGB) | ecc u8 | payload length u32 LE | payload CRC-32 u32 LE
//   * The payload follows, 1 byte/pixel in gray mode, 3 bytes/pixel in RGB mode. It is split into
//     blocks of (255 - ecc) bytes; each block gets `ecc` Reed-Solomon parity bytes, and the
//     resulting codewords are byte-interleaved so a burst of damage (a row, a column, a smear)
//     is spread thinly over many codewords. `ecc = 0` disables error correction.
//   * Each RS block repairs up to `ecc / 2` damaged bytes at unknown positions, or up to `ecc`
//     bytes at known positions. The decoder uses the column CRCs to learn which columns are bad
//     and passes those positions to Reed-Solomon as erasures (twice as efficient).
//   * The payload CRC-32 is the final check on the recovered data.
use crate::crc_24_openpgp;
use crate::error::{Error, Result};
use crc::{Crc, CRC_32_ISO_HDLC};
use image::{Rgb, RgbImage};
use reed_solomon::{Decoder, Encoder};

pub const MAGIC: [u8; 3] = *b"YVS";
const HEADER_DATA: usize = 13;
const HEADER_ECC: usize = 12;
pub const HEADER_LEN: usize = HEADER_DATA + HEADER_ECC;
pub const MAX_ECC: u8 = 200;
pub const DEFAULT_ECC: u8 = 32;
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

/// Result of a successful decode, with diagnostics about the repairs made.
#[derive(Debug)]
pub struct Decoded {
    pub data: Vec<u8>,
    /// Number of bytes Reed-Solomon had to repair (including header).
    pub corrected_bytes: usize,
    /// Columns whose CRC-24 did not match.
    pub bad_columns: Vec<u32>,
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

fn check_ecc(ecc: u8) -> Result<()> {
    if ecc > MAX_ECC {
        return Err(Error::InvalidParameter(format!("ecc must be 0..={MAX_ECC}, got {ecc}")));
    }
    Ok(())
}

fn block_data_len(ecc: u8) -> usize {
    255 - ecc as usize
}

fn num_blocks(len: usize, ecc: u8) -> usize {
    len.div_ceil(block_data_len(ecc))
}

fn stream_len(len: usize, ecc: u8) -> usize {
    len + num_blocks(len, ecc) * ecc as usize
}

/// For every byte position in the stream, which (block, index within codeword) it holds.
fn layout(len: usize, ecc: u8) -> Vec<(usize, usize)> {
    let k = block_data_len(ecc);
    let blocks = num_blocks(len, ecc);
    let cw_len = |j: usize| (len - j * k).min(k) + ecc as usize;
    let mut out = Vec::with_capacity(stream_len(len, ecc));
    for i in 0..255 {
        for j in 0..blocks {
            if i < cw_len(j) {
                out.push((j, i));
            }
        }
    }
    out
}

/// Maximum payload size in bytes for a frame of the given size and ECC level.
pub fn capacity(width: u32, height: u32, mode: Mode, ecc: u8) -> Result<usize> {
    check_ecc(ecc)?;
    let data_pixels = check_dims(width, height)?;
    let total = (data_pixels - HEADER_LEN) * mode.bytes_per_pixel();
    let k = block_data_len(ecc);
    let mut len = total * k / 255 + k;
    while len > 0 && stream_len(len, ecc) > total {
        len -= 1;
    }
    Ok(len)
}

/// Encode `data` into a width x height frame with `ecc` Reed-Solomon parity bytes per block.
/// Fails instead of truncating when the data doesn't fit.
pub fn encode(data: &[u8], width: u32, height: u32, mode: Mode, ecc: u8) -> Result<RgbImage> {
    let cap = capacity(width, height, mode, ecc)?;
    if data.len() > cap || data.len() > u32::MAX as usize {
        return Err(Error::Overflow { capacity: cap, len: data.len() });
    }

    let mut hdr = [0u8; HEADER_DATA];
    hdr[..3].copy_from_slice(&MAGIC);
    hdr[3] = mode.id();
    hdr[4] = ecc;
    hdr[5..9].copy_from_slice(&(data.len() as u32).to_le_bytes());
    hdr[9..13].copy_from_slice(&CRC_32.checksum(data).to_le_bytes());
    let header = Encoder::new(HEADER_ECC).encode(&hdr);

    // Reed-Solomon codewords, then interleave.
    let k = block_data_len(ecc);
    let codewords: Vec<Vec<u8>> = if ecc == 0 {
        data.chunks(k).map(|c| c.to_vec()).collect()
    } else {
        let enc = Encoder::new(ecc as usize);
        data.chunks(k).map(|c| enc.encode(c).to_vec()).collect()
    };
    let stream: Vec<u8> = layout(data.len(), ecc)
        .iter()
        .map(|&(j, i)| codewords[j][i])
        .collect();

    let mut img = RgbImage::new(width, height);
    let mut pixels = img.pixels_mut();
    for &b in header.iter() {
        *pixels.next().unwrap() = Rgb([b, b, b]);
    }
    match mode {
        Mode::Gray => {
            for (px, &b) in pixels.zip(&stream) {
                *px = Rgb([b, b, b]);
            }
        }
        Mode::Rgb => {
            for (px, chunk) in pixels.zip(stream.chunks(3)) {
                let mut c = [0u8; 3];
                c[..chunk.len()].copy_from_slice(chunk);
                *px = Rgb(c);
            }
        }
    }

    crc_24_openpgp::write_crcs(&mut img);
    Ok(img)
}

/// Try with erasures first, then without (in case the erasure hints were over-eager).
fn rs_correct(dec: &Decoder, cw: &[u8], erasures: &[u8]) -> Option<(Vec<u8>, usize)> {
    if !erasures.is_empty() {
        if let Ok((buf, n)) = dec.correct_err_count(cw, Some(erasures)) {
            return Some((buf.data().to_vec(), n));
        }
    }
    dec.correct_err_count(cw, None).ok().map(|(b, n)| (b.data().to_vec(), n))
}

/// Decode a frame, returning just the data.
#[allow(dead_code)]
pub fn decode(img: &RgbImage) -> Result<Vec<u8>> {
    decode_report(img).map(|d| d.data)
}

/// Decode a frame produced by `encode`, repairing damage where possible.
pub fn decode_report(img: &RgbImage) -> Result<Decoded> {
    let (width, height) = img.dimensions();
    let data_pixels = check_dims(width, height)?;
    let bad_columns = crc_24_openpgp::bad_columns(img);
    let is_bad = |pixel_idx: usize| bad_columns.binary_search(&(pixel_idx as u32 % width)).is_ok();
    let mut corrected_bytes = 0;

    // Header
    let header_raw: Vec<u8> = img.pixels().take(HEADER_LEN).map(|p| p.0[0]).collect();
    let header_erasures: Vec<u8> = (0..HEADER_LEN).filter(|&i| is_bad(i)).map(|i| i as u8).collect();
    let (hdr, n) = rs_correct(&Decoder::new(HEADER_ECC), &header_raw, &header_erasures)
        .ok_or_else(|| Error::BadHeader("header is damaged beyond repair or not a frame".into()))?;
    corrected_bytes += n;
    if hdr[..3] != MAGIC {
        return Err(Error::BadHeader("magic bytes not found".into()));
    }
    let mode = Mode::from_id(hdr[3])
        .ok_or_else(|| Error::BadHeader(format!("unknown mode {}", hdr[3])))?;
    let ecc = hdr[4];
    check_ecc(ecc).map_err(|e| Error::BadHeader(e.to_string()))?;
    let len = u32::from_le_bytes(hdr[5..9].try_into().unwrap()) as usize;
    let expected = u32::from_le_bytes(hdr[9..13].try_into().unwrap());
    let cap = capacity(width, height, mode, ecc)?;
    if len > cap {
        return Err(Error::BadHeader(format!("length {len} exceeds capacity {cap}")));
    }

    // Payload stream
    let bpp = mode.bytes_per_pixel();
    let slen = stream_len(len, ecc);
    let mut stream = Vec::with_capacity(slen + 2);
    for px in img.pixels().skip(HEADER_LEN).take(data_pixels - HEADER_LEN) {
        if stream.len() >= slen {
            break;
        }
        match mode {
            Mode::Gray => stream.push(px.0[0]),
            Mode::Rgb => stream.extend_from_slice(&px.0),
        }
    }
    stream.truncate(slen);

    let lay = layout(len, ecc);
    let blocks = num_blocks(len, ecc);
    let k = block_data_len(ecc);
    let mut codewords: Vec<Vec<u8>> = (0..blocks)
        .map(|j| vec![0u8; (len - j * k).min(k) + ecc as usize])
        .collect();
    let mut erasures: Vec<Vec<u8>> = vec![Vec::new(); blocks];
    for (p, &(j, i)) in lay.iter().enumerate() {
        codewords[j][i] = stream[p];
        if is_bad(HEADER_LEN + p / bpp) {
            erasures[j].push(i as u8);
        }
    }

    let mut out = Vec::with_capacity(len);
    if ecc == 0 {
        for cw in &codewords {
            out.extend_from_slice(cw);
        }
    } else {
        let dec = Decoder::new(ecc as usize);
        for (j, cw) in codewords.iter().enumerate() {
            let (data, n) = rs_correct(&dec, cw, &erasures[j]).ok_or(Error::Uncorrectable { block: j })?;
            corrected_bytes += n;
            out.extend_from_slice(&data);
        }
    }
    out.truncate(len);

    let actual = CRC_32.checksum(&out);
    if actual != expected {
        if !bad_columns.is_empty() && ecc == 0 {
            return Err(Error::ColumnCrc(bad_columns));
        }
        return Err(Error::PayloadCrc { expected, actual });
    }
    Ok(Decoded { data: out, corrected_bytes, bad_columns })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::nnscale::{downscale, rescale};

    const ECC: u8 = DEFAULT_ECC;

    fn sample(n: usize) -> Vec<u8> {
        (0..n).map(|i| (i * 31 % 251) as u8).collect()
    }

    /// Tiny deterministic PRNG so tests need no extra dependencies.
    struct Lcg(u64);
    impl Lcg {
        fn next(&mut self) -> u64 {
            self.0 = self.0.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            self.0 >> 33
        }
    }

    #[test]
    fn roundtrip_modes_and_ecc_levels() {
        for mode in [Mode::Gray, Mode::Rgb] {
            for ecc in [0, 2, ECC, 200] {
                for n in [0, 1, 2, 3, 100, 253, 254, 255, 256, 1000] {
                    let data = sample(n);
                    let cap = capacity(64, 32, mode, ecc).unwrap();
                    if n > cap {
                        continue;
                    }
                    let img = encode(&data, 64, 32, mode, ecc).unwrap();
                    assert_eq!(decode(&img).unwrap(), data, "{mode:?} ecc {ecc} len {n}");
                }
            }
        }
    }

    #[test]
    fn trailing_zeros_preserved() {
        let data = vec![1, 2, 0, 0, 0, 0, 0];
        for mode in [Mode::Gray, Mode::Rgb] {
            assert_eq!(decode(&encode(&data, 32, 8, mode, ECC).unwrap()).unwrap(), data);
        }
    }

    #[test]
    fn exact_capacity_and_overflow() {
        for mode in [Mode::Gray, Mode::Rgb] {
            for ecc in [0, ECC] {
                let cap = capacity(40, 10, mode, ecc).unwrap();
                let full = sample(cap);
                assert_eq!(decode(&encode(&full, 40, 10, mode, ecc).unwrap()).unwrap(), full);
                assert!(matches!(
                    encode(&sample(cap + 1), 40, 10, mode, ecc),
                    Err(Error::Overflow { .. })
                ));
            }
        }
    }

    #[test]
    fn ecc_reduces_capacity_and_is_validated() {
        let plain = capacity(100, 50, Mode::Rgb, 0).unwrap();
        let protected = capacity(100, 50, Mode::Rgb, ECC).unwrap();
        assert!(protected < plain);
        assert!(protected > plain * 85 / 100);
        assert!(matches!(encode(&[], 64, 32, Mode::Gray, 201), Err(Error::InvalidParameter(_))));
    }

    #[test]
    fn rejects_tiny_frames() {
        assert!(encode(&[], 0, 10, Mode::Gray, ECC).is_err());
        assert!(encode(&[], 10, 1, Mode::Gray, ECC).is_err());
        assert!(encode(&[], 5, 5, Mode::Gray, ECC).is_err());
    }

    #[test]
    fn without_ecc_corruption_reports_column() {
        let mut img = encode(&sample(500), 50, 20, Mode::Rgb, 0).unwrap();
        img.get_pixel_mut(7, 1).0[1] ^= 0x40; // pixel 57: inside the payload
        match decode(&img) {
            Err(Error::ColumnCrc(cols)) => assert_eq!(cols, vec![7]),
            other => panic!("expected ColumnCrc, got {other:?}"),
        }
    }

    #[test]
    fn scattered_pixel_damage_is_repaired() {
        for mode in [Mode::Gray, Mode::Rgb] {
            let data = sample(2000);
            let mut img = encode(&data, 80, 40, mode, ECC).unwrap();
            let mut rng = Lcg(42);
            for _ in 0..30 {
                let (x, y) = ((rng.next() % 80) as u32, (rng.next() % 39) as u32);
                img.put_pixel(x, y, Rgb([rng.next() as u8, rng.next() as u8, rng.next() as u8]));
            }
            let d = decode_report(&img).unwrap();
            assert_eq!(d.data, data, "{mode:?}");
            assert!(d.corrected_bytes > 0);
            assert!(!d.bad_columns.is_empty());
        }
    }

    #[test]
    fn damage_with_valid_column_crcs_is_repaired_as_errors() {
        // Recomputing the CRCs hides the damage from the erasure hints, so this exercises
        // true unknown-position error correction.
        let data = sample(2000);
        let mut img = encode(&data, 80, 40, Mode::Gray, ECC).unwrap();
        let mut rng = Lcg(7);
        for _ in 0..20 {
            let (x, y) = ((rng.next() % 80) as u32, (rng.next() % 39) as u32);
            img.put_pixel(x, y, Rgb([1, 2, 3]));
        }
        crate::crc_24_openpgp::write_crcs(&mut img);
        let d = decode_report(&img).unwrap();
        assert_eq!(d.data, data);
        assert!(d.bad_columns.is_empty());
    }

    #[test]
    fn whole_column_and_row_damage_is_repaired() {
        let data = sample(2500);
        let mut img = encode(&data, 80, 40, Mode::Gray, ECC).unwrap();
        for y in 0..40 {
            img.put_pixel(33, y, Rgb([255, 0, 255])); // a destroyed column
        }
        for x in 0..80 {
            img.put_pixel(x, 20, Rgb([0, 0, 0])); // a destroyed row
        }
        assert_eq!(decode(&img).unwrap(), data);
    }

    #[test]
    fn header_damage_is_repaired() {
        let data = sample(300);
        let mut img = encode(&data, 64, 16, Mode::Rgb, ECC).unwrap();
        for x in [0u32, 4, 9, 20] {
            img.put_pixel(x, 0, Rgb([200, 200, 200]));
        }
        assert_eq!(decode(&img).unwrap(), data);
    }

    #[test]
    fn excessive_damage_fails_cleanly() {
        let data = sample(2000);
        let mut img = encode(&data, 80, 40, Mode::Gray, ECC).unwrap();
        let mut rng = Lcg(1);
        for y in 0..39 {
            for x in 0..80 {
                if rng.next() % 2 == 0 {
                    img.put_pixel(x, y, Rgb([rng.next() as u8; 3]));
                }
            }
        }
        assert!(decode(&img).is_err());
    }

    #[test]
    fn non_frame_image_is_rejected() {
        let mut img = RgbImage::from_pixel(32, 8, Rgb([9, 9, 9]));
        crate::crc_24_openpgp::write_crcs(&mut img);
        assert!(matches!(decode(&img), Err(Error::BadHeader(_))));
    }

    #[test]
    fn scaled_roundtrip_with_noise() {
        let data = sample(700);
        let img = encode(&data, 48, 24, Mode::Rgb, ECC).unwrap();
        let mut big = rescale(&img, Some(8), "nearest").unwrap();
        assert_eq!(big.dimensions(), (384, 192));
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
