//! Regenerates assets/redesk.ico and assets/redesk.png from the app's icon drawing.
//!   cargo run --example make_icon
use image::codecs::ico::{IcoEncoder, IcoFrame};

fn main() {
    let frames: Vec<IcoFrame> = [16u32, 24, 32, 48, 64, 128, 256]
        .iter()
        .map(|&s| IcoFrame::as_png(&redesk_agent::ui::icon_rgba(s), s, s, image::ExtendedColorType::Rgba8).unwrap())
        .collect();
    let f = std::fs::File::create("assets/redesk.ico").unwrap();
    IcoEncoder::new(f).encode_images(&frames).unwrap();
    image::RgbaImage::from_raw(256, 256, redesk_agent::ui::icon_rgba(256)).unwrap().save("assets/redesk.png").unwrap();
    println!("wrote assets/redesk.ico, assets/redesk.png");
}
