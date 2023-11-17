use image::{GenericImage, ImageBuffer, RgbImage};
use std::vec::Vec;

pub fn new_image(width: Option<u32>, height: Option<u32>, data: Vec<u8>) {
    let mut byte_img = ImageBuffer::new(width.unwrap_or(96), height.unwrap_or(54));
    let mut i = 0;
    let mut k = 0;

    for (x, y, pixel) in byte_img.enumerate_pixels_mut() {
        *pixel = image::Rgb([0, 0, 0]);
    }

    for y in 0..54 {    // Run through image, set each pixel as the contents of the vector(data)
        for x in 0..96 {
            let pixel = byte_img.get_pixel_mut(x, y);
            // let image::Rgb(data) = *pixel;

            if i < 240-3 && k < 778-4 {    // RGB values are set as the decimal values of the vector being read for every other RGB position as to not have a repeat of colors.
                if x%3 == 0 {
                    *pixel = image::Rgb([data[0+k] as u8, 0, 0]);
                    i += 1;
                }
                else if x%3 == 1 {
                    *pixel = image::Rgb([0, data[1+k], 0]);
                    i += 1;
                }
                else if x%3 == 2 {
                    *pixel = image::Rgb([0, 0, data[2+k]]);
                    i += 1;
                }
                else {
                    *pixel = image::Rgb([0, 0, 0]);
                }
                k += 3;
            }
            else {
                *pixel = image::Rgb([0, 0, 0]);
            }
        }
        i = 0;
    }

    byte_img.save("output.png").unwrap();
}

pub fn rescale() {

}