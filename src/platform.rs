use std::path::Path;
use std::sync::Arc;

use eframe::egui::{self, FontData, FontDefinitions, FontFamily, IconData};

pub fn install_panic_hook() {
    std::panic::set_hook(Box::new(|panic| {
        let message = if let Some(message) = panic.payload().downcast_ref::<&str>() {
            *message
        } else if let Some(message) = panic.payload().downcast_ref::<String>() {
            message.as_str()
        } else {
            "未知 panic"
        };
        crate::logging::warn(format!("应用捕获到不可恢复错误：{message}"));
    }));
}

pub fn install_fonts(ctx: &egui::Context) {
    let candidates: &[&str] = if cfg!(target_os = "macos") {
        &[
            "/System/Library/Fonts/PingFang.ttc",
            "/System/Library/Fonts/STHeiti Light.ttc",
        ]
    } else if cfg!(target_os = "windows") {
        &[
            r"C:\Windows\Fonts\msyh.ttc",
            r"C:\Windows\Fonts\msyhbd.ttc",
            r"C:\Windows\Fonts\simhei.ttf",
        ]
    } else {
        &[
            "/usr/share/fonts/opentype/noto/NotoSansCJK-Regular.ttc",
            "/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf",
        ]
    };

    let Some((path, bytes)) = candidates
        .iter()
        .find_map(|path| std::fs::read(path).ok().map(|bytes| (*path, bytes)))
    else {
        crate::logging::warn("未找到系统中文字体，将使用 egui 默认字体");
        return;
    };

    let mut fonts = FontDefinitions::default();
    fonts.font_data.insert(
        "system_cjk".to_owned(),
        Arc::new(FontData::from_owned(bytes)),
    );
    if let Some(family) = fonts.families.get_mut(&FontFamily::Proportional) {
        family.insert(0, "system_cjk".to_owned());
    }
    if let Some(family) = fonts.families.get_mut(&FontFamily::Monospace) {
        family.push("system_cjk".to_owned());
    }
    ctx.set_fonts(fonts);
    crate::logging::info(format!("已加载系统字体 {}", Path::new(path).display()));
}

pub fn app_icon() -> Arc<IconData> {
    let size = 64_u32;
    let mut rgba = Vec::with_capacity((size * size * 4) as usize);
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
                rgba.extend_from_slice(&[0, 0, 0, 0]);
                continue;
            }

            let t = (fx + fy) / (2.0 * size as f32);
            let mut color = [
                (36.0 + 28.0 * t) as u8,
                (72.0 + 38.0 * t) as u8,
                (176.0 + 48.0 * t) as u8,
                255,
            ];
            let margin = size as f32 * 0.20;
            let right = size as f32 - margin;
            for (row, fill) in [0.32_f32, 0.50, 0.68]
                .into_iter()
                .zip([0.76_f32, 0.48, 0.88])
            {
                let cy = size as f32 * row;
                if (fy - cy).abs() <= size as f32 * 0.043 && fx >= margin && fx <= right {
                    color = if fx <= margin + (right - margin) * fill {
                        [239, 245, 255, 255]
                    } else {
                        [113, 142, 210, 255]
                    };
                }
            }
            rgba.extend_from_slice(&color);
        }
    }

    Arc::new(IconData {
        rgba,
        width: size,
        height: size,
    })
}
