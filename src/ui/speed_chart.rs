use crate::engine::{SpeedSample, TestMode};
use egui::{Color32, Ui};
use egui_plot::{Legend, Line, Plot, PlotPoints};

/// Renders the live interactive read/write speed curve using `egui_plot`.
pub fn render_speed_chart(
    ui: &mut Ui,
    mode: TestMode,
    write_curve: &[SpeedSample],
    read_curve: &[SpeedSample],
    height: f32,
) {
    let x_label = if mode == TestMode::Aging {
        "拷机轮次 (#)"
    } else {
        "测试位置 (GiB)"
    };

    ui.horizontal(|ui| {
        ui.strong("实时读写速度曲线");
        ui.weak(format!("（横轴：{} / 纵轴：MB/s）", x_label));
    });
    ui.add_space(4.0);

    let write_points: PlotPoints<'_> = write_curve
        .iter()
        .map(|s| [s.x, s.mb_per_sec])
        .collect();
    let read_points: PlotPoints<'_> = read_curve
        .iter()
        .map(|s| [s.x, s.mb_per_sec])
        .collect();

    let plot = Plot::new("speed_curve_plot")
        .height(height)
        .include_y(0.0)
        .x_axis_label(x_label)
        .y_axis_label("速度 (MB/s)")
        .legend(Legend::default())
        .allow_drag(false)
        .allow_zoom(false)
        .allow_scroll(false);

    plot.show(ui, |plot_ui| {
        if !write_curve.is_empty() {
            plot_ui.line(
                Line::new(write_points)
                    .name("写入速度 (MB/s)")
                    .color(Color32::from_rgb(56, 189, 248))
                    .width(2.0_f32),
            );
        }
        if !read_curve.is_empty() {
            plot_ui.line(
                Line::new(read_points)
                    .name("读取速度 (MB/s)")
                    .color(Color32::from_rgb(52, 211, 153))
                    .width(2.0_f32),
            );
        }
    });
}
