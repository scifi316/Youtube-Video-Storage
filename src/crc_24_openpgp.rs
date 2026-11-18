// Per-column CRC-24/OPENPGP for RGB frames. The CRC of every column (all rows except the
// last) is stored as an RGB pixel in the last row of that column.
use crc::{Crc, CRC_24_OPENPGP};
use image::{Rgb, RgbImage};

pub const CRC_24: Crc<u32> = Crc::<u32>::new(&CRC_24_OPENPGP);

/// CRC-24 of column `x`, covering every row except the final (CRC) row.
pub fn gen_col_crc24(img: &RgbImage, x: u32) -> [u8; 3] {
    let mut crc = CRC_24.digest();
    for y in 0..img.height().saturating_sub(1) {
        crc.update(&img.get_pixel(x, y).0);
    }
    let b = crc.finalize().to_le_bytes();
    [b[0], b[1], b[2]]
}

/// Write each column's CRC into the last row of the image.
pub fn write_crcs(img: &mut RgbImage) {
    let last = img.height() - 1;
    for x in 0..img.width() {
        let crc = gen_col_crc24(img, x);
        img.put_pixel(x, last, Rgb(crc));
    }
}

/// Return the x-coordinates of all columns whose stored CRC does not match.
pub fn bad_columns(img: &RgbImage) -> Vec<u32> {
    if img.height() < 2 {
        return (0..img.width()).collect();
    }
    let last = img.height() - 1;
    (0..img.width())
        .filter(|&x| img.get_pixel(x, last).0 != gen_col_crc24(img, x))
        .collect()
}

