//! Regenerates assets/redesk.ico and assets/redesk.png from the app's icon drawing.
//!   cargo run --example make_icon
//!   cargo run --example make_icon -- --iconset DIR   # macOS .iconset for iconutil (build-app.sh)
use image::codecs::ico::{IcoEncoder, IcoFrame};

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if let [flag, dir] = args.as_slice()
        && flag == "--iconset"
    {
        iconset(std::path::Path::new(dir));
        return;
    }
    let frames: Vec<IcoFrame> = [16u32, 24, 32, 48, 64, 128, 256]
        .iter()
        .map(|&s| IcoFrame::as_png(&redesk_agent::ui::icon_rgba(s), s, s, image::ExtendedColorType::Rgba8).unwrap())
        .collect();
    let f = std::fs::File::create("assets/redesk.ico").unwrap();
    IcoEncoder::new(f).encode_images(&frames).unwrap();
    image::RgbaImage::from_raw(256, 256, redesk_agent::ui::icon_rgba(256)).unwrap().save("assets/redesk.png").unwrap();
    println!("wrote assets/redesk.ico, assets/redesk.png");
}

/// macOS app icon sizes. The artwork sits on the standard grid (824 of 1024 px, centred) so it
/// matches the size of other Dock and Finder icons.
fn iconset(dir: &std::path::Path) {
    std::fs::create_dir_all(dir).unwrap();
    for (pt, scale) in [16u32, 32, 128, 256, 512].iter().flat_map(|&p| [(p, 1u32), (p, 2)]) {
        let px = pt * scale;
        let body = (px as f32 * 824.0 / 1024.0).round() as u32;
        let art = image::RgbaImage::from_raw(body, body, redesk_agent::ui::icon_rgba(body)).unwrap();
        let mut canvas = image::RgbaImage::new(px, px);
        let off = ((px - body) / 2) as i64;
        image::imageops::overlay(&mut canvas, &art, off, off);
        let name = if scale == 1 { format!("icon_{pt}x{pt}.png") } else { format!("icon_{pt}x{pt}@2x.png") };
        canvas.save(dir.join(name)).unwrap();
    }
    println!("wrote {}", dir.display());
}
