use image::{ImageBuffer, Luma, Rgb};
use std::vec::Vec;
use std::io::{self};

pub fn new_image(width: Option<u32>, height: Option<u32>, data: Vec<u8>) -> io::Result<ImageBuffer<Luma<u8>, Vec<u8>>>{
    let mut byte_img = ImageBuffer::new(width.unwrap_or(240), height.unwrap_or(135)); // Default image size is 240x135

    for (_x, _y, pixel) in byte_img.enumerate_pixels_mut() {
        *pixel = image::Luma([0]);
        // *pixel = image::Rgb([0, 0, 0]);  // Set all pixels to black in RGB mode
    }

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

pub fn rescale(img: &ImageBuffer<Rgb<u8>, Vec<u8>>, scale: Option<u32>, scale_type: &str) -> io::Result<ImageBuffer<Rgb<u8>, Vec<u8>>>{
    let (width, height) = img.dimensions();
    let mut rescaled_img: ImageBuffer<Rgb<u8>, Vec<u8>> = ImageBuffer::new(width*scale.unwrap_or(8), height*scale.unwrap_or(8));

    // TO-DO: Implement other scaling types
    // TO-DO: Clean up/optimize nearest neighbor scaling algorithm
    if scale_type == "nearest" {
        for (_x, _y, pixel) in rescaled_img.enumerate_pixels_mut() {
            *pixel = image::Rgb([0, 0, 0]);     // Set all pixels to black in RGB for now
        }

        for (x, y, _pixel_img) in img.enumerate_pixels() {  // Run through image, set each pixel as the contents of the vector(data)
            for xs in x*scale.unwrap()..(x+1)*scale.unwrap() {
                for ys in y*scale.unwrap()..(y+1)*scale.unwrap() {
                    let pixel = rescaled_img.get_pixel_mut(xs, ys);
                    let image::Rgb(data) = img.get_pixel(x, y); // Get RGB value of img

                    *pixel = image::Rgb([data[0], data[1], data[2]]);  // Set pixel to RGB value
                }
            }
        }
    }

    else {
        panic!("Scale type not supported or not specified!")
    }

    Ok(rescaled_img)
}