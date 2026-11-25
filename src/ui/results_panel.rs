use crate::disk::{format_bytes, format_bytes_decimal, format_duration};
use crate::engine::{AgingCycleRecord, BadSectorEntry, BenchmarkMetrics, LiveTestSnapshot, Verdict};
use egui::{Color32, CornerRadius, RichText, Stroke, Ui};

/// Renders the color-coded verdict banner when a test is done or failed.
pub fn render_verdict_banner(ui: &mut Ui, verdict: &Verdict) {
    let (bg, border, title, body) = match verdict {
        Verdict::Running => return,
        Verdict::Healthy { summary, .. } => (
            Color32::from_rgba_unmultiplied(34, 197, 94, 30),
            Color32::from_rgb(34, 197, 94),
            "✅ 检测通过 — 正品足容 / 存储健康".to_string(),
            summary.clone(),
        ),
        Verdict::FakeCapacity {
            reported_bytes,
            estimated_real_bytes,
            reason,
        } => (
            Color32::from_rgba_unmultiplied(239, 68, 68, 40),
            Color32::from_rgb(239, 68, 68),
            format!(
                "🚨 严重警告：检测到扩容盘 / 假容量！（标称 {} → 真实约 {}）",
                format_bytes_decimal(*reported_bytes),
                format_bytes(*estimated_real_bytes)
            ),
            reason.clone(),
        ),
        Verdict::BadBlocks { summary, .. } => (
            Color32::from_rgba_unmultiplied(245, 158, 11, 35),
            Color32::from_rgb(245, 158, 11),
            "⚠️ 警告：检测到坏块或数据校验异常".to_string(),
            summary.clone(),
        ),
        Verdict::BenchmarkComplete { summary } => (
            Color32::from_rgba_unmultiplied(59, 130, 246, 30),
            Color32::from_rgb(59, 130, 246),
            "📊 读写速度基准测试完成".to_string(),
            summary.clone(),
        ),
        Verdict::AgingComplete {
            total_errors,
            summary,
            ..
        } => {
            if *total_errors == 0 {
                (
                    Color32::from_rgba_unmultiplied(34, 197, 94, 30),
                    Color32::from_rgb(34, 197, 94),
                    "🔥 老化拷机测试通过（零错误）".to_string(),
                    summary.clone(),
                )
            } else {
                (
                    Color32::from_rgba_unmultiplied(239, 68, 68, 35),
                    Color32::from_rgb(239, 68, 68),
                    "🔥 老化拷机检测到不稳定 / 数据错误".to_string(),
                    summary.clone(),
                )
            }
        }
        Verdict::Cancelled { summary } => (
            Color32::from_rgba_unmultiplied(148, 163, 184, 25),
            Color32::from_rgb(148, 163, 184),
            "⏹ 测试已停止".to_string(),
            summary.clone(),
        ),
        Verdict::Failed { error } => (
            Color32::from_rgba_unmultiplied(239, 68, 68, 35),
            Color32::from_rgb(239, 68, 68),
            "❌ 测试未能完成".to_string(),
            error.clone(),
        ),
    };

    egui::Frame::new()
        .fill(bg)
        .stroke(Stroke::new(1.5_f32, border))
        .corner_radius(CornerRadius::same(8))
        .inner_margin(12.0)
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.label(RichText::new(title).strong().size(15.0).color(border));
            ui.add_space(4.0);
            ui.label(&body);

            #[cfg(target_os = "macos")]
            if body.contains("完全磁盘访问权限") || body.contains("Operation not permitted") {
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    let btn = egui::Button::new(
                        RichText::new("🔓 打开「完全磁盘访问权限」系统设置")
                            .strong()
                            .color(Color32::WHITE),
                    )
                    .fill(Color32::from_rgb(37, 99, 235))
                    .corner_radius(CornerRadius::same(6));
                    if ui.add(btn).clicked() {
                        let _ = std::process::Command::new("open")
                            .arg("x-apple.systempreferences:com.apple.preference.security?Privacy_AllFiles")
                            .spawn();
                    }
                    ui.small("提示：添加 /Applications/MyDiskTest.app 并打开开关后，退出重开即可生效。");
                });
            }
        });
    ui.add_space(8.0);
}

/// Renders top KPI metrics cards during & after test execution.
pub fn render_kpi_cards(ui: &mut Ui, snap: &LiveTestSnapshot) {
    let dark = ui.visuals().dark_mode;
    let card_bg = if dark {
        Color32::from_rgb(28, 33, 42)
    } else {
        Color32::from_rgb(246, 248, 251)
    };
    let card_stroke = if dark {
        Color32::from_rgb(48, 55, 68)
    } else {
        Color32::from_rgb(218, 225, 235)
    };

    ui.columns(5, |cols| {
        kpi_tile(
            &mut cols[0],
            card_bg,
            card_stroke,
            "实时速度",
            &format!("{:.1} MB/s", snap.current_speed_mb_s),
            Color32::from_rgb(56, 189, 248),
        );
        kpi_tile(
            &mut cols[1],
            card_bg,
            card_stroke,
            "平均写入",
            &format!("{:.1} MB/s", snap.avg_write_mb_s),
            Color32::from_rgb(96, 165, 250),
        );
        kpi_tile(
            &mut cols[2],
            card_bg,
            card_stroke,
            "平均读取",
            &format!("{:.1} MB/s", snap.avg_read_mb_s),
            Color32::from_rgb(52, 211, 153),
        );
        kpi_tile(
            &mut cols[3],
            card_bg,
            card_stroke,
            "已写 / 已校验",
            &format!(
                "{} / {}",
                format_bytes(snap.bytes_written),
                format_bytes(snap.bytes_verified)
            ),
            if dark { Color32::LIGHT_GRAY } else { Color32::DARK_GRAY },
        );
        let eta_str = snap
            .eta_secs
            .map(|e| format!("余 {}", format_duration(e)))
            .unwrap_or_else(|| {
                if snap.running {
                    "计算中".to_string()
                } else {
                    "已完成".to_string()
                }
            });
        kpi_tile(
            &mut cols[4],
            card_bg,
            card_stroke,
            "耗时 / 预计",
            &format!("{} ({})", format_duration(snap.elapsed_secs), eta_str),
            if dark { Color32::LIGHT_GRAY } else { Color32::DARK_GRAY },
        );
    });
}

fn kpi_tile(
    ui: &mut Ui,
    bg: Color32,
    border: Color32,
    label: &str,
    value: &str,
    accent: Color32,
) {
    egui::Frame::new()
        .fill(bg)
        .stroke(Stroke::new(1.0_f32, border))
        .corner_radius(CornerRadius::same(8))
        .inner_margin(10.0)
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.small(RichText::new(label).weak());
            ui.add_space(2.0);
            ui.label(RichText::new(value).strong().size(15.0).color(accent));
        });
}

/// Renders structured benchmark metrics table (Seq 1MB, 64KB, 4K Random).
pub fn render_benchmark_table(ui: &mut Ui, m: &BenchmarkMetrics) {
    ui.strong("读写基准测试结果汇总");
    ui.add_space(6.0);

    egui::Frame::group(ui.style())
        .inner_margin(12.0)
        .show(ui, |ui| {
            egui::Grid::new("benchmark_results_grid")
                .striped(true)
                .num_columns(4)
                .spacing([24.0, 8.0])
                .show(ui, |ui| {
                    ui.strong("测试项目");
                    ui.strong("写入速度");
                    ui.strong("读取速度");
                    ui.strong("延迟 / IOPS / 波动详情");
                    ui.end_row();

                    ui.label("顺序 1MB (大文件)");
                    ui.colored_label(
                        Color32::from_rgb(56, 189, 248),
                        format!("{:.2} MB/s", m.seq_write_mb_s),
                    );
                    ui.colored_label(
                        Color32::from_rgb(52, 211, 153),
                        format!("{:.2} MB/s", m.seq_read_mb_s),
                    );
                    ui.small(format!(
                        "写 {:.1}~{:.1} MB/s | 读 {:.1}~{:.1} MB/s",
                        m.seq_write_min_mb_s,
                        m.seq_write_max_mb_s,
                        m.seq_read_min_mb_s,
                        m.seq_read_max_mb_s
                    ));
                    ui.end_row();

                    ui.label("64KB 中小块");
                    ui.colored_label(
                        Color32::from_rgb(56, 189, 248),
                        format!("{:.2} MB/s", m.block64k_write_mb_s),
                    );
                    ui.colored_label(
                        Color32::from_rgb(52, 211, 153),
                        format!("{:.2} MB/s", m.block64k_read_mb_s),
                    );
                    ui.small("日常照片/文档传输参考");
                    ui.end_row();

                    ui.label("4K 随机 (QD1)");
                    ui.colored_label(
                        Color32::from_rgb(56, 189, 248),
                        format!("{:.2} MB/s", m.rand4k_write_mb_s),
                    );
                    ui.colored_label(
                        Color32::from_rgb(52, 211, 153),
                        format!("{:.2} MB/s", m.rand4k_read_mb_s),
                    );
                    ui.small(format!(
                        "写 {:.0} IOPS ({:.2} ms) | 读 {:.0} IOPS ({:.2} ms)",
                        m.rand4k_write_iops,
                        m.rand4k_write_lat_ms,
                        m.rand4k_read_iops,
                        m.rand4k_read_lat_ms
                    ));
                    ui.end_row();
                });
        });
}

/// Renders aging / burn-in per-cycle records table.
pub fn render_aging_table(ui: &mut Ui, records: &[AgingCycleRecord]) {
    ui.strong("老化拷机各轮次记录");
    ui.add_space(6.0);

    if records.is_empty() {
        ui.weak("等待第一轮拷机完成后显示各轮次数据与掉速趋势...");
        return;
    }

    egui::Frame::group(ui.style())
        .inner_margin(10.0)
        .show(ui, |ui| {
            egui::ScrollArea::vertical()
                .id_salt("aging_table_scroll")
                .max_height(150.0)
                .show(ui, |ui| {
                    egui::Grid::new("aging_records_grid")
                        .striped(true)
                        .num_columns(5)
                        .spacing([24.0, 6.0])
                        .show(ui, |ui| {
                            ui.strong("轮次");
                            ui.strong("写入速度");
                            ui.strong("读取速度");
                            ui.strong("校验错误");
                            ui.strong("耗时");
                            ui.end_row();

                            for r in records {
                                ui.label(format!("第 {} 轮", r.cycle));
                                ui.label(format!("{:.2} MB/s", r.write_mb_s));
                                ui.label(format!("{:.2} MB/s", r.read_mb_s));
                                if r.errors == 0 {
                                    ui.colored_label(Color32::from_rgb(34, 197, 94), "0");
                                } else {
                                    ui.colored_label(
                                        Color32::from_rgb(239, 68, 68),
                                        format!("{} 次", r.errors),
                                    );
                                }
                                ui.label(format!("{:.1} s", r.duration_secs));
                                ui.end_row();
                            }
                        });
                });
        });
}

/// Renders the HD Tune Pro Error Scan bad/slow sector coordinate list (LBA & byte offset).
pub fn render_bad_sectors_table(ui: &mut Ui, entries: &[BadSectorEntry]) {
    let hard_cnt = entries.iter().filter(|e| e.is_hard_error).count();
    let slow_cnt = entries.len().saturating_sub(hard_cnt);

    ui.horizontal(|ui| {
        ui.strong("坏道 / 慢速扇区定位清单");
        if entries.is_empty() {
            ui.colored_label(Color32::from_rgb(34, 197, 94), "（当前未发现任何坏道或慢速扇区）");
        } else {
            ui.colored_label(
                if hard_cnt > 0 {
                    Color32::from_rgb(239, 68, 68)
                } else {
                    Color32::from_rgb(245, 158, 11)
                },
                format!("（硬坏道 {} 处 / 慢速预警 {} 处）", hard_cnt, slow_cnt),
            );
        }
    });
    ui.add_space(4.0);

    if entries.is_empty() {
        return;
    }

    egui::Frame::group(ui.style())
        .inner_margin(10.0)
        .show(ui, |ui| {
            egui::ScrollArea::vertical()
                .id_salt("bad_sectors_scroll")
                .max_height(150.0)
                .show(ui, |ui| {
                    egui::Grid::new("bad_sectors_grid")
                        .striped(true)
                        .num_columns(5)
                        .spacing([18.0, 6.0])
                        .show(ui, |ui| {
                            ui.strong("区块");
                            ui.strong("偏移位置");
                            ui.strong("LBA (512B)");
                            ui.strong("速度");
                            ui.strong("诊断说明");
                            ui.end_row();

                            for e in entries.iter().take(300) {
                                let color = if e.is_hard_error {
                                    Color32::from_rgb(248, 113, 113)
                                } else {
                                    Color32::from_rgb(251, 191, 36)
                                };
                                ui.colored_label(color, format!("#{}", e.block_index + 1));
                                ui.label(format_bytes(e.offset_bytes));
                                ui.label(RichText::new(format!("{}", e.lba_512)).monospace());
                                ui.label(format!("{:.2} MB/s", e.speed_mb_s));
                                ui.colored_label(color, &e.reason);
                                ui.end_row();
                            }
                        });
                });
        });
}
