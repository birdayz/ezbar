//! Canvas line-graphs for cpu/memory/temperature/ping.
//! Port of pkg/widget/graph.go, rendered with iced's GPU canvas (anti-aliased).

use iced::widget::canvas::{self, Frame, Geometry, Path, Stroke, Text};
use iced::{mouse, Color, Point, Rectangle, Renderer, Theme};

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
    frame.stroke(
        &path,
        Stroke::default().with_width(1.5).with_color(color),
    );
}

impl Graph {
    fn draw_fixed(&self, frame: &mut Frame, w: f32, h: f32, min: f64, max: f64, color: fn(f64) -> Color) {
        let values = &self.values;
        if values.is_empty() {
            return;
        }
        let n = values.len();
        let mut prev: Option<(f32, f32, f64)> = None;
        for (i, &val) in values.iter().enumerate() {
            if val < 0.0 {
                continue;
            }
            let x = i as f32 * w / (n as f32 - 1.0);
            let y = h - (((val - min) / (max - min)) as f32) * h;
            if let Some((px, py, pv)) = prev {
                let seg = if pv > val { pv } else { val };
                stroke_segment(frame, Point::new(px, py), Point::new(x, y), color(seg));
            }
            prev = Some((x, y, val));
        }
    }

    fn draw_temperature(&self, frame: &mut Frame, w: f32, h: f32) {
        let temps = &self.values;
        let (min_t, max_t) = match temp_range(temps) {
            Some(r) => r,
            None => return,
        };
        let n = temps.len();
        let mut prev: Option<(f32, f32, f64)> = None;
        for (i, &t) in temps.iter().enumerate() {
            if t > 0.0 {
                let x = i as f32 * w / (n as f32 - 1.0);
                let y = h - (((t - min_t) / (max_t - min_t)) as f32) * h;
                if let Some((px, py, pt)) = prev {
                    let seg = if pt > t { pt } else { t };
                    stroke_segment(frame, Point::new(px, py), Point::new(x, y), temperature_color(seg));
                }
                prev = Some((x, y, t));
            }
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
                let x = i as f32 * w / (n as f32 - 1.0);
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

/// Larger axes+grid line chart for the stock hover popup. Port of stock.go drawChart.
pub struct StockChart {
    pub values: Vec<f64>,
    pub symbol: String,
}

fn label<'a>(content: String, x: f32, y: f32, color: Color, size: f32) -> Text {
    Text {
        content,
        position: Point::new(x, y),
        color,
        size: size.into(),
        ..Default::default()
    }
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
        frame.fill(
            &Path::rectangle(Point::ORIGIN, frame.size()),
            Color::from_rgb(0.05, 0.05, 0.05),
        );

        if self.values.len() < 2 {
            frame.fill_text(label(
                "Loading 7d chart…".to_string(),
                w / 2.0 - 70.0,
                h / 2.0,
                Color::WHITE,
                14.0,
            ));
            return vec![frame.into_geometry()];
        }

        let (left, right, top, bottom) = (52.0_f32, 14.0_f32, 26.0_f32, 22.0_f32);
        let cw = w - left - right;
        let ch = h - top - bottom;

        let mut min_p = self.values[0];
        let mut max_p = self.values[0];
        for &p in &self.values {
            if p < min_p {
                min_p = p;
            }
            if p > max_p {
                max_p = p;
            }
        }
        if max_p == min_p {
            max_p = min_p + 1.0;
        }
        let range = max_p - min_p;
        min_p -= range * 0.05;
        max_p += range * 0.05;

        // horizontal gridlines + y-axis price labels
        let ticks = 5;
        for i in 0..=ticks {
            let ratio = i as f32 / ticks as f32;
            let price = min_p + (max_p - min_p) * ratio as f64;
            let y = top + ch - ratio * ch;
            frame.stroke(
                &Path::new(|p| {
                    p.move_to(Point::new(left, y));
                    p.line_to(Point::new(left + cw, y));
                }),
                Stroke::default()
                    .with_width(0.5)
                    .with_color(Color::from_rgb(0.15, 0.15, 0.15)),
            );
            frame.fill_text(label(
                format!("${:.0}", price),
                4.0,
                y - 6.0,
                Color::from_rgb(0.7, 0.7, 0.7),
                10.0,
            ));
        }

        // trend line
        let first = self.values[0];
        let last = *self.values.last().unwrap();
        let color = if last >= first {
            Color::from_rgb(0.2, 0.8, 0.2)
        } else {
            Color::from_rgb(0.8, 0.2, 0.2)
        };
        let n = self.values.len();
        let line = Path::new(|pb| {
            for (i, &price) in self.values.iter().enumerate() {
                let x = left + (i as f32 / (n as f32 - 1.0)) * cw;
                let y = top + ch - (((price - min_p) / (max_p - min_p)) as f32) * ch;
                if i == 0 {
                    pb.move_to(Point::new(x, y));
                } else {
                    pb.line_to(Point::new(x, y));
                }
            }
        });
        frame.stroke(&line, Stroke::default().with_width(2.0).with_color(color));

        // title + percent change
        frame.fill_text(label(
            format!("{} — 7 Day", self.symbol),
            left,
            6.0,
            Color::WHITE,
            13.0,
        ));
        let pct = (last - first) / first * 100.0;
        let (arrow, pcol) = if pct >= 0.0 {
            ("▲", Color::from_rgb(0.2, 0.8, 0.2))
        } else {
            ("▼", Color::from_rgb(0.8, 0.2, 0.2))
        };
        frame.fill_text(label(
            format!("{} {:.2}%", arrow, pct.abs()),
            left + cw - 72.0,
            6.0,
            pcol,
            12.0,
        ));

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
        assert!((lo - 38.0).abs() < 1e-9 && (hi - 62.0).abs() < 1e-9, "got {lo}..{hi}");
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
