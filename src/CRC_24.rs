// Cyclic Redundancy Check (CRC) implementation for pixel columns on PNG images.
use image::{ImageBuffer, Luma, Rgb, Pixel};    // Implementation for Luma<u8> and Rgb<u8> images
use crc::{Crc,  CRC_24_OPENPGP};
use std::io::{self};

pub const CRC_24: Crc<u32> = Crc::<u32>::new(&CRC_24_OPENPGP);

pub fn gen_col_crc24(img: &ImageBuffer<Luma<u8>, Vec<u8>>, x_col: u32) -> io::Result<Vec<u8>>{
    let mut crc = CRC_24.digest();  // Initialize CRC

    for y in 0..img.height() {
        let pixel = img.get_pixel(x_col, y);
        let image::Luma(data) = *pixel;

        crc.update(&data);  // Update CRC with pixel data of entire column
    }

    let crc_u32 = crc.finalize();  // Return CRC as a vector of bytes (u8) 

    //let mut crc_out = Vec::new();

    // Slice CRC into 3 bytes (24 bits)
     let crc_out = crc_u32.to_le_bytes()[0..3].to_vec();

    Ok(crc_out)
}

// Open image, generate CRC for each column, and return a new image with the CRC values as the pixel data on last row
pub fn gen_img_crc(img: &ImageBuffer<Luma<u8>, Vec<u8>>) -> io::Result<ImageBuffer<Rgb<u8>, Vec<u8>>> {
    let (width, height) = img.dimensions();
    let mut crc_img: ImageBuffer<Rgb<u8>, Vec<u8>> = ImageBuffer::new(width, height);

    for (x, y, pixel) in crc_img.enumerate_pixels_mut() {
        *pixel = img.get_pixel(x, y).to_rgb();
    }

    for x in 0..img.width() {
        let crc = gen_col_crc24(img, x);
        let pixel = crc_img.get_pixel_mut(x, height-1);

        *pixel = image::Rgb([crc.as_ref().unwrap()[0], crc.as_ref().unwrap()[1], crc.as_ref().unwrap()[2]]);  // Set pixel to CRC value
        // *pixel = image::Rgb([0, 0, 0]);  // Set all pixels to black in RGB mode
    }

    Ok(crc_img)
}