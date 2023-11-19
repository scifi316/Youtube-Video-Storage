// Cyclic Redundancy Check (CRC) implementation for pixel columns on PNG images.
use image::{ImageBuffer, Luma, Rgb};    // Implementation for Luma<u8> and Rgb<u8> images
use crc::{Crc, Algorithm, CRC_24_OPENPGP};

pub const CRC_24: Crc<u32> = Crc::<u32>::new(&CRC_24_OPENPGP);

pub fn gen_col_crc24(img: &ImageBuffer<Luma<u8>, Vec<u8>>, x_col: u32) -> Vec<u8> {
    let mut crc = CRC_24.digest();  // Initialize CRC

    for y in 0..img.height() {
        let pixel = img.get_pixel(x_col, y);
        let image::Luma(data) = *pixel;

        crc.update(&data);
    }

    crc.finalize()
}