use image::{ImageBuffer, Luma};
use std::vec::Vec;
use std::io::{self};

pub fn new_image(width: Option<u32>, height: Option<u32>, data: Vec<u8>) -> io::Result<ImageBuffer<Luma<u8>, Vec<u8>>>{
    let mut byte_img = ImageBuffer::new(width.unwrap_or(240), height.unwrap_or(135)); // Default image size is 96x54

    for (_x, _y, pixel) in byte_img.enumerate_pixels_mut() {
        *pixel = image::Luma([0]);
        // *pixel = image::Rgb([0, 0, 0]);  // Set all pixels to black in RGB mode
    }

    //  TO-DO
    //  Rework to allow for any size image; the vector can only be accessed as a 2D array, so coordinates must be calculated to access the correct value.
    //  Future access for variables stored in 'data' can be calculated with the following: data[(y*width)+x]
    //  Also change for loop to be able to access variable lengths of the vector, as it is currently hardcoded to 96x54
    for y in 0..height.unwrap() {    // Run through image, set each pixel as the contents of the vector(data)
        for x in 0..width.unwrap() {
            let pixel = byte_img.get_pixel_mut(x, y);
            // let image::Rgb(data) = *pixel;
            let index = match width {
                Some(w) => (y * w) + x,
                None => {
                    240 // Default width
                }
            };

            if index < data.len().try_into().unwrap() {    // RGB values are set as the decimal values of the vector being read for every other RGB position as to not have a repeat of colors.
                *pixel = image::Luma([data[(y as usize * width.unwrap() as usize) + x as usize]]);
            }
            else {
                *pixel = image::Luma([0]);
            }
        }
    }

    Ok(byte_img)
}

// pub fn rescale(scale: u32, data: Vec<u8>) {

// }