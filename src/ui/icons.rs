//! Small vector controls: stable line weight and short interaction motion.
use crate::{
    theme,
    ui::{animate_bool, control_bg},
};
use egui::{Color32, Pos2, Rect, Response, Sense, Stroke, Ui, Vec2, pos2};

#[derive(Clone, Copy)]
pub enum Icon {
    Screen,
    Soundboard,
    Music,
    Disconnect,
    Voice,
    Announcement,
    TextChannel,
    Settings,
}

pub fn button(ui: &mut Ui, icon: Icon, size: Vec2, selected: bool, tooltip: &str) -> Response {
    let (rect, response) = ui.allocate_exact_size(size, Sense::click());
    response.widget_info(|| {
        egui::WidgetInfo::labeled(egui::WidgetType::Button, ui.is_enabled(), tooltip)
    });
    ui.painter()
        .rect_filled(rect, theme::RADIUS_SM, control_bg(ui, &response, selected));
    let hover = animate_bool(
        ui.ctx(),
        response.id.with("icon_hover"),
        response.hovered(),
        0.12,
    );
    let press = animate_bool(
        ui.ctx(),
        response.id.with("icon_press"),
        response.is_pointer_button_down_on(),
        0.08,
    );
    let center = rect.center() + Vec2::new(0.0, press);
    let color = if matches!(icon, Icon::Disconnect) {
        theme::RED
    } else if selected {
        theme::GREEN
    } else {
        theme::TEXT
    };
    draw(ui.painter(), icon, center, color, hover);
    response
        .on_hover_cursor(egui::CursorIcon::PointingHand)
        .on_hover_text(tooltip)
}

pub fn draw(p: &egui::Painter, icon: Icon, c: Pos2, color: Color32, hover: f32) {
    let stroke = Stroke::new(1.8, color);
    let line = |a: Vec2, b: Vec2| {
        p.line_segment([c + a, c + b], stroke);
    };
    match icon {
        Icon::Screen => {
            p.rect_stroke(
                Rect::from_center_size(c, Vec2::new(19.0, 13.0)),
                2.0,
                stroke,
                egui::StrokeKind::Inside,
            );
            line(Vec2::new(0.0, 6.0), Vec2::new(0.0, 9.0));
            line(Vec2::new(-5.0, 9.0), Vec2::new(5.0, 9.0));
            let y = -2.0 - hover;
            line(Vec2::new(0.0, y + 5.0), Vec2::new(0.0, y - 2.0));
            line(Vec2::new(-3.0, y + 1.0), Vec2::new(0.0, y - 2.0));
            line(Vec2::new(0.0, y - 2.0), Vec2::new(3.0, y + 1.0));
        }
        Icon::Soundboard | Icon::Voice => {
            p.add(egui::Shape::closed_line(
                vec![
                    c + Vec2::new(-8.0, -3.0),
                    c + Vec2::new(-4.0, -3.0),
                    c + Vec2::new(1.0, -7.0),
                    c + Vec2::new(1.0, 7.0),
                    c + Vec2::new(-4.0, 3.0),
                    c + Vec2::new(-8.0, 3.0),
                ],
                stroke,
            ));
            for radius in [5.0, 9.0] {
                let points = (0..=8)
                    .map(|i| {
                        let angle = -0.7 + i as f32 * 1.4 / 8.0;
                        pos2(
                            c.x + (radius + hover) * angle.cos(),
                            c.y + radius * angle.sin(),
                        )
                    })
                    .collect();
                p.add(egui::Shape::line(points, stroke));
            }
        }
        Icon::Music => {
            let c = c - Vec2::new(0.0, hover);
            p.line_segment(
                [c + Vec2::new(-3.0, 5.0), c + Vec2::new(-3.0, -6.0)],
                stroke,
            );
            p.line_segment([c + Vec2::new(7.0, 3.0), c + Vec2::new(7.0, -8.0)], stroke);
            p.line_segment(
                [c + Vec2::new(-3.0, -6.0), c + Vec2::new(7.0, -8.0)],
                Stroke::new(3.0, color),
            );
            p.circle_filled(c + Vec2::new(-5.0, 5.0), 3.0, color);
            p.circle_filled(c + Vec2::new(5.0, 3.0), 3.0, color);
        }
        Icon::Announcement => {
            p.add(egui::Shape::closed_line(
                vec![
                    c + Vec2::new(-8.0, -3.0),
                    c + Vec2::new(-3.0, -3.0),
                    c + Vec2::new(6.0, -7.0),
                    c + Vec2::new(6.0, 7.0),
                    c + Vec2::new(-3.0, 3.0),
                    c + Vec2::new(-8.0, 3.0),
                ],
                stroke,
            ));
            line(Vec2::new(-4.0, 3.0), Vec2::new(-2.0, 8.0));
            line(Vec2::new(-2.0, 8.0), Vec2::new(1.0, 8.0));
            line(Vec2::new(9.0, -3.0 - hover), Vec2::new(11.0, -4.0 - hover));
            line(Vec2::new(9.0, 3.0 + hover), Vec2::new(11.0, 4.0 + hover));
        }
        Icon::TextChannel => {
            for x in [-3.0, 4.0] {
                line(Vec2::new(x, -8.0), Vec2::new(x - 2.0, 8.0));
            }
            for y in [-3.0, 4.0] {
                line(Vec2::new(-8.0, y), Vec2::new(8.0, y));
            }
        }
        Icon::Settings => {
            p.circle_stroke(c, 6.0, stroke);
            p.circle_stroke(c, 2.4, stroke);
            for i in 0..8 {
                let angle = i as f32 * std::f32::consts::TAU / 8.0 + hover * 0.15;
                let direction = Vec2::new(angle.cos(), angle.sin());
                p.line_segment(
                    [c + direction * 6.0, c + direction * 8.5],
                    Stroke::new(2.8, color),
                );
            }
        }
        Icon::Disconnect => {
            p.add(egui::Shape::line(
                vec![
                    c + Vec2::new(-9.0, 2.0),
                    c + Vec2::new(-7.0, -1.0),
                    c + Vec2::new(0.0, -3.0),
                    c + Vec2::new(7.0, -1.0),
                    c + Vec2::new(9.0, 2.0),
                ],
                Stroke::new(3.0, color),
            ));
            line(Vec2::new(-7.0, -1.0), Vec2::new(-7.0, 5.0));
            line(Vec2::new(7.0, -1.0), Vec2::new(7.0, 5.0));
        }
    }
}
