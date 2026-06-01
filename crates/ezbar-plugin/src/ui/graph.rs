//! Canvas line-graphs for cpu/memory/temperature/ping.
//! Port of pkg/widget/graph.go, rendered with iced's GPU canvas (anti-aliased).

use iced::advanced::text::Alignment as TextAlign;
use iced::alignment::Vertical as VAlign;
use iced::font::Weight;
use iced::widget::canvas::{
    self, gradient, Fill, Frame, Geometry, LineCap, LineDash, LineJoin, Path, Stroke, Style, Text,
};
use iced::{mouse, Color, Font, Point, Rectangle, Renderer, Size, Theme};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GraphKind {
    Cpu,
    Memory,
    Temperature,
    Ping,
}

/// Canvas program holding a snapshot of the history values to plot.
pub struct Graph {
    pub values: Vec<f64>,
    pub kind: GraphKind,
}

fn cpu_color(v: f64) -> Color {
    if v <= 25.0 {
        Color::from_rgb(0.2, 0.8, 0.2)
    } else if v <= 50.0 {
        Color::from_rgb(1.0, 1.0, 0.0)
    } else if v <= 75.0 {
        Color::from_rgb(1.0, 0.6, 0.0)
    } else {
        Color::from_rgb(1.0, 0.2, 0.2)
    }
}

fn memory_color(v: f64) -> Color {
    if v <= 50.0 {
        Color::from_rgb(0.2, 0.8, 0.2)
    } else if v <= 70.0 {
        Color::from_rgb(1.0, 1.0, 0.0)
    } else if v <= 85.0 {
        Color::from_rgb(1.0, 0.6, 0.0)
    } else {
        Color::from_rgb(1.0, 0.2, 0.2)
    }
}

fn temperature_color(v: f64) -> Color {
    if v <= 50.0 {
        Color::from_rgb(0.2, 0.8, 0.2)
    } else if v <= 60.0 {
        Color::from_rgb(1.0, 1.0, 0.0)
    } else if v <= 70.0 {
        Color::from_rgb(1.0, 0.6, 0.0)
    } else {
        Color::from_rgb(1.0, 0.2, 0.2)
    }
}

fn ping_color(v: f64) -> Color {
    if v <= 20.0 {
        Color::from_rgb(0.2, 0.8, 0.2)
    } else if v <= 50.0 {
        Color::from_rgb(1.0, 1.0, 0.0)
    } else if v <= 100.0 {
        Color::from_rgb(1.0, 0.6, 0.0)
    } else {
        Color::from_rgb(1.0, 0.2, 0.2)
    }
}

/// Temperature graph y-range: skips unfilled (0) slots, and centers a flat/near-
/// flat reading (±5°) so a constant temperature still draws a visible line (the
/// old code returned nothing when max_t == min_t); otherwise pads the range 10%.
/// Returns None when there is no valid sample.
fn temp_range(temps: &[f64]) -> Option<(f64, f64)> {
    if temps.is_empty() {
        return None;
    }
    let mut min_t = temps[0];
    let mut max_t = temps[0];
    let mut valid = 0;
    for &t in temps {
        if t > 0.0 {
            if t < min_t || min_t == 0.0 {
                min_t = t;
            }
            if t > max_t {
                max_t = t;
            }
            valid += 1;
        }
    }
    if valid == 0 {
        return None;
    }
    let range = max_t - min_t;
    if range < 10.0 {
        let center = (min_t + max_t) / 2.0;
        Some((center - 5.0, center + 5.0))
    } else {
        let padding = range * 0.1;
        Some((min_t - padding, max_t + padding))
    }
}

fn stroke_segment(frame: &mut Frame, a: Point, b: Point, color: Color) {
    let path = Path::new(|p| {
        p.move_to(a);
        p.line_to(b);
    });
    frame.stroke(&path, Stroke::default().with_width(1.5).with_color(color));
}

/// Fill the area under a polyline (low alpha) so a sparkline reads as a filled
/// area chart — ezbar's graph-forward look.
fn fill_under(frame: &mut Frame, points: &[(f32, f32)], h: f32, color: Color) {
    if points.len() < 2 {
        return;
    }
    let fill = Color { a: 0.16, ..color };
    let path = Path::new(|p| {
        p.move_to(Point::new(points[0].0, h));
        for &(x, y) in points {
            p.line_to(Point::new(x, y));
        }
        p.line_to(Point::new(points[points.len() - 1].0, h));
        p.close();
    });
    frame.fill(&path, fill);
}

impl Graph {
    fn draw_fixed(
        &self,
        frame: &mut Frame,
        w: f32,
        h: f32,
        min: f64,
        max: f64,
        color: fn(f64) -> Color,
    ) {
        let values = &self.values;
        if values.is_empty() {
            return;
        }
        let n = values.len();
        let pts: Vec<(f32, f32, f64)> = values
            .iter()
            .enumerate()
            .filter(|(_, &v)| v >= 0.0)
            .map(|(i, &v)| {
                let x = i as f32 * w / (n as f32 - 1.0).max(1.0);
                let y = h - (((v - min) / (max - min)) as f32) * h;
                (x, y, v)
            })
            .collect();
        if pts.is_empty() {
            return;
        }
        let max_val = pts.iter().map(|p| p.2).fold(min, f64::max);
        let xy: Vec<(f32, f32)> = pts.iter().map(|p| (p.0, p.1)).collect();
        fill_under(frame, &xy, h, color(max_val));
        for seg in pts.windows(2) {
            let (px, py, pv) = seg[0];
            let (x, y, v) = seg[1];
            let c = color(if pv > v { pv } else { v });
            stroke_segment(frame, Point::new(px, py), Point::new(x, y), c);
        }
    }

    fn draw_temperature(&self, frame: &mut Frame, w: f32, h: f32) {
        let temps = &self.values;
        let (min_t, max_t) = match temp_range(temps) {
            Some(r) => r,
            None => return,
        };
        let n = temps.len();
        let pts: Vec<(f32, f32, f64)> = temps
            .iter()
            .enumerate()
            .filter(|(_, &t)| t > 0.0)
            .map(|(i, &t)| {
                let x = i as f32 * w / (n as f32 - 1.0).max(1.0);
                let y = h - (((t - min_t) / (max_t - min_t)) as f32) * h;
                (x, y, t)
            })
            .collect();
        if pts.is_empty() {
            return;
        }
        let max_t_val = pts.iter().map(|p| p.2).fold(min_t, f64::max);
        let xy: Vec<(f32, f32)> = pts.iter().map(|p| (p.0, p.1)).collect();
        fill_under(frame, &xy, h, temperature_color(max_t_val));
        for seg in pts.windows(2) {
            let (px, py, pt) = seg[0];
            let (x, y, t) = seg[1];
            let c = temperature_color(if pt > t { pt } else { t });
            stroke_segment(frame, Point::new(px, py), Point::new(x, y), c);
        }
    }

    fn draw_ping(&self, frame: &mut Frame, w: f32, h: f32) {
        let pings = &self.values;
        if pings.is_empty() {
            return;
        }
        let mut min_p = 0.0;
        let mut max_p = 0.0;
        let mut valid = 0;
        for &p in pings {
            if p >= 0.0 {
                if valid == 0 || p < min_p {
                    min_p = p;
                }
                if p > max_p {
                    max_p = p;
                }
                valid += 1;
            }
        }
        if valid == 0 {
            return;
        }
        if max_p < 20.0 {
            max_p = 20.0;
        } else if max_p < 50.0 {
            max_p = 50.0;
        } else if max_p < 100.0 {
            max_p = 100.0;
        } else {
            max_p *= 1.1;
        }

        let n = pings.len();
        let mut prev: Option<(f32, f32, f64)> = None;
        for (i, &p) in pings.iter().enumerate() {
            if p >= 0.0 {
                let x = i as f32 * w / (n as f32 - 1.0).max(1.0);
                let denom = if max_p > min_p { max_p - min_p } else { 1.0 };
                let y = h - (((p - min_p) / denom) as f32) * h;
                if let Some((px, py, pp)) = prev {
                    let seg = if pp > p { pp } else { p };
                    stroke_segment(frame, Point::new(px, py), Point::new(x, y), ping_color(seg));
                }
                prev = Some((x, y, p));
            } else {
                prev = None;
            }
        }
    }
}

impl<Message> canvas::Program<Message> for Graph {
    type State = ();

    fn draw(
        &self,
        _state: &(),
        renderer: &Renderer,
        _theme: &Theme,
        bounds: Rectangle,
        _cursor: mouse::Cursor,
    ) -> Vec<Geometry> {
        let mut frame = Frame::new(renderer, bounds.size());
        let w = bounds.width;
        let h = bounds.height;
        match self.kind {
            GraphKind::Cpu => self.draw_fixed(&mut frame, w, h, 0.0, 100.0, cpu_color),
            GraphKind::Memory => self.draw_fixed(&mut frame, w, h, 0.0, 100.0, memory_color),
            GraphKind::Temperature => self.draw_temperature(&mut frame, w, h),
            GraphKind::Ping => self.draw_ping(&mut frame, w, h),
        }
        vec![frame.into_geometry()]
    }
}

/// Filled-area trend chart for the stock hover popup: a smoothed (Catmull-Rom)
/// price curve with a vertical gradient fill, a soft glow, an opening-price
/// baseline and high/low/now markers. Draws over the themed popup background
/// (no opaque fill of its own) so it inherits the user's theme.
pub struct StockChart {
    pub values: Vec<f64>,
    pub symbol: String,
}

fn with_alpha(c: Color, a: f32) -> Color {
    Color { a, ..c }
}

/// The stock module's inline "icon": a tiny smoothed sparkline of the recent price
/// series with a vertical gradient fill and a round end-cap dot, in the trend colour
/// (green up / red down). It's the bar's GPU-graph identity shrunk to glyph size —
/// crisp at any scale and fully themeable, unlike a bitmap emoji. Draws on a
/// transparent surface; the caller sizes it (~24×15).
pub struct MiniTrend {
    pub values: Vec<f64>,
    pub color: Color,
    /// the surface colour behind the icon (the island/bar bg) — used to punch a
    /// hair of gap around the end-cap dot so it reads as a distinct marker.
    pub bg: Color,
}

impl<Message> canvas::Program<Message> for MiniTrend {
    type State = ();

    fn draw(
        &self,
        _state: &(),
        renderer: &Renderer,
        _theme: &Theme,
        bounds: Rectangle,
        _cursor: mouse::Cursor,
    ) -> Vec<Geometry> {
        let mut frame = Frame::new(renderer, bounds.size());
        let n = self.values.len();
        if n < 2 {
            return vec![frame.into_geometry()];
        }
        // Inset so the 1.6px stroke and the end dot stay inside the bounds.
        let pad = 2.0_f32;
        let (x0, x1) = (pad, (bounds.width - pad).max(pad + 1.0));
        let (y_top, y_bot) = (pad, (bounds.height - pad).max(pad + 1.0));
        let (cw, ch) = (x1 - x0, y_bot - y_top);

        let (mut lo, mut hi) = (f64::MAX, f64::MIN);
        for &v in &self.values {
            lo = lo.min(v);
            hi = hi.max(v);
        }
        if (hi - lo).abs() < f64::EPSILON {
            hi = lo + 1.0;
        }
        let range = hi - lo;
        lo -= range * 0.18;
        hi += range * 0.18;
        let y_of = |v: f64| y_top + ch - (((v - lo) / (hi - lo)) as f32) * ch;
        let x_of = |i: usize| x0 + (i as f32 / (n as f32 - 1.0)) * cw;
        let pts: Vec<Point> = self
            .values
            .iter()
            .enumerate()
            .map(|(i, &v)| Point::new(x_of(i), y_of(v)))
            .collect();
        let segs = smooth_controls(&pts, y_top, y_bot);

        // gradient area fill under the smoothed curve
        let area = Path::new(|b| {
            b.move_to(Point::new(pts[0].x, y_bot));
            b.line_to(pts[0]);
            for &(c1, c2, end) in &segs {
                b.bezier_curve_to(c1, c2, end);
            }
            b.line_to(Point::new(pts[n - 1].x, y_bot));
            b.close();
        });
        frame.fill(
            &area,
            Fill::from(
                gradient::Linear::new(Point::new(0.0, y_top), Point::new(0.0, y_bot))
                    .add_stop(0.0, with_alpha(self.color, 0.55))
                    .add_stop(0.6, with_alpha(self.color, 0.18))
                    .add_stop(1.0, with_alpha(self.color, 0.0)),
            ),
        );

        let line = Path::new(|b| {
            b.move_to(pts[0]);
            for &(c1, c2, end) in &segs {
                b.bezier_curve_to(c1, c2, end);
            }
        });
        // a very soft wide glow under a crisp uniform stroke — the GPU-glow identity
        // at glyph size, without muddying the line.
        frame.stroke(
            &line,
            Stroke {
                style: Style::Solid(with_alpha(self.color, 0.16)),
                width: 4.0,
                line_cap: LineCap::Round,
                line_join: LineJoin::Round,
                ..Stroke::default()
            },
        );
        frame.stroke(
            &line,
            Stroke {
                style: Style::Solid(self.color),
                width: 1.5,
                line_cap: LineCap::Round,
                line_join: LineJoin::Round,
                ..Stroke::default()
            },
        );
        // end-cap marker at the latest price: a wide *opaque* bg halo punches a clear
        // gap through the line + glow, and a brightened dot sits in it — so it reads
        // as a deliberate marker, not a fat line terminus.
        let last = pts[n - 1];
        let halo = Color { a: 1.0, ..self.bg };
        let dot = Color::from_rgb(
            self.color.r * 0.6 + 0.4,
            self.color.g * 0.6 + 0.4,
            self.color.b * 0.6 + 0.4,
        );
        frame.fill(&Path::circle(last, 3.8), halo);
        frame.fill(&Path::circle(last, 1.9), dot);
        vec![frame.into_geometry()]
    }
}

/// A canvas `Text` with sane defaults; `ax`/`ay` anchor it (e.g. right/center).
fn mk_text(content: String, pos: Point, color: Color, size: f32, weight: Weight) -> Text {
    Text {
        content,
        position: pos,
        color,
        size: size.into(),
        font: Font {
            weight,
            ..Font::default()
        },
        ..Default::default()
    }
}

/// Catmull-Rom → cubic-Bézier control points for a smooth curve through `pts`.
/// Control points are y-clamped to `[y_lo, y_hi]` so the spline never overshoots
/// out of the plot area. Returns one (control_a, control_b, end) triple per
/// segment, ready to feed to `bezier_curve_to`.
fn smooth_controls(pts: &[Point], y_lo: f32, y_hi: f32) -> Vec<(Point, Point, Point)> {
    let n = pts.len();
    let clamp = |p: Point| Point::new(p.x, p.y.clamp(y_lo, y_hi));
    (0..n.saturating_sub(1))
        .map(|i| {
            let p0 = pts[i.saturating_sub(1)];
            let p1 = pts[i];
            let p2 = pts[i + 1];
            let p3 = pts[(i + 2).min(n - 1)];
            let c1 = Point::new(p1.x + (p2.x - p0.x) / 6.0, p1.y + (p2.y - p0.y) / 6.0);
            let c2 = Point::new(p2.x - (p3.x - p1.x) / 6.0, p2.y - (p3.y - p1.y) / 6.0);
            (clamp(c1), clamp(c2), p2)
        })
        .collect()
}

impl<Message> canvas::Program<Message> for StockChart {
    type State = ();

    fn draw(
        &self,
        _state: &(),
        renderer: &Renderer,
        _theme: &Theme,
        bounds: Rectangle,
        _cursor: mouse::Cursor,
    ) -> Vec<Geometry> {
        let mut frame = Frame::new(renderer, bounds.size());
        let (w, h) = (bounds.width, bounds.height);

        if self.values.len() < 2 {
            frame.fill_text(Text {
                align_x: TextAlign::Center,
                align_y: VAlign::Center,
                ..mk_text(
                    "Loading 7-day chart…".to_string(),
                    Point::new(w / 2.0, h / 2.0),
                    with_alpha(Color::WHITE, 0.6),
                    13.0,
                    Weight::Normal,
                )
            });
            return vec![frame.into_geometry()];
        }

        let first = self.values[0];
        let last = *self.values.last().unwrap();
        let up = last >= first;
        // Modern mint-green / coral palette; brighter variants for line + text.
        let trend = if up {
            Color::from_rgb(0.30, 0.86, 0.52)
        } else {
            Color::from_rgb(0.96, 0.36, 0.42)
        };
        let trend_bright = if up {
            Color::from_rgb(0.45, 0.95, 0.62)
        } else {
            Color::from_rgb(1.0, 0.50, 0.55)
        };
        let muted = with_alpha(Color::WHITE, 0.42);

        // --- header -------------------------------------------------------
        let (pad_l, pad_r) = (4.0_f32, 4.0_f32);
        // ticker + price share a baseline (bottom-anchored), so the larger price
        // grows upward instead of dropping below the ticker
        let head_base = 23.0_f32;
        // second header row (caption + pill) shares this vertical center
        let row_y = 38.0_f32;
        frame.fill_text(Text {
            align_y: VAlign::Bottom,
            ..mk_text(
                self.symbol.clone(),
                Point::new(pad_l, head_base),
                Color::WHITE,
                20.0,
                Weight::Semibold,
            )
        });
        frame.fill_text(Text {
            align_y: VAlign::Center,
            ..mk_text(
                "7-day trend".to_string(),
                Point::new(pad_l, row_y),
                muted,
                10.5,
                Weight::Normal,
            )
        });
        // current price, right-aligned, slightly larger for emphasis
        frame.fill_text(Text {
            align_x: TextAlign::Right,
            align_y: VAlign::Bottom,
            ..mk_text(
                format!("${:.2}", last),
                Point::new(w - pad_r, head_base),
                Color::WHITE,
                21.0,
                Weight::Semibold,
            )
        });
        // percent-change pill, right-aligned under the price
        let pct = if first != 0.0 {
            (last - first) / first * 100.0
        } else {
            0.0
        };
        let badge = format!("{} {:.2}%", if pct >= 0.0 { "▲" } else { "▼" }, pct.abs());
        let badge_size = 11.5_f32;
        let badge_h = 19.0_f32;
        let badge_w = badge.chars().count() as f32 * badge_size * 0.56 + 14.0;
        // centered on `row_y` so the pill and the caption share one baseline
        frame.fill(
            &Path::rounded_rectangle(
                Point::new(w - pad_r - badge_w, row_y - badge_h / 2.0),
                Size::new(badge_w, badge_h),
                (badge_h / 2.0).into(),
            ),
            with_alpha(trend, 0.16),
        );
        frame.fill_text(Text {
            align_x: TextAlign::Center,
            align_y: VAlign::Center,
            ..mk_text(
                badge,
                Point::new(w - pad_r - badge_w / 2.0, row_y),
                trend_bright,
                badge_size,
                Weight::Semibold,
            )
        });

        // --- plot geometry ------------------------------------------------
        // generous right inset so the "now" marker + glow float clear of the bezel
        let (m_l, m_r) = (10.0_f32, 20.0_f32);
        let (top, bottom) = (54.0_f32, 30.0_f32);
        let (x0, x1) = (m_l, w - m_r);
        let cw = x1 - x0;
        let (y_top, y_bot) = (top, h - bottom);
        let ch = y_bot - y_top;

        let (mut min_p, mut max_p) = (self.values[0], self.values[0]);
        for &p in &self.values {
            min_p = min_p.min(p);
            max_p = max_p.max(p);
        }
        if (max_p - min_p).abs() < f64::EPSILON {
            max_p = min_p + 1.0;
        }
        let range = max_p - min_p;
        min_p -= range * 0.12;
        max_p += range * 0.12;
        let y_of = |price: f64| y_top + ch - (((price - min_p) / (max_p - min_p)) as f32) * ch;

        let n = self.values.len();
        let x_of = |i: usize| x0 + (i as f32 / (n as f32 - 1.0).max(1.0)) * cw;
        let pts: Vec<Point> = self
            .values
            .iter()
            .enumerate()
            .map(|(i, &p)| Point::new(x_of(i), y_of(p)))
            .collect();
        let segs = smooth_controls(&pts, y_top, y_bot);

        // dashed baseline at the opening price — neutral so it reads as an axis,
        // not data; the curve sitting above/below it still shows gain/loss
        let base_y = y_of(first);
        let dash = [3.0_f32, 4.0];
        frame.stroke(
            &Path::line(Point::new(x0, base_y), Point::new(x1, base_y)),
            Stroke {
                style: Style::Solid(with_alpha(Color::WHITE, 0.16)),
                width: 1.0,
                line_dash: LineDash {
                    segments: &dash,
                    offset: 0,
                },
                ..Stroke::default()
            },
        );

        // gradient area fill under the smoothed curve
        let area = Path::new(|b| {
            b.move_to(Point::new(pts[0].x, y_bot));
            b.line_to(pts[0]);
            for &(c1, c2, end) in &segs {
                b.bezier_curve_to(c1, c2, end);
            }
            b.line_to(Point::new(pts[n - 1].x, y_bot));
            b.close();
        });
        frame.fill(
            &area,
            Fill::from(
                gradient::Linear::new(Point::new(0.0, y_top), Point::new(0.0, y_bot))
                    .add_stop(0.0, with_alpha(trend, 0.34))
                    .add_stop(0.55, with_alpha(trend, 0.10))
                    .add_stop(1.0, with_alpha(trend, 0.0)),
            ),
        );

        // the smoothed line: a soft wide glow under a crisp gradient stroke
        let line = Path::new(|b| {
            b.move_to(pts[0]);
            for &(c1, c2, end) in &segs {
                b.bezier_curve_to(c1, c2, end);
            }
        });
        frame.stroke(
            &line,
            Stroke {
                style: Style::Solid(with_alpha(trend, 0.10)),
                width: 4.5,
                line_cap: LineCap::Round,
                line_join: LineJoin::Round,
                ..Stroke::default()
            },
        );
        frame.stroke(
            &line,
            Stroke {
                style: Style::Gradient(
                    gradient::Linear::new(Point::new(x0, 0.0), Point::new(x1, 0.0))
                        .add_stop(0.0, with_alpha(trend, 0.65))
                        .add_stop(1.0, trend_bright)
                        .into(),
                ),
                width: 2.3,
                line_cap: LineCap::Round,
                line_join: LineJoin::Round,
                ..Stroke::default()
            },
        );

        // high / low markers with floating price labels
        let hi_i = self
            .values
            .iter()
            .enumerate()
            .max_by(|a, b| a.1.partial_cmp(b.1).unwrap())
            .map(|(i, _)| i)
            .unwrap_or(0);
        let lo_i = self
            .values
            .iter()
            .enumerate()
            .min_by(|a, b| a.1.partial_cmp(b.1).unwrap())
            .map(|(i, _)| i)
            .unwrap_or(0);
        let mut marker = |i: usize, above: bool| {
            let p = pts[i];
            frame.stroke(
                &Path::circle(p, 2.5),
                Stroke::default()
                    .with_width(1.2)
                    .with_color(with_alpha(Color::WHITE, 0.55)),
            );
            let lx = p.x.clamp(x0 + 22.0, x1 - 22.0);
            // keep the callout in the plot band; flip across the marker if its
            // natural side would run into the header or the footer row
            let ly = if above {
                (p.y - 14.0).max(y_top + 4.0)
            } else if p.y + 14.0 <= y_bot - 2.0 {
                p.y + 14.0
            } else {
                p.y - 14.0
            };
            frame.fill_text(Text {
                align_x: TextAlign::Center,
                align_y: VAlign::Center,
                ..mk_text(
                    format!("${:.2}", self.values[i]),
                    Point::new(lx, ly),
                    muted,
                    9.5,
                    Weight::Normal,
                )
            });
        };
        // the latest point is already flagged by the "now" marker + header price,
        // so skip a redundant high/low callout (and its glow collision) there
        if hi_i != n - 1 {
            marker(hi_i, true);
        }
        if lo_i != n - 1 {
            marker(lo_i, false);
        }

        // end-of-line "now" marker: restrained halo, ring, dot, sparkle core
        let end = pts[n - 1];
        frame.fill(&Path::circle(end, 4.5), with_alpha(trend, 0.10));
        frame.stroke(
            &Path::circle(end, 3.2),
            Stroke::default().with_width(1.3).with_color(trend_bright),
        );
        frame.fill(&Path::circle(end, 2.1), trend_bright);
        frame.fill(&Path::circle(end, 0.9), Color::WHITE);

        // x-axis endpoints — bottom-anchored so the gutter matches the sides
        frame.fill_text(Text {
            align_y: VAlign::Bottom,
            ..mk_text(
                "7 days ago".to_string(),
                Point::new(x0, h - 9.0),
                muted,
                9.5,
                Weight::Normal,
            )
        });
        frame.fill_text(Text {
            align_x: TextAlign::Right,
            align_y: VAlign::Bottom,
            ..mk_text(
                "now".to_string(),
                Point::new(x1, h - 9.0),
                muted,
                9.5,
                Weight::Normal,
            )
        });

        vec![frame.into_geometry()]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn temp_range_constant_reading_centers() {
        // regression: a constant temperature used to draw nothing (max_t == min_t)
        assert_eq!(temp_range(&[17.0, 17.0, 17.0]), Some((12.0, 22.0)));
    }

    #[test]
    fn temp_range_skips_zeros_and_pads() {
        let (lo, hi) = temp_range(&[0.0, 0.0, 40.0, 60.0]).unwrap();
        assert!(
            (lo - 38.0).abs() < 1e-9 && (hi - 62.0).abs() < 1e-9,
            "got {lo}..{hi}"
        );
    }

    #[test]
    fn temp_range_none_without_valid_samples() {
        assert_eq!(temp_range(&[0.0, 0.0]), None);
        assert_eq!(temp_range(&[]), None);
    }

    #[test]
    fn colors_by_threshold() {
        assert_eq!(cpu_color(10.0), Color::from_rgb(0.2, 0.8, 0.2));
        assert_eq!(cpu_color(40.0), Color::from_rgb(1.0, 1.0, 0.0));
        assert_eq!(cpu_color(60.0), Color::from_rgb(1.0, 0.6, 0.0));
        assert_eq!(cpu_color(90.0), Color::from_rgb(1.0, 0.2, 0.2));
        assert_eq!(ping_color(10.0), Color::from_rgb(0.2, 0.8, 0.2));
        assert_eq!(ping_color(150.0), Color::from_rgb(1.0, 0.2, 0.2));
    }
}
