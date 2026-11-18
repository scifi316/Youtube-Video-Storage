// Frame format
//
//   * Pixels are filled row-major.
//   * The first HEADER_PIXELS pixels hold the header at 1 bit per pixel (black/white), the most
//     robust encoding possible. The header is a Reed-Solomon codeword (14 data bytes + 12 parity
//     bytes, so up to 6 damaged bytes are repaired):
//       magic "YVS" | mode (0 = gray, 1 = RGB) | ecc u8 | bits per symbol u8 | payload length u32 LE
//       | payload CRC-32 u32 LE
//   * The payload follows. Every byte is split into 8/bits symbols and each symbol is drawn as one
//     of 2^bits evenly spaced intensity levels (bits = 1, 2, 4 or 8, i.e. 2, 4, 16 or 256 levels).
//     Fewer levels means a bigger gap between levels, so lossy video compression (YouTube) can
//     blur values without flipping a symbol. Gray mode puts one symbol in each pixel; RGB mode puts
//     three (one per channel, which is higher capacity but chroma is compressed harder in video).
//   * Only at bits = 8 (lossless pipeline) the last row holds a CRC-24 per column. Those CRCs
//     tell the decoder which columns are damaged so Reed-Solomon can treat them as erasures.
//   * The payload is split into blocks of (255 - ecc) bytes; each block gets `ecc` Reed-Solomon
//     parity bytes, and the codewords are byte-interleaved so a burst of damage (a row, a column,
//     a smear) is spread thinly over many codewords. `ecc = 0` disables error correction.
//     A block repairs up to ecc/2 damaged bytes at unknown positions, or ecc at known positions.
//   * The payload CRC-32 is the final check on the recovered data.
use crate::crc_24_openpgp;
use crate::error::{Error, Result};
use crate::nnscale::downscale;
use crc::{Crc, CRC_32_ISO_HDLC};
use image::{Rgb, RgbImage};
use reed_solomon::{Decoder, Encoder};

pub const MAGIC: [u8; 3] = *b"YVS";
const HEADER_DATA: usize = 14;
const HEADER_ECC: usize = 12;
const HEADER_BYTES: usize = HEADER_DATA + HEADER_ECC;
/// The header occupies this many pixels (1 bit each).
pub const HEADER_PIXELS: usize = HEADER_BYTES * 8;
pub const DEFAULT_BITS: u8 = 8;
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
    fn symbols_per_pixel(self) -> usize {
        match self {
            Mode::Gray => 1,
            Mode::Rgb => 3,
        }
    }
}

/// Encoding parameters.
#[derive(Clone, Copy, Debug)]
pub struct Params {
    pub mode: Mode,
    /// Reed-Solomon parity bytes per block (0 disables).
    pub ecc: u8,
    /// Bits per symbol: 1, 2, 4 or 8 (2, 4, 16 or 256 intensity levels).
    pub bits: u8,
}

impl Params {
    fn validate(&self) -> Result<()> {
        check_ecc(self.ecc)?;
        if !matches!(self.bits, 1 | 2 | 4 | 8) {
            return Err(Error::InvalidParameter(format!(
                "bits per symbol must be 1, 2, 4 or 8, got {}", self.bits
            )));
        }
        Ok(())
    }
    /// Pixel value step between adjacent intensity levels.
    fn step(&self) -> u32 {
        255 / ((1u32 << self.bits) - 1)
    }
    fn has_crc_row(&self) -> bool {
        self.bits == 8
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

/// Number of pixels available for the payload.
fn payload_pixels(width: u32, height: u32, p: &Params) -> Result<usize> {
    let total = width as usize * height as usize;
    let reserved = HEADER_PIXELS + if p.has_crc_row() { width as usize } else { 0 };
    if width == 0 || height == 0 || total <= reserved {
        return Err(Error::BadDimensions(format!(
            "{width}x{height} is too small (needs room for the {HEADER_PIXELS}-pixel header{})",
            if p.has_crc_row() { " and a CRC row" } else { "" }
        )));
    }
    Ok(total - reserved)
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

/// Maximum payload size in bytes for a frame of the given size and parameters.
pub fn capacity(width: u32, height: u32, p: &Params) -> Result<usize> {
    p.validate()?;
    let pixels = payload_pixels(width, height, p)?;
    let total = pixels * p.mode.symbols_per_pixel() * p.bits as usize / 8;
    let k = block_data_len(p.ecc);
    let mut len = total * k / 255 + k;
    while len > 0 && stream_len(len, p.ecc) > total {
        len -= 1;
    }
    Ok(len)
}

fn bytes_to_symbols(bytes: &[u8], bits: u8) -> Vec<u8> {
    let per = 8 / bits as usize;
    let mask = ((1u16 << bits) - 1) as u8;
    let mut out = Vec::with_capacity(bytes.len() * per);
    for &b in bytes {
        for i in 0..per {
            out.push((b >> (8 - bits as usize * (i + 1))) & mask);
        }
    }
    out
}

fn symbols_to_bytes(symbols: &[u8], bits: u8) -> Vec<u8> {
    let per = 8 / bits as usize;
    symbols
        .chunks(per)
        .map(|c| (c.iter().fold(0u32, |acc, &s| (acc << bits) | s as u32) << (bits as usize * (per - c.len()))) as u8)
        .collect()
}

/// Round a (noisy) pixel channel value to the nearest symbol.
fn snap(v: u8, p: &Params) -> u8 {
    let step = p.step();
    let max = (1u32 << p.bits) - 1;
    ((v as u32 + step / 2) / step).min(max) as u8
}

/// Encode `data` into a width x height frame. Fails instead of truncating when it doesn't fit.
pub fn encode(data: &[u8], width: u32, height: u32, p: &Params) -> Result<RgbImage> {
    let cap = capacity(width, height, p)?;
    if data.len() > cap || data.len() > u32::MAX as usize {
        return Err(Error::Overflow { capacity: cap, len: data.len() });
    }
    let ecc = p.ecc;

    let mut hdr = [0u8; HEADER_DATA];
    hdr[..3].copy_from_slice(&MAGIC);
    hdr[3] = p.mode.id();
    hdr[4] = ecc;
    hdr[5] = p.bits;
    hdr[6..10].copy_from_slice(&(data.len() as u32).to_le_bytes());
    hdr[10..14].copy_from_slice(&CRC_32.checksum(data).to_le_bytes());
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
    let symbols = bytes_to_symbols(&stream, p.bits);
    let step = p.step() as u8;

    let mut img = RgbImage::new(width, height);
    let mut pixels = img.pixels_mut();
    for byte in header.iter() {
        for bit in (0..8).rev() {
            let v = if (byte >> bit) & 1 == 1 { 255 } else { 0 };
            *pixels.next().unwrap() = Rgb([v, v, v]);
        }
    }
    match p.mode {
        Mode::Gray => {
            for (px, &s) in pixels.zip(&symbols) {
                let v = s * step;
                *px = Rgb([v, v, v]);
            }
        }
        Mode::Rgb => {
            for (px, chunk) in pixels.zip(symbols.chunks(3)) {
                let mut c = [0u8; 3];
                for (d, &s) in c.iter_mut().zip(chunk) {
                    *d = s * step;
                }
                *px = Rgb(c);
            }
        }
    }

    if p.has_crc_row() {
        crc_24_openpgp::write_crcs(&mut img);
    }
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

struct Header {
    p: Params,
    len: usize,
    expected: u32,
    corrected: usize,
}

/// Read and validate just the (Reed-Solomon protected) header of a frame.
fn parse_header(img: &RgbImage) -> Result<Header> {
    let (width, height) = img.dimensions();
    if (width as usize) * (height as usize) < HEADER_PIXELS {
        return Err(Error::BadDimensions(format!("{width}x{height} is too small for a frame")));
    }
    // 1 bit per pixel, thresholded at mid-grey.
    let mut header_raw = vec![0u8; HEADER_BYTES];
    for (i, px) in img.pixels().take(HEADER_PIXELS).enumerate() {
        let luma = (px.0[0] as u32 + px.0[1] as u32 + px.0[2] as u32) / 3;
        if luma >= 128 {
            header_raw[i / 8] |= 0x80 >> (i % 8);
        }
    }
    let (hdr, corrected) = rs_correct(&Decoder::new(HEADER_ECC), &header_raw, &[])
        .ok_or_else(|| Error::BadHeader("header is damaged beyond repair or not a frame".into()))?;
    if hdr[..3] != MAGIC {
        return Err(Error::BadHeader("magic bytes not found".into()));
    }
    let mode = Mode::from_id(hdr[3])
        .ok_or_else(|| Error::BadHeader(format!("unknown mode {}", hdr[3])))?;
    let p = Params { mode, ecc: hdr[4], bits: hdr[5] };
    p.validate().map_err(|e| Error::BadHeader(e.to_string()))?;
    let len = u32::from_le_bytes(hdr[6..10].try_into().unwrap()) as usize;
    let expected = u32::from_le_bytes(hdr[10..14].try_into().unwrap());
    let cap = capacity(width, height, &p).map_err(|e| Error::BadHeader(e.to_string()))?;
    if len > cap {
        return Err(Error::BadHeader(format!("length {len} exceeds capacity {cap}")));
    }
    Ok(Header { p, len, expected, corrected })
}

/// Find the integer scale factor at which `img` (an upscaled frame) holds a valid frame header.
/// Returns 1 for an unscaled frame.
pub fn detect_scale(img: &RgbImage) -> Option<u32> {
    let (w, h) = img.dimensions();
    (1..=32u32)
        .filter(|s| w % s == 0 && h % s == 0)
        .find(|&s| match downscale(img, s) {
            Ok(small) => parse_header(&small).is_ok(),
            Err(_) => false,
        })
}

/// Decode a frame produced by `encode`, repairing damage where possible.
pub fn decode_report(img: &RgbImage) -> Result<Decoded> {
    let width = img.width();
    let Header { p, len, expected, corrected: mut corrected_bytes } = parse_header(img)?;
    let (mode, ecc) = (p.mode, p.ecc);

    let bad_columns = if p.has_crc_row() { crc_24_openpgp::bad_columns(img) } else { Vec::new() };
    let is_bad = |pixel_idx: usize| bad_columns.binary_search(&(pixel_idx as u32 % width)).is_ok();

    // Payload: read pixels, snap each channel to the nearest symbol, pack back into bytes.
    let spp = mode.symbols_per_pixel();
    let per = 8 / p.bits as usize;
    let slen = stream_len(len, ecc);
    let nsym = slen * per;
    let mut symbols = Vec::with_capacity(nsym + 2);
    for px in img.pixels().skip(HEADER_PIXELS) {
        if symbols.len() >= nsym {
            break;
        }
        match mode {
            Mode::Gray => {
                let luma = (px.0[0] as u32 + px.0[1] as u32 + px.0[2] as u32) / 3;
                symbols.push(snap(luma as u8, &p));
            }
            Mode::Rgb => symbols.extend(px.0.iter().map(|&v| snap(v, &p))),
        }
    }
    symbols.truncate(nsym);
    let stream = symbols_to_bytes(&symbols, p.bits);

    let lay = layout(len, ecc);
    let blocks = num_blocks(len, ecc);
    let k = block_data_len(ecc);
    let mut codewords: Vec<Vec<u8>> = (0..blocks)
        .map(|j| vec![0u8; (len - j * k).min(k) + ecc as usize])
        .collect();
    let mut erasures: Vec<Vec<u8>> = vec![Vec::new(); blocks];
    for (q, &(j, i)) in lay.iter().enumerate() {
        codewords[j][i] = stream[q];
        let (first, last) = (q * per, q * per + per - 1);
        if !bad_columns.is_empty()
            && (is_bad(HEADER_PIXELS + first / spp) || is_bad(HEADER_PIXELS + last / spp))
        {
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
    use crate::nnscale::rescale;

    const ECC: u8 = DEFAULT_ECC;

    fn p(mode: Mode, ecc: u8, bits: u8) -> Params {
        Params { mode, ecc, bits }
    }

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
    fn symbol_packing_roundtrip() {
        let data = sample(50);
        for bits in [1, 2, 4, 8] {
            let sym = bytes_to_symbols(&data, bits);
            assert!(sym.iter().all(|&s| (s as u32) < (1 << bits)));
            assert_eq!(symbols_to_bytes(&sym, bits), data);
        }
    }

    #[test]
    fn roundtrip_modes_ecc_levels_and_bits() {
        for mode in [Mode::Gray, Mode::Rgb] {
            for bits in [1, 2, 4, 8] {
                for ecc in [0, 2, ECC, 200] {
                    for n in [0, 1, 2, 3, 100, 253, 254, 255, 256, 1000] {
                        let params = p(mode, ecc, bits);
                        let data = sample(n);
                        if n > capacity(96, 64, &params).unwrap() {
                            continue;
                        }
                        let img = encode(&data, 96, 64, &params).unwrap();
                        assert_eq!(decode(&img).unwrap(), data, "{params:?} len {n}");
                    }
                }
            }
        }
    }

    #[test]
    fn trailing_zeros_preserved() {
        let data = vec![1, 2, 0, 0, 0, 0, 0];
        for mode in [Mode::Gray, Mode::Rgb] {
            for bits in [2, 8] {
                let img = encode(&data, 64, 16, &p(mode, ECC, bits)).unwrap();
                assert_eq!(decode(&img).unwrap(), data);
            }
        }
    }

    #[test]
    fn exact_capacity_and_overflow() {
        for mode in [Mode::Gray, Mode::Rgb] {
            for bits in [2, 8] {
                for ecc in [0, ECC] {
                    let params = p(mode, ecc, bits);
                    let cap = capacity(80, 40, &params).unwrap();
                    let full = sample(cap);
                    assert_eq!(decode(&encode(&full, 80, 40, &params).unwrap()).unwrap(), full);
                    assert!(matches!(
                        encode(&sample(cap + 1), 80, 40, &params),
                        Err(Error::Overflow { .. })
                    ));
                }
            }
        }
    }

    #[test]
    fn capacity_scales_with_bits_and_ecc() {
        let cap = |ecc, bits| capacity(240, 135, &p(Mode::Gray, ecc, bits)).unwrap();
        assert!(cap(ECC, 8) < cap(0, 8));
        assert!(cap(ECC, 8) > cap(0, 8) * 85 / 100);
        assert!(cap(0, 4) < cap(0, 8) * 55 / 100);
        assert!(cap(0, 1) < cap(0, 2));
    }

    #[test]
    fn parameters_are_validated() {
        assert!(matches!(encode(&[], 64, 32, &p(Mode::Gray, 201, 8)), Err(Error::InvalidParameter(_))));
        assert!(matches!(encode(&[], 64, 32, &p(Mode::Gray, 32, 3)), Err(Error::InvalidParameter(_))));
    }

    #[test]
    fn rejects_tiny_frames() {
        let g = p(Mode::Gray, ECC, 8);
        assert!(encode(&[], 0, 10, &g).is_err());
        assert!(encode(&[], 10, 1, &g).is_err());
        assert!(encode(&[], 5, 5, &g).is_err());
    }

    #[test]
    fn without_ecc_corruption_reports_column() {
        let mut img = encode(&sample(500), 50, 20, &p(Mode::Rgb, 0, 8)).unwrap();
        img.get_pixel_mut(7, 6).0[1] ^= 0x40; // pixel 307: inside the payload
        match decode(&img) {
            Err(Error::ColumnCrc(cols)) => assert_eq!(cols, vec![7]),
            other => panic!("expected ColumnCrc, got {other:?}"),
        }
    }

    #[test]
    fn scattered_pixel_damage_is_repaired() {
        for mode in [Mode::Gray, Mode::Rgb] {
            let data = sample(2000);
            let mut img = encode(&data, 80, 40, &p(mode, ECC, 8)).unwrap();
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
        let mut img = encode(&data, 80, 40, &p(Mode::Gray, ECC, 8)).unwrap();
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
        let mut img = encode(&data, 80, 40, &p(Mode::Gray, ECC, 8)).unwrap();
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
        let mut img = encode(&data, 64, 16, &p(Mode::Rgb, ECC, 4)).unwrap();
        for x in [0u32, 4, 9, 20] {
            let v = img.get_pixel(x, 0).0[0];
            img.put_pixel(x, 0, Rgb([255 - v; 3]));
        }
        assert_eq!(decode(&img).unwrap(), data);
    }

    #[test]
    fn excessive_damage_fails_cleanly() {
        let data = sample(2000);
        let mut img = encode(&data, 80, 40, &p(Mode::Gray, ECC, 8)).unwrap();
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
        let mut img = RgbImage::from_pixel(64, 8, Rgb([9, 9, 9]));
        crate::crc_24_openpgp::write_crcs(&mut img);
        assert!(matches!(decode(&img), Err(Error::BadHeader(_))));
    }

    #[test]
    fn quantized_frames_tolerate_noise_below_half_step() {
        for mode in [Mode::Gray, Mode::Rgb] {
            for bits in [1u8, 2, 4] {
                let params = p(mode, ECC, bits);
                let data = sample(400);
                let mut img = encode(&data, 96, 64, &params).unwrap();
                let max_noise = (params.step() / 2) as i64 - 1; // strictly less than half a step
                let mut rng = Lcg(99);
                for px in img.pixels_mut() {
                    for c in px.0.iter_mut() {
                        let n = (rng.next() % (2 * max_noise as u64 + 1)) as i64 - max_noise;
                        *c = (*c as i64 + n).clamp(0, 255) as u8;
                    }
                }
                let d = decode_report(&img).unwrap();
                assert_eq!(d.data, data, "{params:?}");
                assert_eq!(d.corrected_bytes, 0, "no repairs needed below half-step noise, {params:?}");
            }
        }
    }

    #[test]
    fn scaled_roundtrip_with_noise() {
        let data = sample(700);
        let img = encode(&data, 48, 24, &p(Mode::Rgb, ECC, 8)).unwrap();
        let mut big = rescale(&img, Some(8), "nearest").unwrap();
        assert_eq!(big.dimensions(), (384, 192));
        for (x, y, px) in big.enumerate_pixels_mut() {
            if x % 8 == 0 || y % 8 == 0 {
                *px = Rgb([255 - px.0[0], 0, 128]);
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

    /// End-to-end check against a real lossy encode. Skipped when ffmpeg is not installed.
    /// See scripts/ffmpeg_roundtrip.py for the full parameter sweep.
    #[test]
    fn survives_x264_reencode() {
        use std::process::Command;
        let have_ffmpeg = Command::new("ffmpeg").arg("-version").output().map(|o| o.status.success());
        if !have_ffmpeg.unwrap_or(false) {
            eprintln!("ffmpeg not found, skipping");
            return;
        }
        let dir = std::env::temp_dir().join(format!("yvs_test_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let params = p(Mode::Gray, ECC, 1); // 2 levels
        let mut rng = Lcg(5);
        let cap = capacity(480, 270, &params).unwrap();
        let payloads: Vec<Vec<u8>> = (0..4)
            .map(|_| (0..cap).map(|_| rng.next() as u8).collect())
            .collect();
        for (i, data) in payloads.iter().enumerate() {
            let img = rescale(&encode(data, 480, 270, &params).unwrap(), Some(4), "nearest").unwrap();
            img.save(dir.join(format!("f_{}.png", i + 1))).unwrap();
        }
        let video = dir.join("v.mp4");
        let ok = Command::new("ffmpeg")
            .args(["-y", "-loglevel", "error", "-framerate", "30", "-i"])
            .arg(dir.join("f_%d.png"))
            .args(["-c:v", "libx264", "-b:v", "3M", "-pix_fmt", "yuv420p"])
            .arg(&video)
            .status()
            .map(|s| s.success())
            .unwrap_or(false);
        if !ok {
            eprintln!("ffmpeg could not encode (libx264 missing?), skipping");
            std::fs::remove_dir_all(&dir).ok();
            return;
        }
        let out_pattern = dir.join("o_%d.png");
        assert!(Command::new("ffmpeg")
            .args(["-y", "-loglevel", "error", "-i"])
            .arg(&video)
            .args(["-fps_mode", "passthrough"])
            .arg(&out_pattern)
            .status()
            .unwrap()
            .success());
        for (i, data) in payloads.iter().enumerate() {
            let img = image::open(dir.join(format!("o_{}.png", i + 1))).unwrap().to_rgb8();
            let decoded = decode(&downscale(&img, 4).unwrap()).unwrap();
            assert_eq!(&decoded, data, "frame {i} differs after x264 re-encode");
        }
        std::fs::remove_dir_all(&dir).ok();
    }
}
