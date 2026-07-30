use std::env;
use std::fs::File;
use std::path::PathBuf;

fn icon_rgba(size: u32) -> Vec<u8> {
    let mut pixels = Vec::with_capacity((size * size * 4) as usize);
    let radius = size as f32 * 0.20;
    let center = (size as f32 - 1.0) / 2.0;

    for y in 0..size {
        for x in 0..size {
            let fx = x as f32;
            let fy = y as f32;
            let dx = (fx - center).abs();
            let dy = (fy - center).abs();
            let corner_dx = (dx - (center - radius)).max(0.0);
            let corner_dy = (dy - (center - radius)).max(0.0);
            let inside = corner_dx * corner_dx + corner_dy * corner_dy <= radius * radius;

            if !inside {
                pixels.extend_from_slice(&[0, 0, 0, 0]);
                continue;
            }

            let t = (fx + fy) / (2.0 * size as f32);
            let r = (36.0 + 28.0 * t) as u8;
            let g = (72.0 + 38.0 * t) as u8;
            let b = (176.0 + 48.0 * t) as u8;

            let margin = size as f32 * 0.20;
            let track_left = margin;
            let track_right = size as f32 - margin;
            let track_height = size as f32 * 0.085;
            let rows = [0.32_f32, 0.50, 0.68];
            let fills = [0.76_f32, 0.48, 0.88];
            let mut color = [r, g, b, 255];

            for (row, fill) in rows.iter().zip(fills) {
                let cy = size as f32 * row;
                if fy >= cy - track_height / 2.0
                    && fy <= cy + track_height / 2.0
                    && fx >= track_left
                    && fx <= track_right
                {
                    color = if fx <= track_left + (track_right - track_left) * fill {
                        [239, 245, 255, 255]
                    } else {
                        [113, 142, 210, 255]
                    };
                }
            }

            pixels.extend_from_slice(&color);
        }
    }
    pixels
}

fn main() {
    println!("cargo:rerun-if-changed=build.rs");

    if env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }

    let out_dir = PathBuf::from(env::var_os("OUT_DIR").expect("OUT_DIR is set by Cargo"));
    let icon_path = out_dir.join("codex-usage-monitor.ico");
    let image = ico::IconImage::from_rgba_data(256, 256, icon_rgba(256));
    let entry = ico::IconDirEntry::encode(&image).expect("encode Windows icon");
    let mut icon = ico::IconDir::new(ico::ResourceType::Icon);
    icon.add_entry(entry);
    icon.write(File::create(&icon_path).expect("create Windows icon"))
        .expect("write Windows icon");

    let mut resource = winresource::WindowsResource::new();
    resource
        .set_icon(icon_path.to_str().expect("icon path is UTF-8"))
        .set("ProductName", "Codex Usage Monitor")
        .set("FileDescription", "Multi-account Codex quota monitor")
        .set("OriginalFilename", "codex-usage-monitor.exe");
    resource.compile().expect("compile Windows resources");
}
