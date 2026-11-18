// Turn a file into a video of numbered data frames, and back, using the ffmpeg command line.
use crate::container::{self, Added, Joiner, Redundancy};
use crate::error::{Error, Result};
use crate::frame::{self, Params};
use crate::nnscale::{downscale, rescale};
use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Codec {
    X264,
    X265,
}

#[derive(Clone, Debug)]
pub struct VideoOpts {
    pub params: Params,
    /// Size of one data frame before scaling.
    pub size: (u32, u32),
    /// Integer enlargement; the video is size * scale.
    pub scale: u32,
    pub codec: Codec,
    pub bitrate: String,
    pub fps: u32,
    /// Each frame is shown this many video frames in a row. Extra copies give the decoder more
    /// chances to read a frame the compressor mangled (at the cost of throughput).
    pub hold: u32,
    /// Parity frames added per group of data frames.
    pub redundancy: Redundancy,
}

#[derive(Debug, Default)]
pub struct DecodeStats {
    pub video_frames: usize,
    pub accepted: usize,
    pub duplicates: usize,
    pub unreadable: usize,
    pub scale: u32,
    pub corrected_bytes: usize,
    /// Frames that were lost or unreadable but rebuilt from parity.
    pub recovered_frames: usize,
}

/// Temp directory removed on drop.
struct TempDir(PathBuf);

impl TempDir {
    fn new() -> Result<Self> {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.subsec_nanos())
            .unwrap_or(0);
        let dir = std::env::temp_dir().join(format!("yvs_{}_{nanos}", std::process::id()));
        fs::create_dir_all(&dir)?;
        Ok(TempDir(dir))
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn ffmpeg(args: &[OsString]) -> Result<()> {
    let out = Command::new("ffmpeg")
        .args(["-y", "-loglevel", "error"])
        .args(args)
        .output()
        .map_err(|e| match e.kind() {
            std::io::ErrorKind::NotFound => Error::External("ffmpeg not found; install it and put it on PATH".into()),
            _ => Error::Io(e),
        })?;
    if !out.status.success() {
        return Err(Error::External(format!("ffmpeg failed: {}", String::from_utf8_lossy(&out.stderr).trim())));
    }
    Ok(())
}

/// Number of video frames the data will need (before `hold` repetition).
pub fn plan(len: usize, o: &VideoOpts) -> Result<usize> {
    container::frames_needed(len, o.size.0, o.size.1, &o.params, o.redundancy)
}

/// Encode `data` into a video file. Returns the number of distinct frames (data plus parity).
pub fn encode_video(data: &[u8], out: &Path, o: &VideoOpts) -> Result<usize> {
    if o.fps == 0 || o.hold == 0 || o.scale == 0 {
        return Err(Error::InvalidParameter("fps, hold and scale must be at least 1".into()));
    }
    let (vw, vh) = (o.size.0 * o.scale, o.size.1 * o.scale);
    if vw % 2 != 0 || vh % 2 != 0 {
        return Err(Error::BadDimensions(format!("video size {vw}x{vh} must have even width and height")));
    }
    let tmp = TempDir::new()?;
    let mut count = 0;
    for (i, frame) in container::split(data, o.size.0, o.size.1, &o.params, o.redundancy)?.enumerate() {
        let img = rescale(&frame?, Some(o.scale), "nearest")?;
        img.save(tmp.0.join(format!("f_{:06}.png", i + 1)))?;
        count += 1;
    }

    let mut args: Vec<OsString> = vec![
        "-framerate".into(),
        format!("{}/{}", o.fps, o.hold).into(),
        "-i".into(),
        tmp.0.join("f_%06d.png").into(),
        "-r".into(),
        o.fps.to_string().into(),
    ];
    let codec_args: &[&str] = match o.codec {
        Codec::X264 => &["-c:v", "libx264", "-preset", "medium"],
        Codec::X265 => &["-c:v", "libx265", "-preset", "fast", "-x265-params", "log-level=error"],
    };
    args.extend(codec_args.iter().map(OsString::from));
    args.extend(["-b:v", &o.bitrate, "-pix_fmt", "yuv420p"].map(OsString::from));
    args.push(out.into());
    ffmpeg(&args)?;
    Ok(count)
}

/// Decode a video produced by `encode_video` (or a re-encode of it, such as a YouTube download).
/// `scale = None` detects the block size automatically.
pub fn decode_video(video: &Path, scale: Option<u32>) -> Result<(Vec<u8>, DecodeStats)> {
    let tmp = TempDir::new()?;
    let pattern = tmp.0.join("o_%06d.png");
    ffmpeg(&["-i".into(), video.into(), "-fps_mode".into(), "passthrough".into(), pattern.into()])?;

    let mut files: Vec<PathBuf> = fs::read_dir(&tmp.0)?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .collect();
    files.sort();

    let mut stats = DecodeStats { video_frames: files.len(), scale: scale.unwrap_or(0), ..Default::default() };
    let mut joiner = Joiner::new();
    let mut last_err: Option<Error> = None;
    for path in &files {
        let img = image::open(path)?.to_rgb8();
        if stats.scale == 0 {
            match frame::detect_scale(&img) {
                Some(s) => stats.scale = s,
                None => {
                    stats.unreadable += 1;
                    continue;
                }
            }
        }
        let small = downscale(&img, stats.scale)?;
        match joiner.add(&small) {
            Ok(Added::New) => stats.accepted += 1,
            Ok(Added::Duplicate) => stats.duplicates += 1,
            Err(e) => {
                stats.unreadable += 1;
                last_err = Some(e);
            }
        }
        if joiner.is_complete() {
            break; // everything we need; skip the remaining duplicates
        }
    }
    stats.corrected_bytes = joiner.corrected_bytes;
    let result = joiner.finish();
    stats.recovered_frames = joiner.recovered_frames;
    match result {
        Ok(data) => Ok((data, stats)),
        Err(Error::Inconsistent(m)) if m.starts_with("no valid") => Err(match last_err {
            Some(e) => e,
            None => Error::Inconsistent(format!("no data frames found in {} video frame(s)", stats.video_frames)),
        }),
        Err(e) => Err(e),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::frame::Mode;

    fn have_x264() -> bool {
        Command::new("ffmpeg")
            .arg("-encoders")
            .output()
            .map(|o| String::from_utf8_lossy(&o.stdout).contains("libx264"))
            .unwrap_or(false)
    }

    fn opts(hold: u32) -> VideoOpts {
        VideoOpts {
            params: Params { mode: Mode::Gray, ecc: 32, bits: 1 },
            size: (480, 270),
            scale: 4,
            codec: Codec::X264,
            bitrate: "8M".into(),
            fps: 30,
            hold,
            redundancy: Redundancy::NONE,
        }
    }

    #[test]
    fn multi_frame_video_roundtrip() {
        if !have_x264() {
            eprintln!("ffmpeg with libx264 not found, skipping");
            return;
        }
        let data: Vec<u8> = (0..40_000u32).map(|i| (i.wrapping_mul(2654435761) >> 13) as u8).collect();
        let dir = TempDir::new().unwrap();
        for hold in [1, 2] {
            let path = dir.0.join(format!("v{hold}.mp4"));
            let frames = encode_video(&data, &path, &opts(hold)).unwrap();
            assert_eq!(frames, plan(data.len(), &opts(hold)).unwrap());
            assert!(frames >= 3);
            let (out, stats) = decode_video(&path, None).unwrap();
            assert_eq!(out, data, "hold {hold}");
            assert_eq!(stats.scale, 4);
            assert_eq!(stats.accepted, frames);
        }
    }

    #[test]
    fn truncated_video_reports_missing_frames() {
        if !have_x264() {
            return;
        }
        let data: Vec<u8> = (0..40_000u32).map(|i| (i.wrapping_mul(2654435761) >> 11) as u8).collect();
        let dir = TempDir::new().unwrap();
        let full = dir.0.join("full.mp4");
        let cut = dir.0.join("cut.mp4");
        encode_video(&data, &full, &opts(1)).unwrap();
        let ok = Command::new("ffmpeg")
            .args(["-y", "-loglevel", "error", "-i"])
            .arg(&full)
            .args(["-frames:v", "2", "-c", "copy"])
            .arg(&cut)
            .status()
            .unwrap()
            .success();
        assert!(ok);
        match decode_video(&cut, None) {
            Err(Error::MissingFrames { missing, .. }) => assert!(!missing.is_empty()),
            other => panic!("expected MissingFrames, got {other:?}"),
        }
    }

    #[test]
    fn lost_frame_in_video_is_rebuilt_from_parity() {
        if !have_x264() {
            return;
        }
        let data: Vec<u8> = (0..60_000u32).map(|i| (i.wrapping_mul(2654435761) >> 9) as u8).collect();
        let dir = TempDir::new().unwrap();
        let mut o = opts(1);
        o.redundancy = Redundancy { group: 4, parity: 1 };
        let full = dir.0.join("full.mp4");
        let frames = encode_video(&data, &full, &o).unwrap();
        assert!(frames >= 6);

        // Drop video frame 1 (a data frame) by re-muxing without it.
        let cut = dir.0.join("cut.mp4");
        let ok = Command::new("ffmpeg")
            .args(["-y", "-loglevel", "error", "-i"])
            .arg(&full)
            .args(["-vf", "select=not(eq(n\\,1))", "-fps_mode", "passthrough", "-c:v", "libx264", "-b:v", "8M"])
            .arg(&cut)
            .status()
            .unwrap()
            .success();
        assert!(ok);
        let (out, stats) = decode_video(&cut, None).unwrap();
        assert_eq!(out, data);
        assert_eq!(stats.recovered_frames, 1);
    }
}
