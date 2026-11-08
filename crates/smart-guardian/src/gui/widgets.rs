//! Custom painted widgets: gauges, time-series charts, badges and the app icon.

use chrono::{Local, TimeZone};
use eframe::egui::{self, pos2, vec2, Align2, Color32, FontId, Pos2, Rect, RichText, Sense, Shape, Stroke, Ui};
use sg_core::{HealthStatus, Severity};

pub const GREEN: Color32 = Color32::from_rgb(0x2E, 0xB8, 0x5C);
pub const LIME: Color32 = Color32::from_rgb(0x8B, 0xC3, 0x4A);
pub const AMBER: Color32 = Color32::from_rgb(0xF2, 0xB7, 0x05);
pub const ORANGE: Color32 = Color32::from_rgb(0xF2, 0x7A, 0x1A);
pub const RED: Color32 = Color32::from_rgb(0xE0, 0x3E, 0x3E);
pub const GRAY: Color32 = Color32::from_rgb(0x8A, 0x90, 0x99);
pub const BLUE: Color32 = Color32::from_rgb(0x3B, 0x8E, 0xEA);
pub const PURPLE: Color32 = Color32::from_rgb(0x9B, 0x6B, 0xDF);
pub const TEAL: Color32 = Color32::from_rgb(0x1F, 0xB5, 0xAD);

pub fn score_color(v: Option<u8>, status: HealthStatus) -> Color32 {
    if status == HealthStatus::Failed {
        return RED;
    }
    match v {
        None => GRAY,
        Some(x) if x >= 90 => GREEN,
        Some(x) if x >= 70 => LIME,
        Some(x) if x >= 50 => AMBER,
        Some(x) if x >= 25 => ORANGE,
        Some(_) => RED,
    }
}

pub fn severity_color(s: Severity) -> Color32 {
    match s {
        Severity::Info => BLUE,
        Severity::Warning => AMBER,
        Severity::Critical => RED,
    }
}

pub fn temp_color(t: i32, (warn, crit): (i32, i32)) -> Color32 {
    if t >= crit {
        RED
    } else if t >= warn {
        ORANGE
    } else if t >= warn - 8 {
        AMBER
    } else {
        GREEN
    }
}

/// Small rounded label.
pub fn chip(ui: &mut Ui, text: &str, color: Color32) {
    let galley = ui.painter().layout_no_wrap(text.to_string(), FontId::proportional(11.5), Color32::WHITE);
    let size = galley.size() + vec2(12.0, 4.0);
    let (rect, _) = ui.allocate_exact_size(size, Sense::hover());
    ui.painter().rect_filled(rect, 8.0, color);
    ui.painter().galley(rect.min + vec2(6.0, 2.0), galley, Color32::WHITE);
}

/// 270° arc gauge showing a percentage.
pub fn gauge(ui: &mut Ui, value: Option<u8>, label: &str, color: Color32, size: f32) {
    let (rect, _) = ui.allocate_exact_size(vec2(size, size * 0.92), Sense::hover());
    let p = ui.painter();
    let c = pos2(rect.center().x, rect.top() + size * 0.44);
    let r = size * 0.36;
    let start = 135f32.to_radians();
    let sweep = 270f32.to_radians();
    let arc = |frac: f32| -> Vec<Pos2> {
        let n = 72;
        (0..=n)
            .map(|i| {
                let a = start + sweep * frac * i as f32 / n as f32;
                c + vec2(a.cos(), a.sin()) * r
            })
            .collect()
    };
    let track = ui.visuals().widgets.inactive.bg_fill;
    let w = size * 0.075;
    p.add(Shape::line(arc(1.0), Stroke::new(w, track)));
    if let Some(v) = value.filter(|&v| v > 0) {
        p.add(Shape::line(arc(v as f32 / 100.0), Stroke::new(w, color)));
    }
    let txt = value.map(|v| format!("{v}%")).unwrap_or_else(|| "–".into());
    p.text(c, Align2::CENTER_CENTER, txt, FontId::proportional(size * 0.2), ui.visuals().strong_text_color());
    p.text(
        pos2(c.x, rect.bottom()),
        Align2::CENTER_BOTTOM,
        label,
        FontId::proportional(13.0),
        ui.visuals().weak_text_color(),
    );
}

pub struct Series {
    pub name: String,
    pub color: Color32,
    /// (unix seconds, value), sorted by time.
    pub pts: Vec<(f64, f64)>,
}

pub struct ChartOpts<'a> {
    pub height: f32,
    pub y_zero: bool,
    pub y_max: Option<f64>,
    /// Smallest visible y span (keeps flat series readable).
    pub min_span: f64,
    pub refs: &'a [(f64, Color32, &'a str)],
}

impl Default for ChartOpts<'_> {
    fn default() -> Self {
        ChartOpts {
            height: 170.0,
            y_zero: false,
            y_max: None,
            min_span: 4.0,
            refs: &[],
        }
    }
}

fn fmt_time(t: f64, span: f64) -> String {
    let dt = Local.timestamp_opt(t as i64, 0).single();
    match dt {
        Some(d) if span > 3.0 * 86_400.0 => d.format("%b %d").to_string(),
        Some(d) if span > 86_400.0 => d.format("%a %H:%M").to_string(),
        Some(d) => d.format("%H:%M").to_string(),
        None => String::new(),
    }
}

/// Time-series line chart with hover read-out.
pub fn chart(ui: &mut Ui, title: &str, series: &[Series], fmt: &dyn Fn(f64) -> String, o: ChartOpts) {
    ui.horizontal(|ui| {
        ui.label(RichText::new(title).strong());
        if series.len() > 1 {
            ui.add_space(10.0);
            for s in series {
                ui.label(RichText::new("━").color(s.color));
                ui.label(RichText::new(&s.name).size(12.0));
            }
        }
    });
    let w = ui.available_width().max(200.0);
    let (rect, resp) = ui.allocate_exact_size(vec2(w, o.height), Sense::hover());
    let p = ui.painter_at(rect);
    let vis = ui.visuals();
    p.rect_filled(rect, 6.0, vis.extreme_bg_color);
    let weak = vis.weak_text_color();
    let grid = vis.widgets.noninteractive.bg_stroke.color;
    let plot = Rect::from_min_max(rect.min + vec2(58.0, 12.0), rect.max - vec2(14.0, 24.0));

    let all: Vec<(f64, f64)> = series.iter().flat_map(|s| s.pts.iter().copied()).collect();
    if all.is_empty() {
        p.text(rect.center(), Align2::CENTER_CENTER, "No data recorded for this period yet", FontId::proportional(13.0), weak);
        return;
    }
    let (mut x0, mut x1) = all.iter().fold((f64::MAX, f64::MIN), |(a, b), (x, _)| (a.min(*x), b.max(*x)));
    if x1 - x0 < 1.0 {
        x0 -= 1800.0;
        x1 += 1800.0;
    }
    let (mut y0, mut y1) = all.iter().fold((f64::MAX, f64::MIN), |(a, b), (_, y)| (a.min(*y), b.max(*y)));
    for (v, _, _) in o.refs {
        if *v < y1 + (y1 - y0).max(5.0) * 2.0 {
            y0 = y0.min(*v);
            y1 = y1.max(*v);
        }
    }
    if o.y_zero {
        y0 = y0.min(0.0);
    }
    // Never let a near-flat series collapse the axis into repeated labels.
    let min_span = o.min_span.max(y1.abs().max(y0.abs()) * 0.02).max(1e-6);
    if y1 - y0 < min_span {
        let mid = (y0 + y1) / 2.0;
        y0 = mid - min_span / 2.0;
        y1 = mid + min_span / 2.0;
        if o.y_zero && y0 < 0.0 && mid >= 0.0 {
            y1 -= y0;
            y0 = 0.0;
        }
    }
    let pad = (y1 - y0) * 0.08;
    y1 += pad;
    if !(o.y_zero && y0 >= 0.0 && y0 < pad) {
        y0 -= pad;
    }
    if let Some(m) = o.y_max {
        y1 = y1.min(m);
        if y0 >= y1 {
            y0 = y1 - min_span;
        }
    }
    let mx = |x: f64| plot.left() + ((x - x0) / (x1 - x0)) as f32 * plot.width();
    let my = |y: f64| plot.bottom() - ((y - y0) / (y1 - y0)) as f32 * plot.height();

    for i in 0..=4 {
        let v = y0 + (y1 - y0) * i as f64 / 4.0;
        let y = my(v);
        p.line_segment([pos2(plot.left(), y), pos2(plot.right(), y)], Stroke::new(0.5, grid));
        p.text(pos2(plot.left() - 6.0, y), Align2::RIGHT_CENTER, fmt(v), FontId::proportional(11.0), weak);
    }
    let span = x1 - x0;
    for (i, align) in [(0, Align2::LEFT_TOP), (1, Align2::CENTER_TOP), (2, Align2::RIGHT_TOP)] {
        let t = x0 + span * i as f64 / 2.0;
        p.text(pos2(mx(t), plot.bottom() + 5.0), align, fmt_time(t, span), FontId::proportional(11.0), weak);
    }
    for (v, c, label) in o.refs {
        if *v >= y0 && *v <= y1 {
            let y = my(*v);
            let c = c.gamma_multiply(0.7);
            let mut x = plot.left();
            while x < plot.right() {
                p.line_segment([pos2(x, y), pos2((x + 6.0).min(plot.right()), y)], Stroke::new(1.0, c));
                x += 10.0;
            }
            p.text(pos2(plot.right() - 2.0, y - 2.0), Align2::RIGHT_BOTTOM, *label, FontId::proportional(10.5), c);
        }
    }
    let max_pts = (plot.width() * 1.5) as usize;
    for s in series {
        if s.pts.is_empty() {
            continue;
        }
        let stride = (s.pts.len() / max_pts.max(1)).max(1);
        let mut pts: Vec<Pos2> = s.pts.iter().step_by(stride).map(|(x, y)| pos2(mx(*x), my(*y))).collect();
        if let Some(last) = s.pts.last() {
            let lp = pos2(mx(last.0), my(last.1));
            if pts.last() != Some(&lp) {
                pts.push(lp);
            }
        }
        if pts.len() == 1 {
            p.circle_filled(pts[0], 3.0, s.color);
        } else {
            p.add(Shape::line(pts, Stroke::new(1.8, s.color)));
        }
    }
    if let Some(h) = resp.hover_pos().filter(|h| plot.contains(*h)) {
        let tx = x0 + ((h.x - plot.left()) / plot.width()) as f64 * (x1 - x0);
        let mut lines = vec![fmt_time(tx, 0.0)];
        if let Some(dt) = Local.timestamp_opt(tx as i64, 0).single() {
            lines[0] = dt.format("%Y-%m-%d %H:%M").to_string();
        }
        for s in series {
            if s.pts.is_empty() {
                continue;
            }
            let i = s.pts.partition_point(|(x, _)| *x < tx);
            let cand = [i.saturating_sub(1), i.min(s.pts.len() - 1)];
            let best = cand.into_iter().min_by(|a, b| {
                (s.pts[*a].0 - tx).abs().partial_cmp(&(s.pts[*b].0 - tx).abs()).unwrap_or(std::cmp::Ordering::Equal)
            });
            if let Some(bi) = best {
                let (px, py) = s.pts[bi];
                p.circle_filled(pos2(mx(px), my(py)), 3.5, s.color);
                lines.push(format!("{}: {}", s.name, fmt(py)));
            }
        }
        p.line_segment([pos2(h.x, plot.top()), pos2(h.x, plot.bottom())], Stroke::new(0.8, weak));
        let galley = p.layout_no_wrap(lines.join("\n"), FontId::proportional(12.0), vis.text_color());
        let size = galley.size() + vec2(12.0, 8.0);
        let mut pos = h + vec2(12.0, -size.y - 6.0);
        if pos.x + size.x > rect.right() {
            pos.x = h.x - size.x - 12.0;
        }
        pos.y = pos.y.max(rect.top() + 2.0);
        let bg = Rect::from_min_size(pos, size);
        p.rect_filled(bg, 4.0, vis.window_fill);
        p.galley(pos + vec2(6.0, 4.0), galley, vis.text_color());
    }
}

/// Procedurally drawn shield icon (64x64 RGBA) for the window.
pub fn app_icon() -> egui::IconData {
    let n = 64usize;
    let mut rgba = vec![0u8; n * n * 4];
    for y in 0..n {
        for x in 0..n {
            let fx = (x as f32 + 0.5) / n as f32 * 2.0 - 1.0;
            let fy = (y as f32 + 0.5) / n as f32;
            // shield: flat top, sides curve to a point at the bottom
            let half_w = if fy < 0.55 { 0.82 } else { 0.82 * (1.0 - ((fy - 0.55) / 0.42).powf(1.6)).max(0.0) };
            let inside = fy > 0.06 && fy < 0.97 && fx.abs() < half_w;
            if !inside {
                continue;
            }
            let i = (y * n + x) * 4;
            let edge = half_w - fx.abs() < 0.09 || fy < 0.12;
            let (r, g, b) = if edge { (0x1B, 0x7A, 0x3E) } else { (0x2E, 0xB8, 0x5C) };
            // white check mark
            let cx = fx;
            let cy = fy;
            let on_check = {
                let d1 = ((cy - 0.55) - (cx + 0.32) * 0.9).abs() < 0.07 && cx > -0.38 && cx < -0.08;
                let d2 = ((cy - 0.73) + (cx + 0.08) * 1.25).abs() < 0.075 && (-0.12..0.42).contains(&cx);
                d1 || d2
            };
            let (r, g, b) = if on_check && !edge { (255, 255, 255) } else { (r, g, b) };
            rgba[i] = r;
            rgba[i + 1] = g;
            rgba[i + 2] = b;
            rgba[i + 3] = 255;
        }
    }
    egui::IconData {
        rgba,
        width: n as u32,
        height: n as u32,
    }
}
