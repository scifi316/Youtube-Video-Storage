// Multi-frame container: splits a file over numbered frames and puts it back together.
//
// Frames are arranged in groups of `group` data frames followed by `parity` parity frames. Parity
// frames are Reed-Solomon codewords taken across the data frames of the group (byte j of every
// frame forms one codeword), so ANY `parity` lost frames per group can be rebuilt. The final group
// may hold fewer data frames. `parity = 0` disables this.
//
// Each frame's payload (itself Reed-Solomon protected by `frame`) starts with a chunk header:
//   version u8 (=2) | frame index u32 | total frames u32 | file length u64 | file CRC-32 u32
//   | group u8 | parity u8
// so frames can arrive in any order, duplicates are harmless (e.g. a frame shown for several video
// frames), and the finished file is checked end to end.
use crate::error::{Error, Result};
use crate::frame::{self, Params};
use crc::{Crc, CRC_32_ISO_HDLC};
use image::RgbImage;
use reed_solomon::{Decoder, Encoder};
use std::collections::BTreeMap;

pub const CHUNK_HEADER: usize = 23;
const VERSION: u8 = 2;
const CRC_32: Crc<u32> = Crc::<u32>::new(&CRC_32_ISO_HDLC);

/// Frame-level redundancy: `parity` extra frames for every `group` data frames.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Redundancy {
    pub group: u8,
    pub parity: u8,
}

impl Redundancy {
    pub const NONE: Redundancy = Redundancy { group: 10, parity: 0 };

    fn validate(&self) -> Result<()> {
        if self.group == 0 || self.group as usize + self.parity as usize > 255 {
            return Err(Error::InvalidParameter(format!(
                "need group >= 1 and group + parity <= 255, got group {} parity {}",
                self.group, self.parity
            )));
        }
        Ok(())
    }
}

/// File bytes carried by each frame.
pub fn chunk_capacity(width: u32, height: u32, p: &Params) -> Result<usize> {
    let cap = frame::capacity(width, height, p)?;
    cap.checked_sub(CHUNK_HEADER)
        .filter(|&c| c > 0)
        .ok_or_else(|| Error::BadDimensions(format!("{width}x{height} is too small to carry data")))
}

/// Number of data frames `len` bytes occupy (at least one, even for an empty file).
fn data_frames(len: usize, per: usize) -> usize {
    len.div_ceil(per).max(1)
}

/// Total frames, including parity, that `len` bytes will occupy.
pub fn frames_needed(len: usize, width: u32, height: u32, p: &Params, red: Redundancy) -> Result<usize> {
    red.validate()?;
    let d = data_frames(len, chunk_capacity(width, height, p)?);
    Ok(d + d.div_ceil(red.group as usize) * red.parity as usize)
}

fn chunk_of(data: &[u8], per: usize, k: usize) -> &[u8] {
    &data[(k * per).min(data.len())..((k + 1) * per).min(data.len())]
}

/// Lazily yield the frames for `data`, one at a time (so huge files don't sit in memory as images).
pub fn split<'a>(
    data: &'a [u8],
    width: u32,
    height: u32,
    p: &'a Params,
    red: Redundancy,
) -> Result<impl Iterator<Item = Result<RgbImage>> + 'a> {
    red.validate()?;
    let per = chunk_capacity(width, height, p)?;
    let (g, pp) = (red.group as usize, red.parity as usize);
    let d_total = data_frames(data.len(), per);
    let total = d_total + d_total.div_ceil(g) * pp;
    let total32 = u32::try_from(total).map_err(|_| Error::Overflow { capacity: u32::MAX as usize, len: total })?;
    let crc = CRC_32.checksum(data);

    let mut next = 0usize;
    let mut cache: Option<(usize, Vec<Vec<u8>>)> = None; // (group, its parity chunks)
    Ok(std::iter::from_fn(move || {
        if next >= total {
            return None;
        }
        let idx = next;
        next += 1;
        let (gi, pos) = (idx / (g + pp), idx % (g + pp));
        let first = gi * g;
        let d = (d_total - first).min(g);

        let body: Vec<u8> = if pos < d {
            chunk_of(data, per, first + pos).to_vec()
        } else {
            if cache.as_ref().map(|c| c.0) != Some(gi) {
                let enc = Encoder::new(pp);
                let mut parity = vec![vec![0u8; per]; pp];
                for j in 0..per {
                    let col: Vec<u8> = (0..d)
                        .map(|k| chunk_of(data, per, first + k).get(j).copied().unwrap_or(0))
                        .collect();
                    let e = enc.encode(&col);
                    for (q, &b) in e.ecc().iter().enumerate() {
                        parity[q][j] = b;
                    }
                }
                cache = Some((gi, parity));
            }
            cache.as_ref().unwrap().1[pos - d].clone()
        };

        let mut payload = Vec::with_capacity(CHUNK_HEADER + body.len());
        payload.push(VERSION);
        payload.extend_from_slice(&(idx as u32).to_le_bytes());
        payload.extend_from_slice(&total32.to_le_bytes());
        payload.extend_from_slice(&(data.len() as u64).to_le_bytes());
        payload.extend_from_slice(&crc.to_le_bytes());
        payload.push(red.group);
        payload.push(red.parity);
        payload.extend_from_slice(&body);
        Some(frame::encode(&payload, width, height, p))
    }))
}

/// Collects frames (in any order, with duplicates) and rebuilds the file.
#[derive(Default)]
pub struct Joiner {
    total: Option<u32>,
    file_len: u64,
    file_crc: u32,
    group: usize,
    parity: usize,
    chunks: BTreeMap<u32, Vec<u8>>,
    /// Bytes Reed-Solomon repaired inside accepted frames.
    pub corrected_bytes: usize,
    /// Whole frames rebuilt from parity (set by `finish`).
    pub recovered_frames: usize,
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

    /// (first frame index, frames in the group, data frames in the group)
    fn group_range(&self, gi: usize) -> (usize, usize, usize) {
        let total = self.total.unwrap() as usize;
        let start = gi * (self.group + self.parity);
        let n = (total - start).min(self.group + self.parity);
        (start, n, n - self.parity)
    }

    fn groups(&self) -> usize {
        (self.total.unwrap() as usize).div_ceil(self.group + self.parity)
    }

    fn has(&self, i: usize) -> bool {
        self.chunks.contains_key(&(i as u32))
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
        let red = Redundancy { group: p[21], parity: p[22] };
        red.validate().map_err(|e| Error::Inconsistent(e.to_string()))?;
        let (group, parity) = (red.group as usize, red.parity as usize);
        if total == 0 || index >= total {
            return Err(Error::Inconsistent(format!("frame index {index} out of range (total {total})")));
        }
        let full = group + parity;
        let last_n = total as usize - (total as usize).div_ceil(full).saturating_sub(1) * full;
        if last_n <= parity {
            return Err(Error::Inconsistent("frame count does not fit the group layout".into()));
        }
        match self.total {
            None => {
                (self.total, self.file_len, self.file_crc) = (Some(total), file_len, file_crc);
                (self.group, self.parity) = (group, parity);
            }
            Some(t)
                if t == total
                    && self.file_len == file_len
                    && self.file_crc == file_crc
                    && (self.group, self.parity) == (group, parity) => {}
            Some(_) => return Err(Error::Inconsistent("frame belongs to a different file".into())),
        }
        if self.chunks.contains_key(&index) {
            return Ok(Added::Duplicate);
        }
        self.corrected_bytes += d.corrected_bytes;
        self.chunks.insert(index, p[CHUNK_HEADER..].to_vec());
        Ok(Added::New)
    }

    /// True once every group has enough frames (data or parity) to rebuild the file.
    pub fn is_complete(&self) -> bool {
        self.total.is_some()
            && (0..self.groups()).all(|gi| {
                let (start, n, d) = self.group_range(gi);
                (start..start + n).filter(|&i| self.has(i)).count() >= d
            })
    }

    /// Rebuild the missing data frames of one group from its present frames.
    fn recover_group(&mut self, start: usize, n: usize, d: usize) -> Result<()> {
        let missing: Vec<usize> = (0..n).filter(|&j| !self.has(start + j)).collect();
        let lost_data: Vec<usize> = missing.iter().copied().filter(|&j| j < d).collect();
        let erasures: Vec<u8> = missing.iter().map(|&j| j as u8).collect();
        let per = (d..n)
            .filter(|&j| self.has(start + j))
            .map(|j| self.chunks[&((start + j) as u32)].len())
            .max()
            .unwrap_or(0);
        let sizes_ok = (0..n)
            .filter(|&j| self.has(start + j))
            .all(|j| {
                let len = self.chunks[&((start + j) as u32)].len();
                if j < d { len <= per } else { len == per }
            });
        if !sizes_ok {
            return Err(Error::Inconsistent("frame sizes within a group disagree".into()));
        }
        let dec = Decoder::new(self.parity);
        let mut rebuilt = vec![vec![0u8; per]; lost_data.len()];
        for j in 0..per {
            let cw: Vec<u8> = (0..n)
                .map(|k| self.chunks.get(&((start + k) as u32)).and_then(|c| c.get(j)).copied().unwrap_or(0))
                .collect();
            let fixed = dec
                .correct(&cw, Some(&erasures))
                .map_err(|_| Error::Inconsistent("parity decode failed".into()))?;
            for (r, &e) in lost_data.iter().enumerate() {
                rebuilt[r][j] = fixed.data()[e];
            }
        }
        for (r, &e) in lost_data.iter().enumerate() {
            self.chunks.insert((start + e) as u32, std::mem::take(&mut rebuilt[r]));
            self.recovered_frames += 1;
        }
        Ok(())
    }

    /// Reassemble and verify the file, rebuilding lost frames from parity where possible.
    pub fn finish(&mut self) -> Result<Vec<u8>> {
        let total = self.total.ok_or_else(|| Error::Inconsistent("no valid frames found".into()))?;
        let mut unrecoverable = Vec::new();
        for gi in 0..self.groups() {
            let (start, n, d) = self.group_range(gi);
            let absent: Vec<u32> = (start..start + n).filter(|&i| !self.has(i)).map(|i| i as u32).collect();
            if n - absent.len() < d {
                unrecoverable.extend(absent);
            } else if absent.iter().any(|&i| (i as usize) < start + d) {
                self.recover_group(start, n, d)?;
            }
        }
        if !unrecoverable.is_empty() {
            return Err(Error::MissingFrames { missing: unrecoverable, total });
        }
        let mut out = Vec::new();
        for gi in 0..self.groups() {
            let (start, _, d) = self.group_range(gi);
            for k in 0..d {
                out.extend_from_slice(&self.chunks[&((start + k) as u32)]);
            }
        }
        if (out.len() as u64) < self.file_len {
            return Err(Error::Inconsistent(format!(
                "reassembled {} bytes but the header says {}",
                out.len(),
                self.file_len
            )));
        }
        out.truncate(self.file_len as usize);
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
        split(data, 64, 32, &params(), Redundancy::NONE).unwrap().map(|f| f.unwrap()).collect()
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
            assert_eq!(fs.len(), frames_needed(len, 64, 32, &params(), Redundancy::NONE).unwrap());
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
        let fs: Vec<RgbImage> = split(&data, 160, 90, &p, Redundancy::NONE)
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

    // ---- parity ----

    fn pframes(data: &[u8], red: Redundancy) -> Vec<RgbImage> {
        split(data, 64, 32, &params(), red).unwrap().map(|f| f.unwrap()).collect()
    }

    fn join_without(fs: &[RgbImage], lost: &[usize]) -> (Result<Vec<u8>>, usize) {
        let mut j = Joiner::new();
        for (i, f) in fs.iter().enumerate() {
            if !lost.contains(&i) {
                j.add(f).unwrap();
            }
        }
        let r = j.finish();
        (r, j.recovered_frames)
    }

    fn per() -> usize {
        chunk_capacity(64, 32, &params()).unwrap()
    }

    #[test]
    fn frame_count_includes_parity() {
        let red = Redundancy { group: 4, parity: 2 };
        // 9 data frames -> 3 groups (4+4+1) -> 6 parity frames
        let len = per() * 8 + 5;
        assert_eq!(frames_needed(len, 64, 32, &params(), red).unwrap(), 9 + 6);
        assert_eq!(pframes(&sample(len), red).len(), 15);
    }

    #[test]
    fn any_single_lost_frame_is_recovered() {
        let data = sample(per() * 7 + 100);
        let fs = pframes(&data, Redundancy { group: 3, parity: 1 });
        for lost in 0..fs.len() {
            assert_eq!(join_without(&fs, &[lost]).0.unwrap(), data, "lost frame {lost}");
        }
    }

    #[test]
    fn up_to_parity_losses_per_group_recovered_exhaustively() {
        let data = sample(per() * 4 + 9); // 5 data frames
        let fs = pframes(&data, Redundancy { group: 3, parity: 2 });
        assert_eq!(fs.len(), 5 + 4); // groups: 3+2 and 2+2
        for a in 0..fs.len() {
            for b in a + 1..fs.len() {
                assert_eq!(join_without(&fs, &[a, b]).0.unwrap(), data, "lost {a} and {b}");
            }
        }
    }

    #[test]
    fn recovery_counts_rebuilt_frames() {
        let data = sample(per() * 5);
        let fs = pframes(&data, Redundancy { group: 5, parity: 1 });
        let (r, rec) = join_without(&fs, &[2]);
        assert_eq!(r.unwrap(), data);
        assert_eq!(rec, 1);
        let (r, rec) = join_without(&fs, &[5]); // only the parity frame lost
        assert_eq!(r.unwrap(), data);
        assert_eq!(rec, 0);
    }

    #[test]
    fn too_many_losses_in_one_group_are_reported() {
        let data = sample(per() * 6);
        let fs = pframes(&data, Redundancy { group: 3, parity: 1 }); // 2 groups of 3+1
        match join_without(&fs, &[0, 1]).0 {
            Err(Error::MissingFrames { missing, total }) => {
                assert_eq!(missing, vec![0, 1]);
                assert_eq!(total, 8);
            }
            other => panic!("expected MissingFrames, got {other:?}"),
        }
        assert_eq!(join_without(&fs, &[0, 5]).0.unwrap(), data); // one loss in each group is fine
    }

    #[test]
    fn is_complete_accounts_for_parity() {
        let fs = pframes(&sample(per() * 3), Redundancy { group: 3, parity: 1 });
        let mut j = Joiner::new();
        for i in [0, 1, 3] {
            j.add(&fs[i]).unwrap();
        }
        assert!(j.is_complete()); // 2 data + 1 parity is enough for 3 data frames
        let mut j = Joiner::new();
        for i in [0, 1] {
            j.add(&fs[i]).unwrap();
        }
        assert!(!j.is_complete());
    }

    #[test]
    fn empty_file_with_parity() {
        let fs = pframes(&[], Redundancy { group: 4, parity: 2 });
        assert_eq!(fs.len(), 3);
        assert_eq!(join_without(&fs, &[0, 1]).0.unwrap(), Vec::<u8>::new());
    }

    #[test]
    fn invalid_redundancy_is_rejected() {
        let p = params();
        assert!(split(&[1], 64, 32, &p, Redundancy { group: 0, parity: 1 }).is_err());
        assert!(split(&[1], 64, 32, &p, Redundancy { group: 200, parity: 100 }).is_err());
    }
}
