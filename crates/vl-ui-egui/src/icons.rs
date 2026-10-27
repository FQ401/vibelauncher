//! Rail icons drawn from simple shapes (an icon font replaces them later).

use egui::{Color32, CornerRadius, Painter, Pos2, Rect, Shape, Stroke, StrokeKind, pos2, vec2};

use crate::nav::Section;

/// Draws the icon of `section` inside `rect`.
pub fn paint_section(painter: &Painter, rect: Rect, section: Section, color: Color32) {
    let c = rect.center();
    let stroke = Stroke::new(1.8, color);
    match section {
        Section::Home => {
            let roof = vec![
                pos2(c.x - 10.0, c.y - 1.0),
                pos2(c.x, c.y - 10.0),
                pos2(c.x + 10.0, c.y - 1.0),
            ];
            painter.add(Shape::line(roof, stroke));
            let body = Rect::from_min_max(pos2(c.x - 7.0, c.y - 2.0), pos2(c.x + 7.0, c.y + 9.0));
            painter.rect_stroke(body, CornerRadius::same(2), stroke, StrokeKind::Middle);
        }
        Section::Instances => {
            for (dx, dy) in [(-1.0, -1.0), (1.0, -1.0), (-1.0, 1.0), (1.0, 1.0)] {
                let cell = Rect::from_center_size(c + vec2(dx * 5.5, dy * 5.5), vec2(8.0, 8.0));
                painter.rect_stroke(cell, CornerRadius::same(2), stroke, StrokeKind::Middle);
            }
        }
        Section::Catalog => {
            let lens = c - vec2(2.0, 2.0);
            painter.circle_stroke(lens, 7.0, stroke);
            painter.line_segment([lens + vec2(5.0, 5.0), c + vec2(9.0, 9.0)], stroke);
        }
        Section::Account => {
            painter.circle_stroke(c - vec2(0.0, 4.5), 4.5, stroke);
            let shoulders = arc(c + vec2(0.0, 11.0), 9.5, 200.0, 340.0);
            painter.add(Shape::line(shoulders, stroke));
        }
        Section::Settings => {
            for (i, knob) in [-0.3_f32, 0.35, -0.1].into_iter().enumerate() {
                let y = c.y - 7.0 + 7.0 * i as f32;
                painter.line_segment([pos2(c.x - 10.0, y), pos2(c.x + 10.0, y)], stroke);
                painter.circle_filled(pos2(c.x + 10.0 * knob * 2.0, y), 3.0, color);
            }
        }
    }
}

/// The tasks indicator: a ring that is partly filled with `fraction`.
pub fn paint_ring(
    painter: &Painter,
    center: Pos2,
    fraction: Option<f32>,
    color: Color32,
    track: Color32,
) {
    painter.circle_stroke(center, 10.0, Stroke::new(2.5, track));
    if let Some(fraction) = fraction {
        let end = -90.0 + 360.0 * fraction.clamp(0.02, 1.0);
        painter.add(Shape::line(
            arc(center, 10.0, -90.0, end),
            Stroke::new(2.5, color),
        ));
    }
}

/// Sun (light theme active) or moon-like filled disc (dark theme active).
pub fn paint_theme(painter: &Painter, center: Pos2, dark: bool, color: Color32, bg: Color32) {
    if dark {
        painter.circle_filled(center, 8.0, color);
        painter.circle_filled(center + vec2(4.0, -3.0), 7.0, bg);
    } else {
        painter.circle_stroke(center, 5.0, Stroke::new(1.8, color));
        for i in 0..8 {
            let a = (i as f32 * 45.0).to_radians();
            let dir = vec2(a.cos(), a.sin());
            painter.line_segment(
                [center + dir * 8.0, center + dir * 11.0],
                Stroke::new(1.8, color),
            );
        }
    }
}

fn arc(center: Pos2, radius: f32, from_deg: f32, to_deg: f32) -> Vec<Pos2> {
    const STEPS: usize = 24;
    (0..=STEPS)
        .map(|i| {
            let a = (from_deg + (to_deg - from_deg) * i as f32 / STEPS as f32).to_radians();
            center + radius * vec2(a.cos(), a.sin())
        })
        .collect()
}
