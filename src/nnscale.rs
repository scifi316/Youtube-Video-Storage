// Scaling of RGB frames. `rescale` enlarges by an integer factor (nearest neighbor);
// `downscale` reverses it by averaging the centre of each block, which tolerates
// small amounts of noise (e.g. from lossy video compression).
use crate::error::{Error, Result};
use image::{Rgb, RgbImage};

pub fn rescale(img: &RgbImage, scale: Option<u32>, scale_type: &str) -> Result<RgbImage> {
    let scale = scale.unwrap_or(8);
    if scale == 0 {
        return Err(Error::Scale("scale factor must be at least 1".into()));
    }
    if scale_type != "nearest" {
        return Err(Error::Scale(format!("unsupported scale type '{scale_type}'")));
    }
    let (w, h) = img.dimensions();
    let (nw, nh) = match (w.checked_mul(scale), h.checked_mul(scale)) {
        (Some(a), Some(b)) => (a, b),
        _ => return Err(Error::Scale("scaled dimensions overflow".into())),
    };
    let mut out = RgbImage::new(nw, nh);
    for (x, y, pixel) in out.enumerate_pixels_mut() {
        *pixel = *img.get_pixel(x / scale, y / scale);
    }
    Ok(out)
}

pub fn downscale(img: &RgbImage, scale: u32) -> Result<RgbImage> {
    if scale == 0 {
        return Err(Error::Scale("scale factor must be at least 1".into()));
    }
    let (w, h) = img.dimensions();
    if w % scale != 0 || h % scale != 0 {
        return Err(Error::BadDimensions(format!(
            "{w}x{h} is not divisible by scale {scale}"
        )));
    }
    let margin = scale / 4; // ignore block edges, where compression bleeds colours
    let (lo, hi) = (margin, scale - margin);
    let n = ((hi - lo) * (hi - lo)) as u32;
    let mut out = RgbImage::new(w / scale, h / scale);
    for (x, y, pixel) in out.enumerate_pixels_mut() {
        let mut sum = [0u32; 3];
        for dy in lo..hi {
            for dx in lo..hi {
                let p = img.get_pixel(x * scale + dx, y * scale + dy);
                for c in 0..3 {
                    sum[c] += p.0[c] as u32;
                }
            }
        }
        *pixel = Rgb([0, 1, 2].map(|c| ((sum[c] + n / 2) / n) as u8));
    }
    Ok(out)
}
