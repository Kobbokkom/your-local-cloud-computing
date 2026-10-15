use std::{fs, env};
fn main() {
    let sum: u64 = (0..100_000).map(|i| i * i).sum();
    assert_eq!(sum, 333_328_333_350_000);
    fs::write("result.json", format!("{{\"os\":\"{}\",\"arch\":\"{}\",\"sum\":{sum}}}\n", env::consts::OS, env::consts::ARCH)).unwrap();
    println!("computed {sum}");
}
