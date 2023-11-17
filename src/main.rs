// Parse text file into byte stream
use std::fs::File;
use std::io::{self, Read};

fn main() -> io::Result<()> {
    let file_path = "input.txt";
    let mut file = File::open(file_path)?;
    let mut content = String::new();    // For now, testing with string buffer

    file.read_to_string(&mut content)?; // Read file into buffer

    println!("File content: {}", content);

    Ok(())
}