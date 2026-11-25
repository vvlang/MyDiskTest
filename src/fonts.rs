use egui::{FontData, FontDefinitions, FontFamily};
use std::sync::Arc;

/// Loads a system CJK font so Chinese UI labels render crisply on macOS, Windows, and Linux
/// without bundling a multi-megabyte font binary into the executable.
pub fn configure_cjk_fonts(ctx: &egui::Context) {
    let mut fonts = FontDefinitions::default();

    let candidates: &[&str] = if cfg!(target_os = "macos") {
        &[
            "/System/Library/Fonts/Hiragino Sans GB.ttc",
            "/System/Library/Fonts/PingFang.ttc",
            "/System/Library/Fonts/STHeiti Medium.ttc",
            "/System/Library/Fonts/STHeiti Light.ttc",
            "/System/Library/Fonts/Supplemental/Songti.ttc",
            "/System/Library/Fonts/Supplemental/Arial Unicode.ttf",
        ]
    } else if cfg!(target_os = "windows") {
        &[
            r"C:\Windows\Fonts\msyh.ttc",
            r"C:\Windows\Fonts\msyhl.ttc",
            r"C:\Windows\Fonts\Deng.ttf",
            r"C:\Windows\Fonts\simhei.ttf",
            r"C:\Windows\Fonts\simsun.ttc",
        ]
    } else {
        &[
            "/usr/share/fonts/opentype/noto/NotoSansCJK-Regular.ttc",
            "/usr/share/fonts/noto-cjk/NotoSansCJK-Regular.ttc",
            "/usr/share/fonts/truetype/wqy/wqy-microhei.ttc",
            "/usr/share/fonts/truetype/droid/DroidSansFallbackFull.ttf",
        ]
    };

    for path in candidates {
        if let Ok(bytes) = std::fs::read(path) {
            if !bytes.is_empty() {
                let font_name = "system_cjk".to_owned();
                fonts.font_data.insert(
                    font_name.clone(),
                    Arc::new(FontData::from_owned(bytes)),
                );
                // Put system CJK first for proportional so Chinese & Latin align nicely,
                // and as fallback for monospace so log/report lines with CJK render cleanly.
                if let Some(family) = fonts.families.get_mut(&FontFamily::Proportional) {
                    family.insert(0, font_name.clone());
                }
                if let Some(family) = fonts.families.get_mut(&FontFamily::Monospace) {
                    family.push(font_name);
                }
                break;
            }
        }
    }

    ctx.set_fonts(fonts);
}
