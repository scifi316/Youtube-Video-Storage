// Parse text file into byte stream
use std::fs:File;
use std::io::Read;

fn main() -> std::io:Result<()> {
    let file_path = "input.txt";
    let mut file = File::open(file_path)?;
    let mut content = String::new();    // For now, testing with string buffer
}