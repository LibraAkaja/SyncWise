use syncwise_engine::normalize_name;

fn main() {
    let input = std::env::args()
        .nth(1)
        .unwrap_or_else(|| String::from(" Example Song.MP3 "));

    println!("Original:   {input}");
    println!("Normalized: {}", normalize_name(&input));
}