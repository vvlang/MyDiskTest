use crate::disk::format_bytes;
use crate::engine::{BlockCell, BlockState};
use egui::{Color32, CornerRadius, Rect, Sense, Stroke, StrokeKind, Ui, Vec2};

pub fn block_color(state: BlockState, dark: bool) -> Color32 {
    match state {
        BlockState::Untested => {
            if dark {
                Color32::from_rgb(48, 54, 65)
            } else {
                Color32::from_rgb(218, 224, 232)
            }
        }
        BlockState::Writing => Color32::from_rgb(56, 189, 248), // Bright sky blue
        BlockState::Written => Color32::from_rgb(59, 130, 246), // Blue (pending verify)
        BlockState::Good => Color32::from_rgb(34, 197, 94),     // Emerald green
        BlockState::Slow => Color32::from_rgb(245, 158, 11),    // Amber warning
        BlockState::Bad => Color32::from_rgb(239, 68, 68),      // Red bad block
        BlockState::Wrapped => Color32::from_rgb(168, 85, 247), // Purple fake wrap-around
    }
}

pub fn state_label_zh(state: BlockState) -> &'static str {
    match state {
        BlockState::Untested => "未检测",
        BlockState::Writing => "正在写入",
        BlockState::Written => "已写入（待回读校验）",
        BlockState::Good => "正常（校验通过）",
        BlockState::Slow => "慢速预警",
        BlockState::Bad => "坏块 / 数据损坏",
        BlockState::Wrapped => "扩容回绕（假容量）",
    }
}

/// Renders the classic MyDiskTest interactive bad-block & fake-capacity grid map.
pub fn render_block_map(ui: &mut Ui, blocks: &[BlockCell]) {
    let dark = ui.visuals().dark_mode;

    // Legend with live counts
    let mut good = 0usize;
    let mut written = 0usize;
    let mut slow = 0usize;
    let mut bad = 0usize;
    let mut wrapped = 0usize;
    for b in blocks {
        match b.state {
            BlockState::Good => good += 1,
            BlockState::Written | BlockState::Writing => written += 1,
            BlockState::Slow => slow += 1,
            BlockState::Bad => bad += 1,
            BlockState::Wrapped => wrapped += 1,
            BlockState::Untested => {}
        }
    }

    let checked = (good + slow + bad + wrapped).max(1);
    let damaged_pct = ((bad + wrapped) as f64 / checked as f64) * 100.0;
    let block_size_label = blocks
        .first()
        .map(|b| format!("每格 {}", format_bytes(b.size_bytes)))
        .unwrap_or_default();

    ui.horizontal_wrapped(|ui| {
        ui.strong("区块健康分布图");
        if !block_size_label.is_empty() {
            ui.small(egui::RichText::new(format!("({})", block_size_label)).weak());
        }
        ui.separator();
        legend_chip(ui, block_color(BlockState::Good, dark), &format!("正常 {}", good));
        if written > 0 {
            legend_chip(ui, block_color(BlockState::Written, dark), &format!("待校 {}", written));
        }
        legend_chip(ui, block_color(BlockState::Slow, dark), &format!("慢速 {}", slow));
        legend_chip(ui, block_color(BlockState::Bad, dark), &format!("坏道/损坏 {}", bad));
        if wrapped > 0 {
            legend_chip(
                ui,
                block_color(BlockState::Wrapped, dark),
                &format!("扩容回绕 {}", wrapped),
            );
        }
        if good + slow + bad + wrapped > 0 {
            ui.separator();
            let dmg_color = if bad + wrapped > 0 {
                Color32::from_rgb(239, 68, 68)
            } else {
                Color32::from_rgb(34, 197, 94)
            };
            ui.colored_label(dmg_color, format!("损坏率 {:.2}%", damaged_pct));
        }
    });

    ui.add_space(6.0);

    if blocks.is_empty() {
        egui::Frame::group(ui.style())
            .inner_margin(16.0)
            .show(ui, |ui| {
                ui.centered_and_justified(|ui| {
                    ui.weak("点击「开始测试」后将在此实时绘制全盘区块健康网格（支持悬停查看每个区块的精确偏移与速度）");
                });
            });
        return;
    }

    let avail_w = ui.available_width().max(240.0);
    let cols = if blocks.len() <= 100 {
        20
    } else if blocks.len() <= 400 {
        25
    } else {
        50 // Classic HD Tune Pro 50-column high-density block grid!
    };
    let rows = (blocks.len() + cols - 1) / cols;
    let gap = if cols >= 50 { 2.0f32 } else { 3.0f32 };
    let max_cell = if cols >= 50 { 16.0f32 } else { 22.0f32 };
    let cell_size = ((avail_w - gap * (cols as f32 - 1.0)) / cols as f32).clamp(6.0, max_cell);
    let total_w = cols as f32 * cell_size + (cols as f32 - 1.0) * gap;
    let total_h = rows as f32 * cell_size + (rows as f32 - 1.0) * gap;

    let (response, painter) = ui.allocate_painter(Vec2::new(total_w, total_h), Sense::hover());
    let origin = response.rect.min;
    let hover_pos = response.hover_pos();
    let mut hovered_cell: Option<&BlockCell> = None;

    for (idx, cell) in blocks.iter().enumerate() {
        let r = idx / cols;
        let c = idx % cols;
        let min_x = origin.x + c as f32 * (cell_size + gap);
        let min_y = origin.y + r as f32 * (cell_size + gap);
        let rect = Rect::from_min_size(egui::pos2(min_x, min_y), Vec2::splat(cell_size));

        let color = block_color(cell.state, dark);
        painter.rect_filled(rect, CornerRadius::same(2), color);

        if let Some(hp) = hover_pos {
            if rect.contains(hp) {
                hovered_cell = Some(cell);
                painter.rect_stroke(
                    rect.expand(1.0),
                    CornerRadius::same(2),
                    Stroke::new(1.5_f32, Color32::WHITE),
                    StrokeKind::Outside,
                );
            }
        }
    }

    if let Some(cell) = hovered_cell {
        response.on_hover_ui_at_pointer(|ui| {
            ui.strong(format!("区块 #{}", cell.index + 1));
            ui.label(format!("状态：{}", state_label_zh(cell.state)));
            ui.label(format!(
                "偏移范围：{} ~ {}",
                format_bytes(cell.offset_bytes),
                format_bytes(cell.offset_bytes + cell.size_bytes)
            ));
            if cell.write_mb_s > 0.0 {
                ui.label(format!("写入速度：{:.2} MB/s", cell.write_mb_s));
            }
            if cell.read_mb_s > 0.0 {
                ui.label(format!("读取速度：{:.2} MB/s", cell.read_mb_s));
            }
            if let Some(ref note) = cell.note {
                ui.colored_label(Color32::from_rgb(248, 113, 113), note);
            }
        });
    }
}

fn legend_chip(ui: &mut Ui, color: Color32, text: &str) {
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 4.0;
        let (rect, _) = ui.allocate_exact_size(Vec2::splat(10.0), Sense::hover());
        ui.painter().rect_filled(rect, CornerRadius::same(2), color);
        ui.small(text);
    });
}
