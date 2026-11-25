pub mod block_map;
pub mod chip_panel;
pub mod results_panel;
pub mod sidebar;
pub mod speed_chart;

use crate::chip::{probe_disk_chip, ChipDatabase, ChipInspection};
use crate::disk::{disk_info_for_custom_path, scan_disks, DiskInfo};
use crate::engine::report::{format_json_report, format_markdown_report};
use crate::engine::{spawn_test, LiveTestSnapshot, TestConfig, TestMode};
use chip_panel::{ChipPanelAction, ChipPanelState};
use egui::{Color32, CornerRadius, RichText, Stroke};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

pub struct MyDiskTestApp {
    disks: Vec<DiskInfo>,
    selected_disk: Option<usize>,
    allow_system_disk: bool,
    dark_mode: bool,
    config: TestConfig,
    /// When true, displays the ChipGenius-style Controller & NAND Flash inspection tab
    chip_view_active: bool,
    chip_db: ChipDatabase,
    chip_inspection: Option<ChipInspection>,
    chip_panel_state: ChipPanelState,
    /// Active or most recently completed test snapshot
    active_state: Option<Arc<Mutex<LiveTestSnapshot>>>,
    cancel_flag: Option<Arc<AtomicBool>>,
    /// Embedded app logo texture for the header bar
    logo_texture: Option<egui::TextureHandle>,
    /// Brief toast/status message (e.g., report saved confirmation)
    toast: Option<String>,
}

impl MyDiskTestApp {
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        crate::fonts::configure_cjk_fonts(&cc.egui_ctx);
        apply_visuals(&cc.egui_ctx, true);

        let logo_texture =
            eframe::icon_data::from_png_bytes(include_bytes!("../../assets/icon_256.png"))
                .ok()
                .map(|icon| {
                    let color_img = egui::ColorImage::from_rgba_unmultiplied(
                        [icon.width as usize, icon.height as usize],
                        &icon.rgba,
                    );
                    cc.egui_ctx.load_texture(
                        "app_logo",
                        color_img,
                        egui::TextureOptions::LINEAR,
                    )
                });

        let chip_db = ChipDatabase::load_default();
        let disks = scan_disks();
        // Auto-select first external/removable disk if present, otherwise first disk
        let selected_disk = disks
            .iter()
            .position(|d| d.kind.is_external_or_removable() && !d.is_system)
            .or_else(|| if disks.is_empty() { None } else { Some(0) });

        let chip_inspection = selected_disk
            .and_then(|i| disks.get(i))
            .map(|d| probe_disk_chip(d, &chip_db));

        Self {
            disks,
            selected_disk,
            allow_system_disk: false,
            dark_mode: true,
            config: TestConfig::default(),
            chip_view_active: false,
            chip_db,
            chip_inspection,
            chip_panel_state: ChipPanelState::default(),
            active_state: None,
            cancel_flag: None,
            logo_texture,
            toast: None,
        }
    }

    fn reprobe_selected_chip(&mut self) {
        self.chip_inspection = self
            .selected_disk
            .and_then(|i| self.disks.get(i))
            .map(|d| probe_disk_chip(d, &self.chip_db));
    }

    fn is_running(&self) -> bool {
        self.active_state
            .as_ref()
            .and_then(|s| s.lock().ok())
            .map(|s| s.running)
            .unwrap_or(false)
    }

    fn current_snapshot(&self) -> Option<LiveTestSnapshot> {
        self.active_state
            .as_ref()
            .and_then(|s| s.lock().ok())
            .map(|s| s.clone())
    }

    fn refresh_disks(&mut self) {
        let prev_mount = self
            .selected_disk
            .and_then(|i| self.disks.get(i))
            .map(|d| d.mount_point.clone());
        self.disks = scan_disks();
        self.selected_disk = prev_mount
            .and_then(|m| self.disks.iter().position(|d| d.mount_point == m))
            .or_else(|| {
                self.disks
                    .iter()
                    .position(|d| d.kind.is_external_or_removable() && !d.is_system)
            })
            .or_else(|| if self.disks.is_empty() { None } else { Some(0) });
        self.reprobe_selected_chip();
        self.toast = Some(format!("已刷新，发现 {} 个挂载卷。", self.disks.len()));
    }

    fn start_test(&mut self, ctx: &egui::Context) {
        let Some(disk) = self.selected_disk.and_then(|i| self.disks.get(i)).cloned() else {
            self.toast = Some("请先选择要检测的磁盘！".to_string());
            return;
        };
        if !self.config.mode.is_read_only() {
            if disk.is_system && !self.allow_system_disk {
                self.toast = Some("已拦截系统盘写入测试：如需测试系统盘请先在左侧勾选解锁，或使用纯只读「错误扫描」。".to_string());
                return;
            }
            if disk.is_read_only {
                self.toast = Some("目标磁盘为只读状态，无法进行写入检测（可使用纯只读「错误扫描」）。".to_string());
                return;
            }
        }

        let egui_ctx = ctx.clone();
        let repaint_cb: Arc<dyn Fn() + Send + Sync> = Arc::new(move || {
            egui_ctx.request_repaint();
        });

        let (state, cancel) = spawn_test(disk, self.config.clone(), Some(repaint_cb));
        self.active_state = Some(state);
        self.cancel_flag = Some(cancel);
        self.toast = None;
    }

    fn stop_test(&mut self) {
        if let Some(ref flag) = self.cancel_flag {
            flag.store(true, Ordering::Relaxed);
            self.toast = Some("正在安全停止测试并清理临时文件...".to_string());
        }
    }

    fn export_report(&mut self, as_json: bool) {
        let Some(snap) = self.current_snapshot() else {
            return;
        };
        let ext = if as_json { "json" } else { "md" };
        let default_name = format!(
            "MyDiskTest_Report_{}.{}",
            chrono::Local::now().format("%Y%m%d_%H%M%S"),
            ext
        );
        let dialog = rfd::FileDialog::new()
            .set_title("导出 MyDiskTest 检测报告")
            .set_file_name(&default_name);

        if let Some(path) = dialog.save_file() {
            let content_res = if as_json {
                format_json_report(&snap).map_err(|e| e.to_string())
            } else {
                Ok(format_markdown_report(&snap))
            };
            match content_res {
                Ok(content) => match std::fs::write(&path, content) {
                    Ok(()) => {
                        self.toast = Some(format!("报告已保存至：{}", path.display()));
                    }
                    Err(e) => {
                        self.toast = Some(format!("保存报告失败：{}", e));
                    }
                },
                Err(e) => {
                    self.toast = Some(format!("生成报告失败：{}", e));
                }
            }
        }
    }
}

impl eframe::App for MyDiskTestApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        let is_running = self.is_running();
        if is_running {
            ctx.request_repaint_after(std::time::Duration::from_millis(100));
        }

        // Top App Bar
        egui::TopBottomPanel::top("top_header")
            .frame(
                egui::Frame::side_top_panel(&ctx.style())
                    .inner_margin(egui::Margin::symmetric(14, 10)),
            )
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    if let Some(ref tex) = self.logo_texture {
                        ui.add(
                            egui::Image::new(tex)
                                .fit_to_exact_size(egui::vec2(26.0, 26.0))
                                .corner_radius(CornerRadius::same(6)),
                        );
                    }
                    ui.label(
                        RichText::new("MyDiskTest Next")
                            .strong()
                            .size(18.0)
                            .color(Color32::from_rgb(56, 189, 248)),
                    );
                    ui.label(
                        RichText::new("v1.2.1  •  扩容检测 / 坏块扫描 / HD Tune 错误扫描 / 主控闪存 / 速度基准 / 老化拷机")
                            .weak()
                            .small(),
                    );

                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        let theme_lbl = if self.dark_mode { "☀ 浅色" } else { "🌙 深色" };
                        if ui.button(theme_lbl).clicked() {
                            self.dark_mode = !self.dark_mode;
                            apply_visuals(ctx, self.dark_mode);
                        }

                        let has_snap = self.active_state.is_some();
                        if ui
                            .add_enabled(has_snap && !is_running, egui::Button::new("导出 JSON"))
                            .clicked()
                        {
                            self.export_report(true);
                        }
                        if ui
                            .add_enabled(has_snap && !is_running, egui::Button::new("📄 导出报告"))
                            .clicked()
                        {
                            self.export_report(false);
                        }

                        if let Some(ref toast) = self.toast {
                            ui.separator();
                            ui.label(
                                RichText::new(toast)
                                    .small()
                                    .color(Color32::from_rgb(52, 211, 153)),
                            );
                        }
                    });
                });
            });

        // Left Sidebar: Drive list + Hardware info
        egui::SidePanel::left("drive_sidebar")
            .default_width(275.0)
            .min_width(240.0)
            .max_width(360.0)
            .frame(
                egui::Frame::side_top_panel(&ctx.style())
                    .inner_margin(egui::Margin::symmetric(12, 12)),
            )
            .show(ctx, |ui| {
                match sidebar::render_sidebar(
                    ui,
                    &self.disks,
                    self.selected_disk,
                    self.chip_inspection.as_ref(),
                    is_running,
                    &mut self.allow_system_disk,
                ) {
                    sidebar::SidebarAction::Refresh => self.refresh_disks(),
                    sidebar::SidebarAction::SelectDisk(idx) => {
                        self.selected_disk = Some(idx);
                        self.allow_system_disk = false;
                        self.reprobe_selected_chip();
                    }
                    sidebar::SidebarAction::PickCustomFolder => {
                        if let Some(folder) = rfd::FileDialog::new()
                            .set_title("选择要测试的挂载目录或文件夹")
                            .pick_folder()
                        {
                            let custom = disk_info_for_custom_path(folder);
                            self.disks.insert(0, custom);
                            self.selected_disk = Some(0);
                            self.reprobe_selected_chip();
                            self.toast = Some("已添加并选中自定义测试目录。".to_string());
                        }
                    }
                    sidebar::SidebarAction::None => {}
                }
            });

        // Main Content Panel
        egui::CentralPanel::default()
            .frame(
                egui::Frame::central_panel(&ctx.style())
                    .inner_margin(egui::Margin::symmetric(16, 14)),
            )
            .show(ctx, |ui| {
                // 1. Mode Selector Bar
                self.render_mode_tabs(ui, is_running);
                ui.add_space(10.0);

                if self.chip_view_active {
                    let selected = self.selected_disk.and_then(|i| self.disks.get(i)).cloned();
                    egui::ScrollArea::vertical()
                        .id_salt("chip_panel_main_scroll")
                        .show(ui, |ui| {
                            match chip_panel::render_chip_panel(
                                ui,
                                selected.as_ref(),
                                self.chip_inspection.as_ref(),
                                &self.chip_db,
                                &mut self.chip_panel_state,
                            ) {
                                ChipPanelAction::Reprobe => {
                                    self.reprobe_selected_chip();
                                    self.toast = Some("已完成主控与闪存重新探测。".to_string());
                                }
                                ChipPanelAction::ImportWdb => {
                                    if let Some(path) = rfd::FileDialog::new()
                                        .set_title("选择新版 Chips.wdb 数据库文件")
                                        .add_filter("ChipGenius Database", &["wdb"])
                                        .pick_file()
                                    {
                                        match ChipDatabase::load_from_file(&path) {
                                            Ok(new_db) => {
                                                let msg = format!(
                                                    "已加载新版 Chips.wdb（日期 {}，{} 主控 / {} 闪存）",
                                                    new_db.db_date,
                                                    new_db.controllers.len(),
                                                    new_db.flashes.len()
                                                );
                                                self.chip_db = new_db;
                                                self.reprobe_selected_chip();
                                                self.toast = Some(msg);
                                            }
                                            Err(e) => {
                                                self.toast = Some(e);
                                            }
                                        }
                                    }
                                }
                                ChipPanelAction::CopiedReport => {
                                    self.toast = Some("已复制 ChipGenius 格式主控闪存报告到剪贴板！".to_string());
                                }
                                ChipPanelAction::None => {}
                            }
                        });
                    return;
                }

                // 2. Mode Options + Start/Stop Button
                self.render_config_and_controls(ui, ctx, is_running);
                ui.add_space(10.0);

                // 3. Live Test Progress, Verdict, Charts & Block Grid
                egui::ScrollArea::vertical()
                    .id_salt("main_content_scroll")
                    .show(ui, |ui| {
                        if let Some(snap) = self.current_snapshot() {
                            results_panel::render_verdict_banner(ui, &snap.verdict);

                            // Progress bar + phase
                            ui.horizontal(|ui| {
                                ui.strong(&snap.phase_label);
                                ui.with_layout(
                                    egui::Layout::right_to_left(egui::Align::Center),
                                    |ui| {
                                        let dio_badge = if snap.direct_io_active {
                                            ("⚡ Direct I/O 无缓冲直写", Color32::from_rgb(34, 197, 94))
                                        } else {
                                            ("🔄 同步落盘模式", Color32::from_rgb(245, 158, 11))
                                        };
                                        ui.label(RichText::new(dio_badge.0).small().color(dio_badge.1));
                                    },
                                );
                            });
                            ui.add_space(4.0);
                            ui.add(
                                egui::ProgressBar::new(snap.progress.clamp(0.0, 1.0))
                                    .show_percentage()
                                    .animate(snap.running)
                                    .desired_height(18.0),
                            );
                            ui.add_space(10.0);

                            // KPI tiles
                            results_panel::render_kpi_cards(ui, &snap);
                            ui.add_space(12.0);

                            // Mode-specific visualizations
                            match snap.config.mode {
                                TestMode::QuickExpand | TestMode::FullVerify => {
                                    block_map::render_block_map(ui, &snap.blocks);
                                    ui.add_space(12.0);
                                    speed_chart::render_speed_chart(
                                        ui,
                                        snap.config.mode,
                                        &snap.write_curve,
                                        &snap.read_curve,
                                        200.0,
                                    );
                                }
                                TestMode::ErrorScan => {
                                    block_map::render_block_map(ui, &snap.blocks);
                                    ui.add_space(10.0);
                                    results_panel::render_bad_sectors_table(ui, &snap.bad_sectors);
                                    ui.add_space(12.0);
                                    speed_chart::render_speed_chart(
                                        ui,
                                        snap.config.mode,
                                        &snap.write_curve,
                                        &snap.read_curve,
                                        190.0,
                                    );
                                }
                                TestMode::Benchmark => {
                                    results_panel::render_benchmark_table(ui, &snap.benchmark);
                                    ui.add_space(12.0);
                                    speed_chart::render_speed_chart(
                                        ui,
                                        snap.config.mode,
                                        &snap.write_curve,
                                        &snap.read_curve,
                                        230.0,
                                    );
                                }
                                TestMode::Aging => {
                                    results_panel::render_aging_table(ui, &snap.aging_records);
                                    ui.add_space(12.0);
                                    speed_chart::render_speed_chart(
                                        ui,
                                        snap.config.mode,
                                        &snap.write_curve,
                                        &snap.read_curve,
                                        210.0,
                                    );
                                }
                            }

                            ui.add_space(12.0);
                            render_log_console(ui, &snap.logs);
                        } else {
                            self.render_welcome_placeholder(ui);
                        }
                    });
            });
    }
}

impl MyDiskTestApp {
    fn render_mode_tabs(&mut self, ui: &mut egui::Ui, is_running: bool) {
        let modes = [
            (TestMode::QuickExpand, "⚡ 快速扩容"),
            (TestMode::FullVerify, "🛡 全盘坏块"),
            (TestMode::ErrorScan, "🔍 错误扫描"),
            (TestMode::Benchmark, "🚀 读写基准"),
            (TestMode::Aging, "🔥 老化拷机"),
        ];

        ui.columns(6, |cols| {
            for (i, (mode, label)) in modes.iter().enumerate() {
                let selected = !self.chip_view_active && self.config.mode == *mode;
                let btn = egui::Button::new(
                    RichText::new(*label)
                        .strong()
                        .size(12.5)
                        .color(if selected {
                            Color32::WHITE
                        } else {
                            cols[i].visuals().text_color()
                        }),
                )
                .fill(if selected {
                    Color32::from_rgb(37, 99, 235)
                } else {
                    cols[i].visuals().widgets.inactive.weak_bg_fill
                })
                .corner_radius(CornerRadius::same(8))
                .min_size(egui::vec2(cols[i].available_width(), 36.0));

                if cols[i].add_enabled(!is_running, btn).clicked() {
                    self.chip_view_active = false;
                    self.config.mode = *mode;
                    // Adjust sensible defaults when switching modes
                    match mode {
                        TestMode::QuickExpand => {
                            self.config.target_size_mb = 0;
                            if self.config.grid_blocks > 256 {
                                self.config.grid_blocks = 128;
                            }
                        }
                        TestMode::FullVerify => {
                            self.config.target_size_mb = 0;
                            self.config.grid_blocks = 400;
                        }
                        TestMode::ErrorScan => {
                            self.config.grid_blocks = 1000;
                        }
                        TestMode::Benchmark => {
                            if self.config.target_size_mb == 0 {
                                self.config.target_size_mb = 1024;
                            }
                        }
                        TestMode::Aging => {
                            if self.config.target_size_mb == 0 {
                                self.config.target_size_mb = 1024;
                            }
                        }
                    }
                }
            }

            // 6th Tab: USB Controller & NAND Flash Inspector (Chips.wdb)
            let chip_selected = self.chip_view_active;
            let chip_btn = egui::Button::new(
                RichText::new("🔬 主控闪存")
                    .strong()
                    .size(12.5)
                    .color(if chip_selected {
                        Color32::WHITE
                    } else {
                        cols[5].visuals().text_color()
                    }),
            )
            .fill(if chip_selected {
                Color32::from_rgb(14, 116, 144)
            } else {
                cols[5].visuals().widgets.inactive.weak_bg_fill
            })
            .corner_radius(CornerRadius::same(8))
            .min_size(egui::vec2(cols[5].available_width(), 36.0));

            if cols[5].add_enabled(!is_running, chip_btn).clicked() {
                self.chip_view_active = true;
                self.reprobe_selected_chip();
            }
        });
    }

    fn render_config_and_controls(
        &mut self,
        ui: &mut egui::Ui,
        ctx: &egui::Context,
        is_running: bool,
    ) {
        let dark = ui.visuals().dark_mode;
        let bg = if dark {
            Color32::from_rgb(25, 30, 38)
        } else {
            Color32::from_rgb(245, 248, 252)
        };
        let border = if dark {
            Color32::from_rgb(48, 56, 68)
        } else {
            Color32::from_rgb(214, 222, 232)
        };

        egui::Frame::new()
            .fill(bg)
            .stroke(Stroke::new(1.0_f32, border))
            .corner_radius(CornerRadius::same(8))
            .inner_margin(12.0)
            .show(ui, |ui| {
                ui.label(
                    RichText::new(self.config.mode.description_zh())
                        .small()
                        .weak(),
                );
                ui.add_space(8.0);

                ui.horizontal_wrapped(|ui| {
                    let selected_has_raw = self
                        .selected_disk
                        .and_then(|i| self.disks.get(i))
                        .map(|d| d.raw_disk_path.is_some() && !d.is_system)
                        .unwrap_or(false);

                    ui.add_enabled_ui(!is_running, |ui| {
                        if self.config.mode == TestMode::QuickExpand && selected_has_raw {
                            ui.checkbox(
                                &mut self.config.use_raw_lba_probe,
                                "⚡ 极速底层 LBA 探针（5~15秒测完任意容量，需管理员授权，无损备份还原）",
                            )
                            .on_hover_text(
                                "与原版 MyDiskTest / ValiDrive 相同原理：直接对全盘跨边界 LBA 写入极小指纹探针并回读校验，测前备份原扇区、测后逆序无损还原并自动重新挂载，128GB~2TB 均只需约 5~15 秒。",
                            );
                            ui.separator();
                        }

                        let raw_mode_active = self.config.mode == TestMode::QuickExpand
                            && selected_has_raw
                            && self.config.use_raw_lba_probe;

                        if self.config.mode == TestMode::ErrorScan {
                            ui.checkbox(&mut self.config.error_scan_quick, "⚡ 快速扫描 (Quick Scan)")
                                .on_hover_text("与 HD Tune Pro 快速扫描一致：逐格抽取 1 MiB 高速表面直读，几秒~几十秒即可覆盖 TB 级硬盘；取消勾选则逐字节 100% 完整表面扫描。");
                            ui.separator();

                            ui.label("网格精度:");
                            egui::ComboBox::from_id_salt("escan_grid_combo")
                                .selected_text(format!("{} 格", self.config.grid_blocks))
                                .width(110.0)
                                .show_ui(ui, |ui| {
                                    ui.selectable_value(&mut self.config.grid_blocks, 400, "400 格");
                                    ui.selectable_value(
                                        &mut self.config.grid_blocks,
                                        1000,
                                        "1000 格 (经典 50×20)",
                                    );
                                    ui.selectable_value(
                                        &mut self.config.grid_blocks,
                                        2500,
                                        "2500 格 (超精细 50×50)",
                                    );
                                });
                            ui.separator();

                            ui.label("范围(GB):");
                            ui.add(
                                egui::DragValue::new(&mut self.config.error_scan_start_gb)
                                    .range(0.0..=16384.0)
                                    .speed(1.0)
                                    .prefix("起 "),
                            );
                            ui.add(
                                egui::DragValue::new(&mut self.config.error_scan_end_gb)
                                    .range(0.0..=16384.0)
                                    .speed(1.0)
                                    .prefix("止 "),
                            )
                            .on_hover_text("结束位置设为 0 表示扫描至磁盘末尾");
                            ui.separator();

                            ui.label("慢速预警:");
                            ui.add(
                                egui::DragValue::new(&mut self.config.slow_threshold_mb_s)
                                    .range(0.5..=200.0)
                                    .speed(0.5)
                                    .suffix(" MB/s"),
                            );
                            ui.separator();
                            ui.colored_label(
                                Color32::from_rgb(52, 211, 153),
                                "🔒 纯只读零写入",
                            );
                        } else if raw_mode_active {
                            ui.label("采样探针数:");
                            egui::ComboBox::from_id_salt("probe_count_combo")
                                .selected_text(format!("{} 点（含容量边界）", self.config.grid_blocks))
                                .width(150.0)
                                .show_ui(ui, |ui| {
                                    ui.selectable_value(&mut self.config.grid_blocks, 64, "64 点（超快 ~3秒）");
                                    ui.selectable_value(&mut self.config.grid_blocks, 128, "128 点（推荐 ~8秒）");
                                    ui.selectable_value(&mut self.config.grid_blocks, 256, "256 点（精细 ~15秒）");
                                });
                        } else {
                            // Target capacity preset selector
                            ui.label("测试容量:");
                            let size_label = match self.config.target_size_mb {
                                0 => "全部可用空间（推荐）".to_string(),
                                256 => "256 MiB（极速验证）".to_string(),
                                512 => "512 MiB".to_string(),
                                1024 => "1 GiB".to_string(),
                                2048 => "2 GiB".to_string(),
                                4096 => "4 GiB".to_string(),
                                16384 => "16 GiB".to_string(),
                                other => format!("{} MiB", other),
                            };
                            egui::ComboBox::from_id_salt("target_size_combo")
                                .selected_text(size_label)
                                .width(155.0)
                                .show_ui(ui, |ui| {
                                    if matches!(
                                        self.config.mode,
                                        TestMode::QuickExpand | TestMode::FullVerify | TestMode::Aging
                                    ) {
                                        ui.selectable_value(
                                            &mut self.config.target_size_mb,
                                            0,
                                            "全部可用空间（推荐）",
                                        );
                                    }
                                    ui.selectable_value(
                                        &mut self.config.target_size_mb,
                                        256,
                                        "256 MiB（极速验证）",
                                    );
                                    ui.selectable_value(&mut self.config.target_size_mb, 512, "512 MiB");
                                    ui.selectable_value(&mut self.config.target_size_mb, 1024, "1 GiB");
                                    ui.selectable_value(&mut self.config.target_size_mb, 2048, "2 GiB");
                                    ui.selectable_value(&mut self.config.target_size_mb, 4096, "4 GiB");
                                    ui.selectable_value(
                                        &mut self.config.target_size_mb,
                                        16384,
                                        "16 GiB",
                                    );
                                });
                        }

                        if matches!(
                            self.config.mode,
                            TestMode::QuickExpand | TestMode::FullVerify
                        ) {
                            ui.separator();
                            ui.label("慢速预警阈值:");
                            ui.add(
                                egui::DragValue::new(&mut self.config.slow_threshold_mb_s)
                                    .range(0.5..=200.0)
                                    .speed(0.5)
                                    .suffix(" MB/s"),
                            );
                        }

                        if self.config.mode == TestMode::Aging {
                            ui.separator();
                            ui.label("拷机轮次:");
                            let cyc_label = if self.config.aging_cycles == 0 {
                                "无限循环（手动停止）".to_string()
                            } else {
                                format!("{} 轮", self.config.aging_cycles)
                            };
                            egui::ComboBox::from_id_salt("aging_cycles_combo")
                                .selected_text(cyc_label)
                                .width(140.0)
                                .show_ui(ui, |ui| {
                                    ui.selectable_value(&mut self.config.aging_cycles, 3, "3 轮");
                                    ui.selectable_value(&mut self.config.aging_cycles, 5, "5 轮");
                                    ui.selectable_value(&mut self.config.aging_cycles, 10, "10 轮");
                                    ui.selectable_value(&mut self.config.aging_cycles, 20, "20 轮");
                                    ui.selectable_value(
                                        &mut self.config.aging_cycles,
                                        0,
                                        "无限循环（手动停止）",
                                    );
                                });
                        }

                        if !raw_mode_active && self.config.mode != TestMode::ErrorScan {
                            ui.separator();
                            ui.checkbox(&mut self.config.auto_cleanup, "测试后自动清理临时文件")
                                .on_hover_text("测试采用安全文件模式，不会破坏磁盘原有分区与数据，完成后自动删除临时测试文件");
                        }
                    });

                    // Start / Stop primary button aligned right
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if is_running {
                            let stop_btn = egui::Button::new(
                                RichText::new("⏹ 停止测试").strong().color(Color32::WHITE),
                            )
                            .fill(Color32::from_rgb(220, 38, 38))
                            .corner_radius(CornerRadius::same(6))
                            .min_size(egui::vec2(120.0, 32.0));
                            if ui.add(stop_btn).clicked() {
                                self.stop_test();
                            }
                        } else {
                            let read_only_mode = self.config.mode.is_read_only();
                            let can_start = self
                                .selected_disk
                                .and_then(|i| self.disks.get(i))
                                .map(|d| {
                                    read_only_mode
                                        || (!d.is_read_only && (!d.is_system || self.allow_system_disk))
                                })
                                .unwrap_or(false);

                            let btn_text = if read_only_mode {
                                "▶ 开始扫描"
                            } else {
                                "▶ 开始测试"
                            };
                            let start_btn = egui::Button::new(
                                RichText::new(btn_text).strong().color(Color32::WHITE),
                            )
                            .fill(if can_start {
                                Color32::from_rgb(22, 163, 74)
                            } else {
                                Color32::from_rgb(75, 85, 99)
                            })
                            .corner_radius(CornerRadius::same(6))
                            .min_size(egui::vec2(120.0, 32.0));

                            if ui.add_enabled(can_start, start_btn).clicked() {
                                self.start_test(ctx);
                            }
                        }
                    });
                });
            });
    }

    fn render_welcome_placeholder(&self, ui: &mut egui::Ui) {
        ui.add_space(16.0);
        egui::Frame::group(ui.style())
            .inner_margin(20.0)
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.heading("欢迎使用 MyDiskTest Next");
                ui.add_space(6.0);
                ui.label("专为最新 Windows 11 与 macOS (Apple Silicon / Intel) 打造的原生高性能磁盘扩容与健康检测工具。");
                ui.add_space(10.0);

                ui.label(RichText::new("核心安全与精准保障：").strong());
                ui.label("• 绕过系统缓存直写直读（macOS F_NOCACHE + F_FULLFSYNC / Windows FILE_FLAG_NO_BUFFERING），杜绝内存缓存导致的虚高速度与漏检扩容盘。");
                ui.label("• 采用安全文件分片测试（自动适配 FAT32 单文件 4GB 限制），绝不破坏磁盘分区表与原有文件，测试结束或取消均自动清理临时文件。");
                ui.label("• 每个 4KB 扇区内置唯一批次 ID、全局逻辑偏移与 64 位校验和，可精准捕获「循环覆盖扩容」「黑洞丢弃扩容」与「闪存位翻转坏块」。");
                ui.add_space(8.0);
                ui.weak("请在左侧选择待测 U 盘 / SD 卡 / 移动硬盘（或选择自定义测试目录），点击右上角「▶ 开始测试」。");
            });
    }
}

fn render_log_console(ui: &mut egui::Ui, logs: &[String]) {
    ui.horizontal(|ui| {
        ui.strong("实时诊断日志");
        ui.weak(format!("（共 {} 条）", logs.len()));
    });
    ui.add_space(4.0);

    egui::Frame::group(ui.style())
        .inner_margin(8.0)
        .show(ui, |ui| {
            egui::ScrollArea::vertical()
                .id_salt("log_console_scroll")
                .max_height(150.0)
                .stick_to_bottom(true)
                .show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    for line in logs {
                        let color = if line.contains('🚨') || line.contains('❌') {
                            Color32::from_rgb(248, 113, 113)
                        } else if line.contains('⚠') {
                            Color32::from_rgb(251, 191, 36)
                        } else if line.contains('✅') {
                            Color32::from_rgb(74, 222, 128)
                        } else {
                            ui.visuals().text_color()
                        };
                        ui.label(RichText::new(line).monospace().size(12.0).color(color));
                    }
                });
        });
}

fn apply_visuals(ctx: &egui::Context, dark: bool) {
    let mut visuals = if dark {
        egui::Visuals::dark()
    } else {
        egui::Visuals::light()
    };
    visuals.window_corner_radius = CornerRadius::same(8);
    if dark {
        visuals.panel_fill = Color32::from_rgb(17, 20, 26);
        visuals.extreme_bg_color = Color32::from_rgb(13, 16, 22);
    }
    ctx.set_visuals(visuals);
}
