//! Small drawing helpers shared by the screens.

use egui::epaint::{Mesh, Vertex, WHITE_UV};
use egui::{
    Align2, Color32, CornerRadius, FontId, Pos2, Rect, RichText, Sense, Shape, Stroke, StrokeKind,
    TextStyle, Ui, pos2, vec2,
};

use crate::theme::Palette;

/// Two-color diagonal gradients for the showcase, picked per instance.
const GRADIENTS: [([u8; 3], [u8; 3]); 4] = [
    ([0x2E, 0x1A, 0x6B], [0x0E, 0x0B, 0x24]),
    ([0xD9, 0x60, 0x3F], [0x3A, 0x1B, 0x6B]),
    ([0x2A, 0x4B, 0x42], [0x0B, 0x16, 0x20]),
    ([0xA8, 0x2E, 0x8A], [0x1B, 0x12, 0x40]),
];

/// The gradient colors for an instance id; stable across runs.
pub fn gradient_for(key: &str) -> (Color32, Color32) {
    let sum = key.bytes().fold(0usize, |acc, b| {
        acc.wrapping_mul(31).wrapping_add(usize::from(b))
    });
    let (a, b) = GRADIENTS[sum % GRADIENTS.len()];
    (
        Color32::from_rgb(a[0], a[1], a[2]),
        Color32::from_rgb(b[0], b[1], b[2]),
    )
}

fn lerp_color(a: Color32, b: Color32, t: f32) -> Color32 {
    let t = t.clamp(0.0, 1.0);
    let mix = |x: u8, y: u8| {
        let x = f32::from(x);
        let y = f32::from(y);
        (x + (y - x) * t).round() as u8
    };
    Color32::from_rgb(mix(a.r(), b.r()), mix(a.g(), b.g()), mix(a.b(), b.b()))
}

/// Points of a rounded rectangle outline, clockwise from the top-left arc.
fn rounded_outline(rect: Rect, radius: f32) -> Vec<Pos2> {
    const STEPS: usize = 10;
    let r = radius.min(rect.width() / 2.0).min(rect.height() / 2.0);
    let corners = [
        (pos2(rect.left() + r, rect.top() + r), 180.0_f32),
        (pos2(rect.right() - r, rect.top() + r), 270.0),
        (pos2(rect.right() - r, rect.bottom() - r), 0.0),
        (pos2(rect.left() + r, rect.bottom() - r), 90.0),
    ];
    let mut points = Vec::with_capacity(4 * (STEPS + 1));
    for (center, start) in corners {
        for step in 0..=STEPS {
            let angle = (start + 90.0 * step as f32 / STEPS as f32).to_radians();
            points.push(center + r * vec2(angle.cos(), angle.sin()));
        }
    }
    points
}

/// Fills a rounded rectangle with a diagonal gradient from `from` (top-left) to `to`
/// (bottom-right). The gradient is affine, so a triangle fan reproduces it exactly.
pub fn gradient_rect(ui: &Ui, rect: Rect, radius: u8, from: Color32, to: Color32) {
    let outline = rounded_outline(rect, f32::from(radius));
    let axis = vec2(rect.width(), rect.height());
    let along = |p: Pos2| {
        let d = p - rect.min;
        ((d.x * axis.x + d.y * axis.y) / axis.length_sq()).clamp(0.0, 1.0)
    };
    let mut mesh = Mesh::default();
    let color_at = |p: Pos2| lerp_color(from, to, along(p));
    let center = rect.center();
    mesh.vertices.push(Vertex {
        pos: center,
        uv: WHITE_UV,
        color: color_at(center),
    });
    for p in &outline {
        mesh.vertices.push(Vertex {
            pos: *p,
            uv: WHITE_UV,
            color: color_at(*p),
        });
    }
    let n = outline.len() as u32;
    for i in 0..n {
        mesh.add_triangle(0, 1 + i, 1 + (i + 1) % n);
    }
    ui.painter().add(Shape::mesh(mesh));
}

/// A small rounded label on a translucent plate (version, loader, status).
pub fn chip(ui: &mut Ui, text: &str, fill: Color32, color: Color32) {
    let font = TextStyle::Small.resolve(ui.style());
    let galley = ui.painter().layout_no_wrap(text.to_owned(), font, color);
    let size = galley.size() + vec2(20.0, 8.0);
    let (rect, response) = ui.allocate_exact_size(size, Sense::hover());
    ui.painter().rect_filled(rect, CornerRadius::same(12), fill);
    ui.painter()
        .galley(rect.center() - galley.size() / 2.0, galley, color);
    response.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Label, true, text));
}

/// The main call-to-action button.
pub fn primary_button(ui: &mut Ui, p: &Palette, label: &str) -> egui::Response {
    let text = RichText::new(label).color(p.on_accent).strong().size(16.0);
    ui.add(
        egui::Button::new(text)
            .fill(p.accent_solid)
            .stroke(Stroke::NONE)
            .corner_radius(CornerRadius::same(14))
            .min_size(vec2(140.0, 44.0)),
    )
}

/// A quiet button on a translucent plate, readable on gradients.
pub fn glass_button(ui: &mut Ui, label: &str) -> egui::Response {
    let text = RichText::new(label)
        .color(Color32::from_rgb(0xF3, 0xF0, 0xFA))
        .size(15.0);
    ui.add(
        egui::Button::new(text)
            .fill(Color32::from_white_alpha(28))
            .stroke(Stroke::new(1.0, Color32::from_white_alpha(60)))
            .corner_radius(CornerRadius::same(14))
            .min_size(vec2(120.0, 44.0)),
    )
}

/// Paints a plain card plate and returns its rect.
pub fn card_plate(ui: &Ui, rect: Rect, p: &Palette, hovered: bool) {
    let fill = if hovered {
        p.surface_hover
    } else {
        p.surface_raised
    };
    ui.painter().rect_filled(rect, CornerRadius::same(16), fill);
    ui.painter().rect_stroke(
        rect,
        CornerRadius::same(16),
        Stroke::new(1.0, p.border),
        StrokeKind::Inside,
    );
}

/// Draws text at an anchor with the given size and color.
pub fn text_at(ui: &Ui, pos: Pos2, align: Align2, text: &str, size: f32, color: Color32) -> Rect {
    ui.painter()
        .text(pos, align, text, FontId::proportional(size), color)
}

/// An empty-state message centered in the remaining space.
pub fn empty_state(ui: &mut Ui, p: &Palette, title: &str, hint: &str) {
    ui.add_space(48.0);
    ui.vertical_centered(|ui| {
        ui.label(RichText::new(title).size(18.0).color(p.text));
        ui.label(RichText::new(hint).color(p.text_muted));
    });
}
