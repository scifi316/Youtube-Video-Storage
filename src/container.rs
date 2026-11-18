// Multi-frame container: splits a file over numbered frames and puts it back together.
//
// Each frame's payload (itself Reed-Solomon protected by `frame`) starts with a small chunk header:
//   version u8 (=1) | frame index u32 | total frames u32 | file length u64 | file CRC-32 u32
// so frames can arrive in any order, duplicates are harmless (e.g. a frame shown for several video
// frames), a damaged frame is reported by index, and the finished file is checked end to end.
use crate::error::{Error, Result};
use crate::frame::{self, Params};
use crc::{Crc, CRC_32_ISO_HDLC};
use image::RgbImage;
use std::collections::BTreeMap;

pub const CHUNK_HEADER: usize = 21;
const VERSION: u8 = 1;
const CRC_32: Crc<u32> = Crc::<u32>::new(&CRC_32_ISO_HDLC);

/// File bytes carried by each frame.
pub fn chunk_capacity(width: u32, height: u32, p: &Params) -> Result<usize> {
    let cap = frame::capacity(width, height, p)?;
    cap.checked_sub(CHUNK_HEADER)
        .filter(|&c| c > 0)
        .ok_or_else(|| Error::BadDimensions(format!("{width}x{height} is too small to carry data")))
}

/// Number of frames `len` bytes will occupy (at least one, even for an empty file).
pub fn frames_needed(len: usize, width: u32, height: u32, p: &Params) -> Result<usize> {
    Ok(len.div_ceil(chunk_capacity(width, height, p)?).max(1))
}

/// Lazily yield the frames for `data`, one at a time (so huge files don't sit in memory as images).
pub fn split<'a>(
    data: &'a [u8],
    width: u32,
    height: u32,
    p: &'a Params,
) -> Result<impl Iterator<Item = Result<RgbImage>> + 'a> {
    let per = chunk_capacity(width, height, p)?;
    let total = frames_needed(data.len(), width, height, p)?;
    let total32 = u32::try_from(total).map_err(|_| Error::Overflow { capacity: u32::MAX as usize, len: total })?;
    let crc = CRC_32.checksum(data);
    Ok((0..total).map(move |i| {
        let chunk = &data[(i * per).min(data.len())..((i + 1) * per).min(data.len())];
        let mut payload = Vec::with_capacity(CHUNK_HEADER + chunk.len());
        payload.push(VERSION);
        payload.extend_from_slice(&(i as u32).to_le_bytes());
        payload.extend_from_slice(&total32.to_le_bytes());
        payload.extend_from_slice(&(data.len() as u64).to_le_bytes());
        payload.extend_from_slice(&crc.to_le_bytes());
        payload.extend_from_slice(chunk);
        frame::encode(&payload, width, height, p)
    }))
}

/// Collects frames (in any order, with duplicates) and rebuilds the file.
#[derive(Default)]
pub struct Joiner {
    total: Option<u32>,
    file_len: u64,
    file_crc: u32,
    chunks: BTreeMap<u32, Vec<u8>>,
    /// Bytes Reed-Solomon repaired across all accepted frames.
    pub corrected_bytes: usize,
}

/// What `Joiner::add` did with a frame.
#[derive(Debug, PartialEq, Eq)]
pub enum Added {
    New,
    Duplicate,
}

impl Joiner {
    pub fn new() -> Self {
        Self::default()
    }

    /// Decode one (already unscaled) frame and file its chunk.
    pub fn add(&mut self, img: &RgbImage) -> Result<Added> {
        let d = frame::decode_report(img)?;
        let p = &d.data;
        if p.len() < CHUNK_HEADER || p[0] != VERSION {
            return Err(Error::Inconsistent("frame has no valid chunk header".into()));
        }
        let index = u32::from_le_bytes(p[1..5].try_into().unwrap());
        let total = u32::from_le_bytes(p[5..9].try_into().unwrap());
        let file_len = u64::from_le_bytes(p[9..17].try_into().unwrap());
        let file_crc = u32::from_le_bytes(p[17..21].try_into().unwrap());
        if total == 0 || index >= total {
            return Err(Error::Inconsistent(format!("frame index {index} out of range (total {total})")));
        }
        match self.total {
            None => {
                self.total = Some(total);
                self.file_len = file_len;
                self.file_crc = file_crc;
            }
            Some(t) if t == total && self.file_len == file_len && self.file_crc == file_crc => {}
            Some(_) => {
                return Err(Error::Inconsistent("frame belongs to a different file".into()));
            }
        }
        if self.chunks.contains_key(&index) {
            return Ok(Added::Duplicate);
        }
        self.corrected_bytes += d.corrected_bytes;
        self.chunks.insert(index, p[CHUNK_HEADER..].to_vec());
        Ok(Added::New)
    }

    pub fn is_complete(&self) -> bool {
        self.total.is_some_and(|t| self.chunks.len() == t as usize)
    }

    /// Reassemble and verify the file.
    pub fn finish(self) -> Result<Vec<u8>> {
        let total = self.total.ok_or_else(|| Error::Inconsistent("no valid frames found".into()))?;
        let missing: Vec<u32> = (0..total).filter(|i| !self.chunks.contains_key(i)).collect();
        if !missing.is_empty() {
            return Err(Error::MissingFrames { missing, total });
        }
        let out: Vec<u8> = self.chunks.into_values().flatten().collect();
        if out.len() as u64 != self.file_len {
            return Err(Error::Inconsistent(format!(
                "reassembled {} bytes but the header says {}", out.len(), self.file_len
            )));
        }
        let actual = CRC_32.checksum(&out);
        if actual != self.file_crc {
            return Err(Error::PayloadCrc { expected: self.file_crc, actual });
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::frame::Mode;
    use image::Rgb;

    fn params() -> Params {
        Params { mode: Mode::Gray, ecc: 32, bits: 8 }
    }

    fn sample(n: usize) -> Vec<u8> {
        (0..n).map(|i| (i * 7 + i / 251) as u8).collect()
    }

    fn frames(data: &[u8]) -> Vec<RgbImage> {
        split(data, 64, 32, &params()).unwrap().map(|f| f.unwrap()).collect()
    }

    fn join(frames: &[RgbImage]) -> Result<Vec<u8>> {
        let mut j = Joiner::new();
        for f in frames {
            j.add(f)?;
        }
        j.finish()
    }

    #[test]
    fn splits_and_rejoins_across_many_frames() {
        let per = chunk_capacity(64, 32, &params()).unwrap();
        for len in [0, 1, per - 1, per, per + 1, per * 5 + 17] {
            let data = sample(len);
            let fs = frames(&data);
            assert_eq!(fs.len(), frames_needed(len, 64, 32, &params()).unwrap());
            assert_eq!(join(&fs).unwrap(), data, "len {len}");
        }
    }

    #[test]
    fn order_and_duplicates_do_not_matter() {
        let data = sample(5000);
        let mut fs = frames(&data);
        assert!(fs.len() > 3);
        fs.reverse();
        let dup = fs[0].clone();
        fs.push(dup);
        let mut j = Joiner::new();
        let mut dups = 0;
        for f in &fs {
            if j.add(f).unwrap() == Added::Duplicate {
                dups += 1;
            }
        }
        assert_eq!(dups, 1);
        assert!(j.is_complete());
        assert_eq!(j.finish().unwrap(), data);
    }

    #[test]
    fn missing_frames_are_reported_by_index() {
        let data = sample(5000);
        let mut fs = frames(&data);
        let total = fs.len() as u32;
        fs.remove(2);
        match join(&fs) {
            Err(Error::MissingFrames { missing, total: t }) => {
                assert_eq!(missing, vec![2]);
                assert_eq!(t, total);
            }
            other => panic!("expected MissingFrames, got {other:?}"),
        }
    }

    #[test]
    fn damaged_frame_is_repaired_or_reported() {
        let data = sample(3000);
        let mut fs = frames(&data);
        fs[1].put_pixel(10, 5, Rgb([0, 0, 0])); // light damage: repaired
        assert_eq!(join(&fs).unwrap(), data);
        for px in fs[1].pixels_mut() {
            *px = Rgb([77, 77, 77]); // destroyed
        }
        assert!(join(&fs).is_err());
    }

    #[test]
    fn frames_from_different_files_are_rejected() {
        let a = frames(&sample(3000));
        let b = frames(&sample(3001));
        let mut j = Joiner::new();
        j.add(&a[0]).unwrap();
        assert!(matches!(j.add(&b[1]), Err(Error::Inconsistent(_))));
    }

    #[test]
    fn works_with_quantized_scaled_frames() {
        let p = Params { mode: Mode::Gray, ecc: 32, bits: 1 };
        let data = sample(4000);
        let fs: Vec<RgbImage> = split(&data, 160, 90, &p)
            .unwrap()
            .map(|f| crate::nnscale::rescale(&f.unwrap(), Some(4), "nearest").unwrap())
            .collect();
        let mut j = Joiner::new();
        for f in &fs {
            let scale = frame::detect_scale(f).expect("scale detected");
            assert_eq!(scale, 4);
            j.add(&crate::nnscale::downscale(f, scale).unwrap()).unwrap();
        }
        assert_eq!(j.finish().unwrap(), data);
    }
}
