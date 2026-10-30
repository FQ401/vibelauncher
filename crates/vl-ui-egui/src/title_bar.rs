//! The launcher's own title bar and window frame: the geometry of its zones (pure, tested) and
//! the window buttons, dragging and resizing on top of egui viewport commands.
//!
//! The bar is the top row of the interface, [`BAR_HEIGHT`] tall. With [`WindowChrome::Custom`]
//! the window has no system frame: an empty part of the bar moves the window
//! ([`ViewportCommand::StartDrag`]) and a double click maximizes or restores it, three buttons
//! [`BUTTON_WIDTH`] wide on the right minimize, maximize and close it, and an invisible border
//! [`RESIZE_BORDER`] wide along the edges resizes it ([`ViewportCommand::BeginResize`]), with
//! longer grips at the corners. With [`WindowChrome::Transparent`] (macOS) the native buttons sit
//! over the bar's left end, which the bar keeps free ([`MAC_BUTTONS_INSET`]).

use egui::viewport::ResizeDirection;
use egui::{
    Color32, Context, CornerRadius, CursorIcon, Painter, Pos2, Rect, Response, Sense, Stroke,
    StrokeKind, Ui, Vec2, ViewportCommand, pos2, vec2,
};

use crate::window::WindowChrome;

/// The height of the title bar.
pub const BAR_HEIGHT: f32 = 60.0;
/// The width of each window button.
pub const BUTTON_WIDTH: f32 = 46.0;
/// The width of the invisible resize border along the window edges.
pub const RESIZE_BORDER: f32 = 5.0;
/// How far from a corner, along each edge, the corner's diagonal resize reaches.
pub const CORNER_GRIP: f32 = 16.0;
/// The space the native window buttons of macOS take at the left end of the bar.
pub const MAC_BUTTONS_INSET: f32 = 80.0;
/// The background of the close button under the pointer.
const CLOSE_HOVER: Color32 = Color32::from_rgb(0xC4, 0x2B, 0x1C);
/// The background of the close button while pressed.
const CLOSE_PRESSED: Color32 = Color32::from_rgb(0x9E, 0x24, 0x18);

/// A window button of the title bar.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WindowButton {
    Minimize,
    /// Maximize, or restore a maximized window.
    Maximize,
    Close,
}

impl WindowButton {
    /// The buttons from left to right.
    pub const ALL: [WindowButton; 3] = [
        WindowButton::Minimize,
        WindowButton::Maximize,
        WindowButton::Close,
    ];
}

/// What a point of the window does under the pointer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Zone {
    /// The resize border: resizes the window in this direction.
    Resize(ResizeDirection),
    /// A window button.
    Button(WindowButton),
    /// The title bar: moves the window where no control of the bar is.
    TitleBar,
    /// The rest of the window: the interface.
    Client,
}

/// The state of the window that decides its zones.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Frame {
    /// The window draws its own buttons and resize border (no system frame).
    pub custom: bool,
    /// The window is maximized or fullscreen: it has no resize border.
    pub maximized: bool,
}

impl Frame {
    /// Whether the window has the resize border.
    fn resizable(self) -> bool {
        self.custom && !self.maximized
    }
}

/// The zone of `pos` in a window of `size` (both in points, from the window's top-left corner).
/// The resize border comes first, so the top pixels of the window buttons resize the window;
/// a maximized window has no border.
pub fn hit_test(size: Vec2, pos: Pos2, frame: Frame) -> Zone {
    let inside = pos.x >= 0.0 && pos.y >= 0.0 && pos.x < size.x && pos.y < size.y;
    if !inside {
        return Zone::Client;
    }
    if frame.resizable()
        && let Some(direction) = resize_direction(size, pos)
    {
        return Zone::Resize(direction);
    }
    if pos.y >= BAR_HEIGHT {
        return Zone::Client;
    }
    if frame.custom {
        for button in WindowButton::ALL {
            if button_rect(size, button).contains(pos) {
                return Zone::Button(button);
            }
        }
    }
    Zone::TitleBar
}

/// The resize direction of a point on the border, if it is on the border.
fn resize_direction(size: Vec2, pos: Pos2) -> Option<ResizeDirection> {
    let left = pos.x < RESIZE_BORDER;
    let right = pos.x >= size.x - RESIZE_BORDER;
    let top = pos.y < RESIZE_BORDER;
    let bottom = pos.y >= size.y - RESIZE_BORDER;
    if !(left || right || top || bottom) {
        return None;
    }
    let near_left = pos.x < CORNER_GRIP;
    let near_right = pos.x >= size.x - CORNER_GRIP;
    let near_top = pos.y < CORNER_GRIP;
    let near_bottom = pos.y >= size.y - CORNER_GRIP;
    let west = left || ((top || bottom) && near_left);
    let east = !west && (right || ((top || bottom) && near_right));
    let north = top || ((left || right) && near_top);
    let south = !north && (bottom || ((left || right) && near_bottom));
    Some(match (north, south, west, east) {
        (true, _, true, _) => ResizeDirection::NorthWest,
        (true, _, _, true) => ResizeDirection::NorthEast,
        (_, true, true, _) => ResizeDirection::SouthWest,
        (_, true, _, true) => ResizeDirection::SouthEast,
        (true, _, _, _) => ResizeDirection::North,
        (_, true, _, _) => ResizeDirection::South,
        (_, _, true, _) => ResizeDirection::West,
        _ => ResizeDirection::East,
    })
}

/// The rectangle of a window button in a window of `size`.
pub fn button_rect(size: Vec2, button: WindowButton) -> Rect {
    let from_right = match button {
        WindowButton::Close => 1.0,
        WindowButton::Maximize => 2.0,
        WindowButton::Minimize => 3.0,
    };
    let left = size.x - from_right * BUTTON_WIDTH;
    Rect::from_min_size(pos2(left, 0.0), vec2(BUTTON_WIDTH, BAR_HEIGHT))
}

/// The total width of the window buttons the bar draws for `chrome`.
pub fn buttons_width(chrome: WindowChrome) -> f32 {
    match chrome {
        WindowChrome::Custom => BUTTON_WIDTH * WindowButton::ALL.len() as f32,
        WindowChrome::System | WindowChrome::Transparent => 0.0,
    }
}

/// The cursor that shows a resize direction.
fn resize_cursor(direction: ResizeDirection) -> CursorIcon {
    match direction {
        ResizeDirection::North => CursorIcon::ResizeNorth,
        ResizeDirection::South => CursorIcon::ResizeSouth,
        ResizeDirection::East => CursorIcon::ResizeEast,
        ResizeDirection::West => CursorIcon::ResizeWest,
        ResizeDirection::NorthEast => CursorIcon::ResizeNorthEast,
        ResizeDirection::SouthEast => CursorIcon::ResizeSouthEast,
        ResizeDirection::NorthWest => CursorIcon::ResizeNorthWest,
        ResizeDirection::SouthWest => CursorIcon::ResizeSouthWest,
    }
}

/// The window's zone state this frame.
pub fn frame_of(ctx: &Context, chrome: WindowChrome) -> Frame {
    let (maximized, fullscreen) = ctx.input(|i| {
        let v = i.viewport();
        (v.maximized.unwrap_or(false), v.fullscreen.unwrap_or(false))
    });
    Frame {
        custom: chrome == WindowChrome::Custom,
        maximized: maximized || fullscreen,
    }
}

/// Whether the window is maximized.
pub fn is_maximized(ctx: &Context) -> bool {
    ctx.input(|i| i.viewport().maximized.unwrap_or(false))
}

/// Moves the window when the bar background (`response`) is dragged, and maximizes or restores
/// it on a double click. Controls on the bar take their own clicks.
pub fn handle_bar(ctx: &Context, response: &Response, frame: Frame) {
    // Where the press began; a double click is reported after the release, at the pointer.
    let origin = ctx
        .input(|i| i.pointer.press_origin())
        .or_else(|| response.interact_pointer_pos());
    let size = ctx.content_rect().size();
    let on_bar = origin.is_some_and(|p| hit_test(size, p, frame) == Zone::TitleBar);
    if response.double_clicked() && on_bar {
        ctx.send_viewport_cmd(ViewportCommand::Maximized(!is_maximized(ctx)));
    } else if response.drag_started_by(egui::PointerButton::Primary) && on_bar {
        ctx.send_viewport_cmd(ViewportCommand::StartDrag);
    }
}

/// The resize border of a window without the system frame: the cursor over it, and a press on
/// it starts resizing.
pub fn handle_resize(ctx: &Context, frame: Frame) {
    if !frame.resizable() {
        return;
    }
    let size = ctx.content_rect().size();
    let Some(hover) = ctx.pointer_hover_pos() else {
        return;
    };
    let Zone::Resize(direction) = hit_test(size, hover, frame) else {
        return;
    };
    ctx.set_cursor_icon(resize_cursor(direction));
    let pressed = ctx.input(|i| {
        i.pointer.primary_pressed()
            && i.pointer
                .press_origin()
                .is_some_and(|p| hit_test(size, p, frame) == Zone::Resize(direction))
    });
    if pressed {
        ctx.send_viewport_cmd(ViewportCommand::BeginResize(direction));
    }
}

/// The colors of the window buttons.
pub struct ButtonColors {
    pub glyph: Color32,
    pub hover: Color32,
    pub pressed: Color32,
}

/// Draws the window buttons at the right end of the bar and carries out their clicks. `labels`
/// are the accessible names (minimize, maximize, restore, close). The top pixels of the buttons
/// belong to the resize border when the window has one.
pub fn window_buttons(ui: &mut Ui, frame: Frame, colors: &ButtonColors, labels: &ButtonLabels) {
    let ctx = ui.ctx().clone();
    let size = ctx.content_rect().size();
    let origin = ctx.content_rect().min.to_vec2();
    let maximized = is_maximized(&ctx);
    for button in WindowButton::ALL {
        let rect = button_rect(size, button).translate(origin);
        let mut sense_rect = rect;
        if frame.resizable() {
            sense_rect.min.y += RESIZE_BORDER;
            if button == WindowButton::Close {
                sense_rect.max.x -= RESIZE_BORDER;
            }
        }
        let id = ui.scope_id().with(("window-button", button as u8));
        let response = ui.interact(sense_rect, id, Sense::click());
        let label = match button {
            WindowButton::Minimize => &labels.minimize,
            WindowButton::Maximize if maximized => &labels.restore,
            WindowButton::Maximize => &labels.maximize,
            WindowButton::Close => &labels.close,
        };
        response.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, true, label));
        let (fill, glyph) = match (
            button,
            response.is_pointer_button_down_on(),
            response.hovered(),
        ) {
            (WindowButton::Close, true, _) => (Some(CLOSE_PRESSED), Color32::WHITE),
            (WindowButton::Close, false, true) => (Some(CLOSE_HOVER), Color32::WHITE),
            (_, true, _) => (Some(colors.pressed), colors.glyph),
            (_, false, true) => (Some(colors.hover), colors.glyph),
            _ => (None, colors.glyph),
        };
        let focused = response.has_focus();
        let painter = ui.painter();
        if let Some(fill) = fill {
            painter.rect_filled(rect, CornerRadius::ZERO, fill);
        }
        if focused {
            painter.rect_stroke(
                rect.shrink(2.0),
                CornerRadius::same(4),
                Stroke::new(1.5, colors.glyph),
                StrokeKind::Inside,
            );
        }
        paint_glyph(painter, rect.center(), button, maximized, glyph);
        if response.clicked() {
            let command = match button {
                WindowButton::Minimize => ViewportCommand::Minimized(true),
                WindowButton::Maximize => ViewportCommand::Maximized(!maximized),
                WindowButton::Close => ViewportCommand::Close,
            };
            ctx.send_viewport_cmd(command);
        }
        response.on_hover_text(label.as_str());
    }
}

/// The accessible names of the window buttons.
pub struct ButtonLabels {
    pub minimize: String,
    pub maximize: String,
    pub restore: String,
    pub close: String,
}

/// The 10-point glyph of a window button.
fn paint_glyph(painter: &Painter, c: Pos2, button: WindowButton, maximized: bool, color: Color32) {
    let stroke = Stroke::new(1.0, color);
    let half = 5.0;
    match button {
        WindowButton::Minimize => {
            painter.line_segment([pos2(c.x - half, c.y), pos2(c.x + half, c.y)], stroke);
        }
        WindowButton::Maximize if maximized => {
            let back = Rect::from_min_size(pos2(c.x - half + 2.0, c.y - half), vec2(8.0, 8.0));
            let front = Rect::from_min_size(pos2(c.x - half, c.y - half + 2.0), vec2(8.0, 8.0));
            painter.line_segment([back.left_top(), back.right_top()], stroke);
            painter.line_segment([back.right_top(), back.right_bottom()], stroke);
            painter.rect_stroke(front, CornerRadius::ZERO, stroke, StrokeKind::Middle);
        }
        WindowButton::Maximize => {
            let square = Rect::from_center_size(c, vec2(2.0 * half, 2.0 * half));
            painter.rect_stroke(square, CornerRadius::ZERO, stroke, StrokeKind::Middle);
        }
        WindowButton::Close => {
            painter.line_segment([c + vec2(-half, -half), c + vec2(half, half)], stroke);
            painter.line_segment([c + vec2(-half, half), c + vec2(half, -half)], stroke);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SIZE: Vec2 = vec2(1280.0, 800.0);
    const CUSTOM: Frame = Frame {
        custom: true,
        maximized: false,
    };
    const MAXIMIZED: Frame = Frame {
        custom: true,
        maximized: true,
    };
    const SYSTEM: Frame = Frame {
        custom: false,
        maximized: false,
    };

    fn zone(x: f32, y: f32, frame: Frame) -> Zone {
        hit_test(SIZE, pos2(x, y), frame)
    }

    #[test]
    fn the_edges_resize_the_window() {
        use ResizeDirection::*;
        assert_eq!(zone(640.0, 0.0, CUSTOM), Zone::Resize(North));
        assert_eq!(zone(640.0, 4.9, CUSTOM), Zone::Resize(North));
        assert_eq!(zone(640.0, 799.0, CUSTOM), Zone::Resize(South));
        assert_eq!(zone(0.0, 400.0, CUSTOM), Zone::Resize(West));
        assert_eq!(zone(1279.0, 400.0, CUSTOM), Zone::Resize(East));
        assert_eq!(zone(1275.0, 400.0, CUSTOM), Zone::Resize(East));
        assert_eq!(zone(5.0, 400.0, CUSTOM), Zone::Client);
        assert_eq!(zone(1274.0, 400.0, CUSTOM), Zone::Client);
    }

    #[test]
    fn the_corners_resize_diagonally_with_a_longer_grip() {
        use ResizeDirection::*;
        assert_eq!(zone(0.0, 0.0, CUSTOM), Zone::Resize(NorthWest));
        assert_eq!(zone(1279.0, 0.0, CUSTOM), Zone::Resize(NorthEast));
        assert_eq!(zone(0.0, 799.0, CUSTOM), Zone::Resize(SouthWest));
        assert_eq!(zone(1279.0, 799.0, CUSTOM), Zone::Resize(SouthEast));
        // Along an edge, near the corner.
        assert_eq!(zone(12.0, 1.0, CUSTOM), Zone::Resize(NorthWest));
        assert_eq!(zone(1.0, 12.0, CUSTOM), Zone::Resize(NorthWest));
        assert_eq!(zone(1270.0, 798.0, CUSTOM), Zone::Resize(SouthEast));
        assert_eq!(zone(1278.0, 790.0, CUSTOM), Zone::Resize(SouthEast));
        assert_eq!(zone(1.0, 790.0, CUSTOM), Zone::Resize(SouthWest));
        // Past the grip it is a plain edge.
        assert_eq!(zone(20.0, 1.0, CUSTOM), Zone::Resize(North));
        assert_eq!(zone(1.0, 20.0, CUSTOM), Zone::Resize(West));
    }

    #[test]
    fn the_buttons_sit_at_the_right_end_of_the_bar() {
        let close = 1280.0 - BUTTON_WIDTH / 2.0;
        let maximize = close - BUTTON_WIDTH;
        let minimize = maximize - BUTTON_WIDTH;
        assert_eq!(zone(close, 30.0, CUSTOM), Zone::Button(WindowButton::Close));
        assert_eq!(
            zone(maximize, 30.0, CUSTOM),
            Zone::Button(WindowButton::Maximize)
        );
        assert_eq!(
            zone(minimize, 30.0, CUSTOM),
            Zone::Button(WindowButton::Minimize)
        );
        assert_eq!(
            zone(1280.0 - 3.0 * BUTTON_WIDTH - 1.0, 30.0, CUSTOM),
            Zone::TitleBar
        );
        assert_eq!(zone(close, BAR_HEIGHT, CUSTOM), Zone::Client);
        // The top border of the buttons resizes the window; a maximized window has none.
        assert_eq!(
            zone(maximize, 2.0, CUSTOM),
            Zone::Resize(ResizeDirection::North)
        );
        assert_eq!(
            zone(maximize, 2.0, MAXIMIZED),
            Zone::Button(WindowButton::Maximize)
        );
        assert_eq!(
            zone(1279.0, 0.0, MAXIMIZED),
            Zone::Button(WindowButton::Close)
        );
        assert_eq!(
            button_rect(SIZE, WindowButton::Minimize).width(),
            BUTTON_WIDTH
        );
    }

    #[test]
    fn the_bar_moves_the_window_and_the_rest_is_the_interface() {
        assert_eq!(zone(640.0, 30.0, CUSTOM), Zone::TitleBar);
        assert_eq!(zone(100.0, BAR_HEIGHT - 0.5, CUSTOM), Zone::TitleBar);
        assert_eq!(zone(640.0, BAR_HEIGHT, CUSTOM), Zone::Client);
        assert_eq!(zone(640.0, 400.0, CUSTOM), Zone::Client);
        assert_eq!(zone(-1.0, 30.0, CUSTOM), Zone::Client);
        assert_eq!(zone(640.0, 800.0, CUSTOM), Zone::Client);
    }

    #[test]
    fn with_the_system_frame_there_is_no_border_and_no_buttons() {
        assert_eq!(zone(0.0, 0.0, SYSTEM), Zone::TitleBar);
        assert_eq!(zone(1279.0, 30.0, SYSTEM), Zone::TitleBar);
        assert_eq!(zone(0.0, 400.0, SYSTEM), Zone::Client);
        assert_eq!(zone(640.0, 799.0, SYSTEM), Zone::Client);
        assert_eq!(buttons_width(WindowChrome::System), 0.0);
        assert_eq!(buttons_width(WindowChrome::Transparent), 0.0);
        assert_eq!(buttons_width(WindowChrome::Custom), 3.0 * BUTTON_WIDTH);
    }
}
