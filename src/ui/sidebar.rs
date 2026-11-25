use crate::chip::ChipInspection;
use crate::disk::{format_bytes, format_bytes_decimal, DiskInfo, DriveKind};
use egui::{Color32, CornerRadius, RichText, Stroke, Ui};

pub enum SidebarAction {
    None,
    Refresh,
    SelectDisk(usize),
    PickCustomFolder,
}

pub fn render_sidebar(
    ui: &mut Ui,
    disks: &[DiskInfo],
    selected_idx: Option<usize>,
    chip_info: Option<&ChipInspection>,
    is_running: bool,
    allow_system_disk: &mut bool,
) -> SidebarAction {
    let mut action = SidebarAction::None;
    let dark = ui.visuals().dark_mode;

    ui.horizontal(|ui| {
        ui.heading("存储设备");
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if ui
                .add_enabled(!is_running, egui::Button::new("🔄 刷新"))
                .on_hover_text("重新扫描已挂载的 U 盘、SD 卡和硬盘")
                .clicked()
            {
                action = SidebarAction::Refresh;
            }
        });
    });

    ui.add_space(4.0);
    if ui
        .add_enabled(
            !is_running,
            egui::Button::new("📁 选择自定义目录测试").min_size(egui::vec2(ui.available_width(), 28.0)),
        )
        .on_hover_text("可手动指定任意挂载路径、移动硬盘文件夹或测试目录")
        .clicked()
    {
        action = SidebarAction::PickCustomFolder;
    }

    ui.add_space(8.0);

    // Scrollable drive cards
    let list_height = (ui.available_height() - 235.0).max(150.0);
    egui::ScrollArea::vertical()
        .id_salt("disk_list_scroll")
        .max_height(list_height)
        .show(ui, |ui| {
            if disks.is_empty() {
                ui.weak("未检测到可用存储设备，请插入 U 盘或点击「刷新」。");
            }

            for (idx, disk) in disks.iter().enumerate() {
                let selected = selected_idx == Some(idx);
                let bg = if selected {
                    if dark {
                        Color32::from_rgb(30, 58, 95)
                    } else {
                        Color32::from_rgb(219, 234, 254)
                    }
                } else if dark {
                    Color32::from_rgb(30, 34, 42)
                } else {
                    Color32::from_rgb(245, 247, 250)
                };

                let stroke_color = if selected {
                    Color32::from_rgb(59, 130, 246)
                } else if dark {
                    Color32::from_rgb(52, 58, 70)
                } else {
                    Color32::from_rgb(214, 220, 229)
                };

                let frame = egui::Frame::new()
                    .fill(bg)
                    .stroke(Stroke::new(if selected { 1.5_f32 } else { 1.0_f32 }, stroke_color))
                    .corner_radius(CornerRadius::same(8))
                    .inner_margin(10.0);

                let resp = frame
                    .show(ui, |ui| {
                        ui.set_width(ui.available_width());
                        ui.horizontal(|ui| {
                            let badge = kind_badge(&disk.kind, disk.is_system);
                            ui.label(RichText::new(badge.0).color(badge.1).strong().small());
                            ui.with_layout(
                                egui::Layout::right_to_left(egui::Align::Center),
                                |ui| {
                                    ui.label(
                                        RichText::new(&disk.filesystem)
                                            .small()
                                            .color(Color32::from_rgb(148, 163, 184)),
                                    );
                                },
                            );
                        });

                        ui.add_space(2.0);
                        ui.label(RichText::new(&disk.name).strong().size(14.0));
                        ui.small(
                            RichText::new(format!(
                                "{}  •  {}",
                                disk.mount_point.display(),
                                disk.protocol
                            ))
                            .weak(),
                        );

                        ui.add_space(4.0);
                        let ratio = disk.used_ratio();
                        ui.add(
                            egui::ProgressBar::new(ratio)
                                .desired_height(6.0)
                                .fill(if ratio > 0.9 {
                                    Color32::from_rgb(239, 68, 68)
                                } else {
                                    Color32::from_rgb(59, 130, 246)
                                }),
                        );
                        ui.add_space(2.0);
                        ui.horizontal(|ui| {
                            ui.small(format!("可用 {}", format_bytes(disk.free_bytes)));
                            ui.with_layout(
                                egui::Layout::right_to_left(egui::Align::Center),
                                |ui| {
                                    ui.small(format!(
                                        "共 {}",
                                        format_bytes_decimal(
                                            disk.physical_bytes.max(disk.total_bytes)
                                        )
                                    ));
                                },
                            );
                        });
                    })
                    .response
                    .interact(egui::Sense::click());

                if resp.clicked() && !is_running {
                    action = SidebarAction::SelectDisk(idx);
                }
                ui.add_space(6.0);
            }
        });

    ui.separator();

    // Hardware details panel for the selected disk
    ui.strong("设备硬件详情");
    ui.add_space(4.0);
    if let Some(disk) = selected_idx.and_then(|i| disks.get(i)) {
        egui::Grid::new("hw_detail_grid")
            .num_columns(2)
            .spacing([8.0, 4.0])
            .show(ui, |ui| {
                kv_row(ui, "类型", disk.kind.label_zh());
                kv_row(ui, "节点", &disk.device_id);
                kv_row(
                    ui,
                    "标称容量",
                    &format!(
                        "{} ({})",
                        format_bytes_decimal(disk.physical_bytes.max(disk.total_bytes)),
                        format_bytes(disk.total_bytes)
                    ),
                );
                kv_row(ui, "接口协议", &disk.protocol);
                if !disk.vendor.is_empty() {
                    kv_row(ui, "厂商", &disk.vendor);
                }
                if !disk.model.is_empty() {
                    kv_row(ui, "型号", &disk.model);
                }
                if let (Some(ref vid), Some(ref pid)) = (&disk.vid, &disk.pid) {
                    kv_row(ui, "VID / PID", &format!("{} / {}", vid, pid));
                }
                if let Some(insp) = chip_info {
                    if let Some(ref m) = insp.controller_match {
                        let ctrl = insp
                            .firmware_controller_hint
                            .as_deref()
                            .filter(|s| !s.is_empty())
                            .unwrap_or(&m.primary.controller);
                        let summary = if ctrl.is_empty() {
                            m.primary.chip_vendor.clone()
                        } else {
                            format!("{} {}", m.primary.chip_vendor, ctrl)
                        };
                        ui.label(RichText::new("主控芯片").small().weak());
                        ui.label(
                            RichText::new(summary.trim())
                                .small()
                                .strong()
                                .color(Color32::from_rgb(56, 189, 248)),
                        );
                        ui.end_row();
                    }
                    if let Some(ref fr) = insp.flash_record {
                        ui.label(RichText::new("闪存颗粒").small().weak());
                        ui.label(
                            RichText::new(format!("{} ({})", fr.manufacturer, fr.flash_type))
                                .small()
                                .color(Color32::from_rgb(52, 211, 153)),
                        );
                        ui.end_row();
                    }
                }
                if let Some(ref sn) = disk.serial {
                    kv_row(ui, "序列号", sn);
                }
            });

        if disk.is_system {
            ui.add_space(6.0);
            ui.colored_label(
                Color32::from_rgb(245, 158, 11),
                "🔒 当前选中系统启动盘，默认锁定防止误写入。",
            );
            ui.add_enabled(
                !is_running,
                egui::Checkbox::new(allow_system_disk, "我了解风险，解锁系统盘测试"),
            );
        }
        if disk.is_read_only {
            ui.add_space(4.0);
            ui.colored_label(
                Color32::from_rgb(239, 68, 68),
                "⚠ 该卷为只读状态，无法执行写入测试。",
            );
        }
    } else {
        ui.weak("请先在上方选择一个目标磁盘。");
    }

    action
}

fn kind_badge(kind: &DriveKind, is_system: bool) -> (&'static str, Color32) {
    if is_system {
        return ("🔒 系统盘", Color32::from_rgb(245, 158, 11));
    }
    match kind {
        DriveKind::RemovableUsb => ("● U 盘", Color32::from_rgb(34, 197, 94)),
        DriveKind::SdCard => ("● SD 卡", Color32::from_rgb(16, 185, 129)),
        DriveKind::ExternalDrive => ("● 外置存储", Color32::from_rgb(56, 189, 248)),
        DriveKind::InternalSsd => ("● 内置 SSD", Color32::from_rgb(148, 163, 184)),
        DriveKind::InternalHdd => ("● 内置 HDD", Color32::from_rgb(148, 163, 184)),
        DriveKind::NetworkOrVirtual => ("● 虚拟/网络", Color32::from_rgb(168, 85, 247)),
        DriveKind::Unknown => ("● 未知", Color32::from_rgb(148, 163, 184)),
    }
}

fn kv_row(ui: &mut Ui, key: &str, value: &str) {
    ui.label(RichText::new(key).small().weak());
    ui.label(RichText::new(value).small());
    ui.end_row();
}
