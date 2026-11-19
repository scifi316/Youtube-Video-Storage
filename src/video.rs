// Turn a file into a video of numbered data frames, and back, using the ffmpeg command line.
// Frames are streamed through ffmpeg's stdin/stdout as raw RGB, so nothing is written to disk and
// memory use is one frame at a time, however large the file.
use crate::container::{self, Added, Joiner, Redundancy};
use crate::error::{Error, Result};
use crate::frame::{self, Params};
use crate::nnscale::{downscale, rescale};
use std::ffi::OsString;
use std::io::{Read, Write};
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::thread::JoinHandle;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Codec {
    X264,
    X265,
}

/// How the encoder spends bits.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Rate {
    Bitrate(String),
    /// Constant quality: lower is better quality and a bigger file (x264 default 23).
    Crf(u8),
}

#[derive(Clone, Debug)]
pub struct VideoOpts {
    pub params: Params,
    /// Size of one data frame before scaling.
    pub size: (u32, u32),
    /// Integer enlargement; the video is size * scale.
    pub scale: u32,
    pub codec: Codec,
    /// Target bitrate such as "8M", or constant quality (`--crf`).
    pub rate: Rate,
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

fn ffmpeg_missing(e: std::io::Error, tool: &str) -> Error {
    match e.kind() {
        std::io::ErrorKind::NotFound => Error::External(format!("{tool} not found; install ffmpeg and put it on PATH")),
        _ => Error::Io(e),
    }
}

/// Start ffmpeg with a piped stdin or stdout. A thread drains stderr so a chatty ffmpeg can't block.
fn spawn_ffmpeg(args: &[OsString], pipe_stdin: bool, pipe_stdout: bool) -> Result<(Child, JoinHandle<String>)> {
    let mut child = Command::new("ffmpeg")
        .args(["-y", "-loglevel", "error"])
        .args(args)
        .stdin(if pipe_stdin { Stdio::piped() } else { Stdio::null() })
        .stdout(if pipe_stdout { Stdio::piped() } else { Stdio::null() })
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| ffmpeg_missing(e, "ffmpeg"))?;
    let mut stderr = child.stderr.take().expect("stderr is piped");
    let drain = std::thread::spawn(move || {
        let mut msg = String::new();
        let _ = stderr.read_to_string(&mut msg);
        msg
    });
    Ok((child, drain))
}

fn ffmpeg_failure(msg: String) -> Error {
    Error::External(format!("ffmpeg failed: {}", msg.trim()))
}

/// Width and height of the first video stream.
fn probe_size(video: &Path) -> Result<(usize, usize)> {
    let out = Command::new("ffprobe")
        .args(["-v", "error", "-select_streams", "v:0", "-show_entries", "stream=width,height", "-of", "csv=p=0"])
        .arg(video)
        .output()
        .map_err(|e| ffmpeg_missing(e, "ffprobe"))?;
    let text = String::from_utf8_lossy(&out.stdout);
    let mut nums = text.trim().split(',').filter_map(|n| n.trim().parse::<usize>().ok());
    match (out.status.success(), nums.next(), nums.next()) {
        (true, Some(w), Some(h)) if w > 0 && h > 0 => Ok((w, h)),
        _ => Err(Error::External(format!(
            "could not read the video size of {}: {}",
            video.display(),
            String::from_utf8_lossy(&out.stderr).trim()
        ))),
    }
}

/// Number of video frames the data will need (before `hold` repetition).
pub fn plan(len: usize, o: &VideoOpts) -> Result<usize> {
    container::frames_needed(len, o.size.0, o.size.1, &o.params, o.redundancy)
}

/// Encode `data` into a video file, streaming frames into ffmpeg. Returns the number of distinct
/// frames (data plus parity).
pub fn encode_video(data: &[u8], out: &Path, o: &VideoOpts) -> Result<usize> {
    if o.fps == 0 || o.hold == 0 || o.scale == 0 {
        return Err(Error::InvalidParameter("fps, hold and scale must be at least 1".into()));
    }
    let (vw, vh) = (o.size.0 * o.scale, o.size.1 * o.scale);
    if vw % 2 != 0 || vh % 2 != 0 {
        return Err(Error::BadDimensions(format!("video size {vw}x{vh} must have even width and height")));
    }
    let frames = container::split(data, o.size.0, o.size.1, &o.params, o.redundancy)?;

    let mut args: Vec<OsString> = ["-f", "rawvideo", "-pix_fmt", "rgb24", "-s"].map(OsString::from).to_vec();
    args.push(format!("{vw}x{vh}").into());
    args.extend(["-framerate".into(), format!("{}/{}", o.fps, o.hold).into(), "-i".into(), "-".into()]);
    args.extend(["-r".into(), o.fps.to_string().into()]);
    let codec_args: &[&str] = match o.codec {
        Codec::X264 => &["-c:v", "libx264", "-preset", "medium"],
        Codec::X265 => &["-c:v", "libx265", "-preset", "fast", "-x265-params", "log-level=error"],
    };
    args.extend(codec_args.iter().map(OsString::from));
    match &o.rate {
        Rate::Bitrate(b) => args.extend(["-b:v".into(), b.into()]),
        Rate::Crf(c) => args.extend(["-crf".into(), c.to_string().into()]),
    }
    args.extend(["-pix_fmt", "yuv420p"].map(OsString::from));
    args.push(out.into());

    let (mut child, stderr) = spawn_ffmpeg(&args, true, false)?;
    let mut stdin = child.stdin.take().expect("stdin is piped");
    let mut count = 0;
    let mut write_failed = false;
    for frame in frames {
        let img = match frame.and_then(|f| rescale(&f, Some(o.scale), "nearest")) {
            Ok(img) => img,
            Err(e) => {
                drop(stdin);
                let _ = child.kill();
                let _ = child.wait();
                let _ = stderr.join();
                return Err(e);
            }
        };
        if stdin.write_all(img.as_raw()).is_err() {
            write_failed = true; // ffmpeg exited early; its stderr says why
            break;
        }
        count += 1;
    }
    drop(stdin);
    let status = child.wait()?;
    let msg = stderr.join().unwrap_or_default();
    if !status.success() || write_failed {
        return Err(ffmpeg_failure(msg));
    }
    Ok(count)
}

/// Decode a video produced by `encode_video` (or a re-encode of it, such as a YouTube download),
/// streaming frames out of ffmpeg one at a time. `scale = None` detects the block size automatically.
pub fn decode_video(video: &Path, scale: Option<u32>) -> Result<(Vec<u8>, DecodeStats)> {
    let (w, h) = probe_size(video)?;
    let args: Vec<OsString> = ["-i"]
        .map(OsString::from)
        .into_iter()
        .chain([video.as_os_str().to_owned()])
        .chain(["-fps_mode", "passthrough", "-f", "rawvideo", "-pix_fmt", "rgb24", "-"].map(OsString::from))
        .collect();
    let (mut child, stderr) = spawn_ffmpeg(&args, false, true)?;
    let mut stdout = child.stdout.take().expect("stdout is piped");

    let mut stats = DecodeStats { scale: scale.unwrap_or(0), ..Default::default() };
    let mut joiner = Joiner::new();
    let mut last_err: Option<Error> = None;
    loop {
        let mut buf = vec![0u8; w * h * 3];
        // Fill one frame; a short read means the stream ended (a cut-off final frame is dropped).
        let mut filled = 0;
        while filled < buf.len() {
            match stdout.read(&mut buf[filled..]) {
                Ok(0) => break,
                Ok(n) => filled += n,
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
                Err(e) => return Err(e.into()),
            }
        }
        if filled < buf.len() {
            break;
        }
        stats.video_frames += 1;
        let img = image::RgbImage::from_raw(w as u32, h as u32, buf).expect("buffer matches size");
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
            break; // everything we need; don't decode the rest of the video
        }
    }
    drop(stdout);
    let _ = child.kill(); // no-op if ffmpeg already finished
    let status = child.wait()?;
    let msg = stderr.join().unwrap_or_default();
    if stats.video_frames == 0 && !status.success() {
        return Err(ffmpeg_failure(msg));
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
    use std::path::PathBuf;

    /// Temp directory removed on drop.
    struct TempDir(PathBuf);

    impl TempDir {
        fn new() -> Result<Self> {
            let nanos = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.subsec_nanos())
                .unwrap_or(0);
            let dir = std::env::temp_dir().join(format!("yvs_{}_{nanos}", std::process::id()));
            std::fs::create_dir_all(&dir)?;
            Ok(TempDir(dir))
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

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
            rate: Rate::Bitrate("8M".into()),
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
