// Parse text file into byte stream
use std::fs::File;
use std::io::{self, Read};

mod nnscale;
mod crc_24_openpgp;

// Read file into a vector of bytes
fn read_file_vec8(file_path: &str) -> io::Result<Vec<u8>> {
    let mut file = File::open(file_path)?;
    let mut content = Vec::new();

    file.read_to_end(&mut content)?;

    Ok(content)
}

// Read file into a string
// fn read_file_str(file_path: &str) -> io::Result<String> {
//     let mut file = File::open(file_path)?;
//     let mut content = String::new();

//     file.read_to_string(&mut content)?;
//     file.read_to_string(&mut content)?;

//     Ok(content)
// }

fn main() -> io::Result<()> {
    let file_path = "input.txt";
    
    let content_vec8 = read_file_vec8(file_path)?;
    //let content_str = read_file_str(file_path)?;

    // for byte in &content_vec8 {
    //     print!("{:X?} ", byte);
    // }

    //println!("\nText content: {}", content_str);

    let img = nnscale::new_image(Some(240), Some(135), content_vec8)?.clone();

    let _ = &img.save("output.png").unwrap();
    
    let crc = crc_24_openpgp::gen_col_crc24(&img, 0);

    println!("CRC: {:X?}", crc);

    let crc_img = crc_24_openpgp::gen_img_crc(&img);

    let _ = &crc_img?.save("output_crc.png").unwrap();

    Ok(())
}