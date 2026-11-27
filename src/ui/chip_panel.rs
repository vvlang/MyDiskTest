use crate::chip::{ChipDatabase, ChipInspection};
use crate::disk::DiskInfo;
use egui::{Color32, CornerRadius, RichText, Stroke, Ui};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChipSearchTab {
    Flash,
    Controller,
}

pub struct ChipPanelState {
    pub search_tab: ChipSearchTab,
    pub query: String,
}

impl Default for ChipPanelState {
    fn default() -> Self {
        Self {
            search_tab: ChipSearchTab::Flash,
            query: String::new(),
        }
    }
}

pub enum ChipPanelAction {
    None,
    Reprobe,
    ImportWdb,
    CopiedReport,
    #[cfg_attr(not(target_os = "windows"), allow(dead_code))]
    InnostorDeepProbe(String),
}

pub fn render_chip_panel(
    ui: &mut Ui,
    disk: Option<&DiskInfo>,
    inspection: Option<&ChipInspection>,
    db: &ChipDatabase,
    panel_state: &mut ChipPanelState,
) -> ChipPanelAction {
    let mut action = ChipPanelAction::None;
    let dark = ui.visuals().dark_mode;
    let card_bg = if dark {
        Color32::from_rgb(24, 29, 38)
    } else {
        Color32::from_rgb(246, 248, 252)
    };
    let card_border = if dark {
        Color32::from_rgb(48, 56, 70)
    } else {
        Color32::from_rgb(214, 222, 232)
    };

    // ---- Card 1: Current Selected Drive ChipGenius Report ----
    egui::Frame::new()
        .fill(card_bg)
        .stroke(Stroke::new(1.0_f32, card_border))
        .corner_radius(CornerRadius::same(8))
        .inner_margin(14.0)
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal_wrapped(|ui| {
                ui.strong(RichText::new("🔬 U 盘主控与闪存颗粒检测（Chips.wdb 引擎）").size(15.0));
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui
                        .button("📂 导入新版 Chips.wdb")
                        .on_hover_text("可加载外部更新的 ChipGenius Chips.wdb 数据库文件")
                        .clicked()
                    {
                        action = ChipPanelAction::ImportWdb;
                    }
                    if let (Some(d), Some(insp)) = (disk, inspection) {
                        if ui.button("📋 复制芯片报告").clicked() {
                            ui.ctx().copy_text(insp.format_chipgenius_report(d));
                            action = ChipPanelAction::CopiedReport;
                        }
                    }
                    if ui
                        .add_enabled(disk.is_some(), egui::Button::new("🔄 重新探测"))
                        .clicked()
                    {
                        action = ChipPanelAction::Reprobe;
                    }
                });
            });

            ui.small(
                RichText::new(format!(
                    "当前数据库：{}（日期 {} | 主控 {} 条 | 闪存颗粒 {} 条）",
                    db.source_label,
                    db.db_date,
                    db.controllers.len(),
                    db.flashes.len()
                ))
                .weak(),
            );
            ui.separator();

            if let (Some(d), Some(insp)) = (disk, inspection) {
                // Warning banner if fake/black-market controller or SpecTek downgraded flash!
                if let Some(ref m) = insp.controller_match {
                    if m.primary.note.contains("黑盘")
                        || m.primary.note.contains("山寨")
                        || m.primary.note.contains("不规范")
                    {
                        warning_banner(
                            ui,
                            Color32::from_rgb(239, 68, 68),
                            &format!("🚨 主控风险提示：{}", m.primary.note),
                        );
                    }
                }
                if let Some(ref fr) = insp.flash_record {
                    if fr.is_downgraded {
                        warning_banner(
                            ui,
                            Color32::from_rgb(245, 158, 11),
                            "⚠️ 闪存颗粒提示：检测到 SpecTek (美光大S) 降级/白片颗粒，重要数据请做好备份！",
                        );
                    }
                }

                ui.columns(2, |cols| {
                    // Left column: USB & Controller Info
                    cols[0].strong("一、USB 硬件与主控芯片");
                    cols[0].add_space(4.0);
                    egui::Grid::new("chip_ctrl_grid")
                        .num_columns(2)
                        .spacing([12.0, 6.0])
                        .show(&mut cols[0], |ui| {
                            row_kv(ui, "目标设备", &d.name);
                            row_kv(ui, "协议版本", &d.protocol);
                            if let Some(ref spd) = d.link_speed {
                                row_kv(ui, "当前速度", spd);
                            }
                            if let Some(ma) = d.max_power_ma {
                                row_kv_mono(ui, "电力消耗", &format!("{} mA", ma));
                            }
                            let vid_pid = match (&insp.vid, &insp.pid) {
                                (Some(v), Some(p)) => format!("VID = {}   PID = {}", v, p),
                                _ => "非 USB 设备 / 未读取到 VID & PID".to_string(),
                            };
                            row_kv_mono(ui, "VID / PID", &vid_pid);
                            if let Some(ref bcd) = insp.bcd_device {
                                row_kv_mono(ui, "设备修订版 (bcdDevice)", bcd);
                            }
                            if let Some(ref um) = d.usb_manufacturer {
                                row_kv(ui, "设备供应商", um);
                            }
                            if let Some(ref up) = d.usb_product {
                                row_kv(ui, "设备名称", up);
                            }
                            if !d.vendor.is_empty() || !d.model.is_empty() {
                                row_kv(
                                    ui,
                                    "厂商 / 型号",
                                    &format!("{} {}", d.vendor, d.model).trim().to_string(),
                                );
                            }
                            if let Some(ref sn) = d.serial {
                                row_kv_mono(ui, "产品序列号", sn);
                            }
                            if let (Some(ref sv), Some(ref sp)) =
                                (&insp.scsi_vendor, &insp.scsi_product)
                            {
                                row_kv_mono(
                                    ui,
                                    "SCSI 查询",
                                    &format!(
                                        "{} / {} (Rev {})",
                                        sv,
                                        sp,
                                        insp.scsi_rev.as_deref().unwrap_or("-")
                                    ),
                                );
                            }

                            // Highlighted Controller Vendor & Model (prefer direct firmware readout over DB guess!)
                            let has_direct_fw = insp.firmware_controller_hint.is_some();
                            if let Some(ref fw_v) = insp.firmware_vendor_hint {
                                row_kv_accent(
                                    ui,
                                    "主控厂商",
                                    &format!("{}（固件直读）", fw_v),
                                    Color32::from_rgb(56, 189, 248),
                                );
                            } else if let Some(ref m) = insp.controller_match {
                                let vendor_txt = if m.primary.chip_vendor.is_empty() {
                                    "未知厂商"
                                } else {
                                    &m.primary.chip_vendor
                                };
                                row_kv_accent(
                                    ui,
                                    "主控厂商",
                                    vendor_txt,
                                    Color32::from_rgb(56, 189, 248),
                                );
                            }

                            if let Some(ref fw_c) = insp.firmware_controller_hint {
                                row_kv_accent(
                                    ui,
                                    "主控型号",
                                    &format!("{}（固件直读）", fw_c),
                                    Color32::from_rgb(52, 211, 153),
                                );
                            } else if let Some(ref m) = insp.controller_match {
                                let ctrl_txt = if m.primary.controller.is_empty() {
                                    "未知型号（仅匹配厂商 VID）".to_string()
                                } else if m.is_wildcard {
                                    format!("{}（特征/VID匹配）", m.primary.controller)
                                } else {
                                    m.primary.controller.clone()
                                };
                                row_kv_accent(
                                    ui,
                                    "主控型号",
                                    &ctrl_txt,
                                    Color32::from_rgb(52, 211, 153),
                                );
                            } else {
                                row_kv_accent(
                                    ui,
                                    "主控识别",
                                    "未命中 Chips.wdb（非 U 盘或未收录的新款主控）",
                                    Color32::from_rgb(148, 163, 184),
                                );
                            }

                            if let Some(ref fw_ver) = insp.firmware_version_hint {
                                row_kv_mono(ui, "主控固件版本", fw_ver);
                            }
                            if let Some(ref ata) = insp.bridge_ata_model {
                                row_kv_accent(
                                    ui,
                                    "桥接内置 SSD",
                                    ata,
                                    Color32::from_rgb(52, 211, 153),
                                );
                            }

                            if let Some(ref m) = insp.controller_match {
                                if !has_direct_fw && m.candidates.len() > 1 {
                                    let others: Vec<String> = m
                                        .candidates
                                        .iter()
                                        .filter(|c| !c.controller.is_empty())
                                        .map(|c| {
                                            if c.rev_filter.is_empty() {
                                                c.controller.clone()
                                            } else {
                                                format!("{} [{}]", c.controller, c.rev_filter)
                                            }
                                        })
                                        .take(6)
                                        .collect();
                                    if !others.is_empty() {
                                        row_kv(ui, "同 ID 候选主控", &others.join(" / "));
                                    }
                                }
                                if !m.primary.note.is_empty() {
                                    row_kv(ui, "数据库备注", &m.primary.note);
                                }
                                if !m.primary.url.is_empty() {
                                    ui.label(RichText::new("参考/量产工具").small().weak());
                                    ui.hyperlink(&m.primary.url);
                                    ui.end_row();
                                }
                            }
                        });

                    // Right column: NAND Flash Info
                    cols[1].strong("二、NAND 闪存颗粒识别");
                    cols[1].add_space(4.0);
                    egui::Grid::new("chip_flash_grid")
                        .num_columns(2)
                        .spacing([12.0, 6.0])
                        .show(&mut cols[1], |ui| {
                            if let Some(ref fid) = insp.flash_id_hex {
                                let ce_str = insp
                                    .flash_ce_count
                                    .map(|n| format!("  [{}CE]", n))
                                    .unwrap_or_default();
                                row_kv_accent(
                                    ui,
                                    "闪存识别码 (ID)",
                                    &format!("{}{}", fid, ce_str),
                                    Color32::from_rgb(56, 189, 248),
                                );
                            } else {
                                row_kv(ui, "闪存识别码 (ID)", "未直读到裸 Flash ID");
                            }

                            if let Some(ref fr) = insp.flash_record {
                                row_kv_accent(
                                    ui,
                                    "闪存厂商",
                                    &fr.manufacturer,
                                    Color32::from_rgb(52, 211, 153),
                                );
                                let type_color = if fr.flash_type.starts_with("SLC") {
                                    Color32::from_rgb(34, 197, 94)
                                } else if fr.flash_type.starts_with("MLC") {
                                    Color32::from_rgb(56, 189, 248)
                                } else if fr.flash_type.starts_with("TLC") {
                                    Color32::from_rgb(245, 158, 11)
                                } else {
                                    Color32::from_rgb(239, 68, 68)
                                };
                                row_kv_accent(ui, "颗粒类型 / 页大小", &fr.flash_type, type_color);
                                row_kv_mono(
                                    ui,
                                    "闪存颗粒料号",
                                    &fr.part_numbers.join("\n"),
                                );
                            } else if let Some(ref vendor) = insp.flash_vendor_fallback {
                                row_kv_accent(
                                    ui,
                                    "闪存厂商",
                                    vendor,
                                    Color32::from_rgb(52, 211, 153),
                                );
                                row_kv(
                                    ui,
                                    "数据库匹配",
                                    "已识别厂商字节，但 Chips.wdb 暂未收录该精确 4 字节 ID",
                                );
                            }
                        });

                    cols[1].add_space(6.0);
                    cols[1].label(
                        RichText::new(&insp.probe_status)
                            .small()
                            .color(Color32::from_rgb(148, 163, 184)),
                    );

                    if let Some((ref ch0, ref ch1)) = insp.flash_channel_map {
                        cols[1].add_space(6.0);
                        cols[1].strong("闪存识别码映射表");
                        egui::Grid::new("flash_ch_map_grid")
                            .striped(true)
                            .num_columns(3)
                            .spacing([14.0, 3.0])
                            .show(&mut cols[1], |ui| {
                                ui.small(RichText::new("CE").weak());
                                ui.small(RichText::new("[通道 0]").strong());
                                ui.small(RichText::new("[通道 1]").strong());
                                ui.end_row();
                                let rows = ch0.len().max(ch1.len());
                                for i in 0..rows {
                                    ui.label(RichText::new(format!("#{}", i)).monospace().small().weak());
                                    let c0 = ch0.get(i).and_then(|x| x.as_deref()).unwrap_or("------------");
                                    let c1 = ch1.get(i).and_then(|x| x.as_deref()).unwrap_or("------------");
                                    ui.label(RichText::new(c0).monospace().small());
                                    ui.label(RichText::new(c1).monospace().small());
                                    ui.end_row();
                                }
                            });
                    }

                    #[cfg(target_os = "windows")]
                    if insp.flash_id_hex.is_none() {
                        if let Some(ref model) = insp.innostor_deep_probe_model {
                            cols[1].add_space(6.0);
                            let deep_btn = egui::Button::new(
                                RichText::new(format!(
                                    "⚡ 加载外部银灿固件深度直读闪存 ({})",
                                    model
                                ))
                                .strong()
                                .color(Color32::WHITE),
                            )
                            .fill(Color32::from_rgb(16, 185, 129))
                            .corner_radius(CornerRadius::same(6));
                            if cols[1]
                                .add(deep_btn)
                                .on_hover_text(
                                    "选择本地银灿量产工具的 RAM 诊断固件 (.bin)，临时卸载盘符读取双通道 8CE 闪存表后自动恢复（也可将 IS902E.bin 放在程序同目录 innostor/ 下自动加载）",
                                )
                                .clicked()
                            {
                                action = ChipPanelAction::InnostorDeepProbe(model.clone());
                            }
                        }
                    }

                    #[cfg(target_os = "windows")]
                    if insp.needs_windows_admin {
                        cols[1].add_space(6.0);
                        let btn = egui::Button::new(
                            RichText::new("🛡 以管理员身份重启（解锁底层闪存直读）")
                                .strong()
                                .color(Color32::WHITE),
                        )
                        .fill(Color32::from_rgb(37, 99, 235))
                        .corner_radius(CornerRadius::same(6));
                        if cols[1].add(btn).clicked() {
                            if let Ok(exe) = std::env::current_exe() {
                                use std::os::windows::process::CommandExt;
                                let ps = format!(
                                    "Start-Process -FilePath '{}' -Verb RunAs",
                                    exe.display().to_string().replace('\'', "''")
                                );
                                if std::process::Command::new("powershell")
                                    .args(["-NoProfile", "-NonInteractive", "-Command", &ps])
                                    .creation_flags(0x08000000)
                                    .spawn()
                                    .is_ok()
                                {
                                    std::process::exit(0);
                                }
                            }
                        }
                    }

                    if !insp.probe_trace.is_empty() {
                        cols[1].add_space(6.0);
                        egui::CollapsingHeader::new("🔍 底层探测诊断明细")
                            .default_open(false)
                            .show(&mut cols[1], |ui| {
                                for line in &insp.probe_trace {
                                    ui.label(RichText::new(line).monospace().small().weak());
                                }
                            });
                    }
                });
            } else {
                ui.weak("请在左侧选择一个 U 盘或存储设备以查看其主控与闪存信息。");
            }
        });

    ui.add_space(12.0);

    // ---- Card 2: Interactive Offline Chips.wdb Database Browser ----
    egui::Frame::new()
        .fill(card_bg)
        .stroke(Stroke::new(1.0_f32, card_border))
        .corner_radius(CornerRadius::same(8))
        .inner_margin(14.0)
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal_wrapped(|ui| {
                ui.strong("📚 Chips.wdb 离线芯片与闪存数据库速查");
                ui.separator();
                ui.selectable_value(
                    &mut panel_state.search_tab,
                    ChipSearchTab::Flash,
                    format!("查闪存颗粒 ({} 条)", db.flashes.len()),
                );
                ui.selectable_value(
                    &mut panel_state.search_tab,
                    ChipSearchTab::Controller,
                    format!("查主控芯片 ({} 条)", db.controllers.len()),
                );
            });

            ui.add_space(6.0);
            ui.horizontal(|ui| {
                let hint = match panel_state.search_tab {
                    ChipSearchTab::Flash => {
                        "输入 8 位 Flash ID（如 983A98B3 / 45 3C 98 B3）、厂商（Samsung/Micron/Hynix）、TLC/MLC 或料号..."
                    }
                    ChipSearchTab::Controller => {
                        "输入 VID/PID（如 090C 1000 / 13FE）、主控厂商（群联/慧荣/安国/芯邦）或主控型号（SM3281/PS2251）..."
                    }
                };
                ui.add(
                    egui::TextEdit::singleline(&mut panel_state.query)
                        .desired_width((ui.available_width() - 80.0).max(200.0))
                        .hint_text(hint),
                );
                if ui.button("清空").clicked() {
                    panel_state.query.clear();
                }
            });

            // Quick filter chips
            ui.add_space(4.0);
            ui.horizontal_wrapped(|ui| {
                ui.small(RichText::new("快捷筛选:").weak());
                match panel_state.search_tab {
                    ChipSearchTab::Flash => {
                        for kw in ["SLC", "MLC", "TLC", "QLC", "Toshiba", "Samsung", "Micron", "Hynix", "SanDisk", "SpecTek"] {
                            if ui.small_button(kw).clicked() {
                                panel_state.query = kw.to_string();
                            }
                        }
                    }
                    ChipSearchTab::Controller => {
                        for kw in ["慧荣", "群联", "安国", "芯邦", "银灿", "闪迪", "Realtek", "JMicron", "黑盘"] {
                            if ui.small_button(kw).clicked() {
                                panel_state.query = kw.to_string();
                            }
                        }
                    }
                }
            });

            ui.add_space(8.0);
            egui::ScrollArea::vertical()
                .id_salt("chips_wdb_table_scroll")
                .max_height(260.0)
                .show(ui, |ui| match panel_state.search_tab {
                    ChipSearchTab::Flash => {
                        let matches = db.search_flashes(&panel_state.query, 150);
                        if matches.is_empty() {
                            ui.weak("未找到匹配的闪存记录。");
                            return;
                        }
                        egui::Grid::new("wdb_flash_results")
                            .striped(true)
                            .num_columns(4)
                            .spacing([16.0, 5.0])
                            .show(ui, |ui| {
                                ui.strong("Flash ID");
                                ui.strong("闪存厂商");
                                ui.strong("类型 / 页");
                                ui.strong("对应颗粒料号 (CE 通道)");
                                ui.end_row();

                                for f in matches {
                                    ui.label(
                                        RichText::new(&f.flash_id)
                                            .monospace()
                                            .color(Color32::from_rgb(56, 189, 248)),
                                    );
                                    if f.is_downgraded {
                                        ui.colored_label(
                                            Color32::from_rgb(245, 158, 11),
                                            &f.manufacturer,
                                        );
                                    } else {
                                        ui.label(&f.manufacturer);
                                    }
                                    let tc = if f.flash_type.starts_with("SLC") {
                                        Color32::from_rgb(34, 197, 94)
                                    } else if f.flash_type.starts_with("MLC") {
                                        Color32::from_rgb(56, 189, 248)
                                    } else if f.flash_type.starts_with("TLC") {
                                        Color32::from_rgb(245, 158, 11)
                                    } else {
                                        Color32::from_rgb(248, 113, 113)
                                    };
                                    ui.colored_label(tc, &f.flash_type);
                                    ui.label(
                                        RichText::new(f.part_numbers.join("  |  "))
                                            .monospace()
                                            .small(),
                                    );
                                    ui.end_row();
                                }
                            });
                    }
                    ChipSearchTab::Controller => {
                        let matches = db.search_controllers(&panel_state.query, 150);
                        if matches.is_empty() {
                            ui.weak("未找到匹配的主控记录。");
                            return;
                        }
                        egui::Grid::new("wdb_ctrl_results")
                            .striped(true)
                            .num_columns(5)
                            .spacing([14.0, 5.0])
                            .show(ui, |ui| {
                                ui.strong("VID : PID");
                                ui.strong("主控厂商");
                                ui.strong("主控型号");
                                ui.strong("版本");
                                ui.strong("备注 / 警告");
                                ui.end_row();

                                for r in matches {
                                    ui.label(
                                        RichText::new(format!("{}:{}", r.vid, r.pid))
                                            .monospace()
                                            .color(Color32::from_rgb(56, 189, 248)),
                                    );
                                    ui.label(&r.chip_vendor);
                                    ui.label(
                                        RichText::new(&r.controller)
                                            .strong()
                                            .color(Color32::from_rgb(52, 211, 153)),
                                    );
                                    ui.small(&r.rev_filter);
                                    if r.note.contains("黑盘") || r.note.contains("山寨") {
                                        ui.colored_label(Color32::from_rgb(248, 113, 113), &r.note);
                                    } else {
                                        ui.small(&r.note);
                                    }
                                    ui.end_row();
                                }
                            });
                    }
                });
        });

    action
}

fn warning_banner(ui: &mut Ui, color: Color32, text: &str) {
    egui::Frame::new()
        .fill(Color32::from_rgba_unmultiplied(color.r(), color.g(), color.b(), 30))
        .stroke(Stroke::new(1.0_f32, color))
        .corner_radius(CornerRadius::same(6))
        .inner_margin(8.0)
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.colored_label(color, RichText::new(text).strong());
        });
    ui.add_space(6.0);
}

fn row_kv(ui: &mut Ui, k: &str, v: &str) {
    ui.label(RichText::new(k).small().weak());
    ui.label(v);
    ui.end_row();
}

fn row_kv_mono(ui: &mut Ui, k: &str, v: &str) {
    ui.label(RichText::new(k).small().weak());
    ui.label(RichText::new(v).monospace());
    ui.end_row();
}

fn row_kv_accent(ui: &mut Ui, k: &str, v: &str, color: Color32) {
    ui.label(RichText::new(k).small().weak());
    ui.label(RichText::new(v).strong().color(color));
    ui.end_row();
}
