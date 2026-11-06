//! Icons: line glyphs drawn in a 20 × 20 box with a 1.6 pt round stroke.
//!
//! The glyphs record drawing operations through a small [`Pen`] trait and are
//! replayed by whichever backend draws them: nsh's canvas for the views, an
//! `NSBezierPath` pen for the macOS menu bar (`platform::status_item`). One
//! vector set stays crisp at every size and every appearance. The tint is the
//! current text color unless the caller gives an [`AdaptiveColor`], and the
//! platform switches that natively without redrawing.
//!
//! ```ignore
//! icon(IconName::Send).size(17.0)
//! icon(IconName::Check).size(14.0).color(colors::SUCCESS)
//! ```

use std::f32::consts::{PI, TAU};

use nsh_core::SvgPaint;
use nsh_core::canvas::{LineCap, LineJoin};
use nsh_core::prelude::*;

/// Every glyph the app uses. One family: 20 × 20 box, 1.6 pt stroke, round
/// caps and joins, 2 pt padding from the box edge.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum IconName {
    /// The app mark and the send destination: a paper plane.
    #[default]
    Send,
    /// An arrow landing in a tray: receive.
    Receive,
    /// A clock face: history.
    History,
    /// A cog: settings.
    Settings,
    /// A folder with a tab.
    Folder,
    /// A page with a folded corner.
    File,
    /// A framed landscape.
    Image,
    /// A capital T: text.
    Text,
    /// An ×.
    Close,
    /// A tick.
    Check,
    /// `›`.
    ChevronRight,
    /// `⌄`.
    ChevronDown,
    /// An arrow up.
    ArrowUp,
    /// An arrow down.
    ArrowDown,
    /// A laptop with a base.
    Laptop,
    /// A phone.
    Phone,
    /// A tablet.
    Tablet,
    /// A globe with a meridian.
    Globe,
    /// A padlock.
    Lock,
    /// A plus.
    Plus,
    /// A waste basket.
    Trash,
    /// Two overlapping pages: copy.
    Copy,
    /// A box with an arrow leaving: open elsewhere.
    ExternalLink,
    /// A crossed circle: cancel.
    Cancel,
    /// A magnifier.
    Search,
    /// A clockwise arrow: refresh discovery.
    Refresh,
    /// A map pin.
    Pin,
    /// Two people.
    Users,
    /// An arrow out of a tray: upload.
    Upload,
    /// A warning triangle.
    Warning,
    /// A lightning bolt.
    Bolt,
}

nsh_core::static_reactive!(IconName: IconName);

/// A glyph, 20 × 20 unless sized otherwise.
#[derive(Clone)]
pub struct Icon {
    name: IconName,
    size: f32,
    color: Option<Reactive<AdaptiveColor>>,
}

/// An icon for `name`, in the current text color.
pub fn icon(name: IconName) -> Icon {
    Icon {
        name,
        size: 20.0,
        color: None,
    }
}

impl Icon {
    /// Its width and height in points (default 20).
    pub fn size(mut self, size: f32) -> Self {
        self.size = size;
        self
    }

    /// Its tint (default: the surrounding text color).
    pub fn color(mut self, color: impl IntoReactive<AdaptiveColor>) -> Self {
        self.color = Some(color.into_reactive());
        self
    }
}

impl IntoElement for Icon {
    fn into_element(self) -> AnyElement {
        let Icon {
            name,
            size,
            color: tint,
        } = self;
        view()
            .size(px(size))
            .flex_shrink_0()
            .pointer_events(PointerEvents::None)
            .child(
                canvas(move || {
                    let scale = size / 20.0;
                    let painter = Painter::new()
                        .scale(scale, scale)
                        .line_width(1.6)
                        .line_cap(LineCap::Round)
                        .line_join(LineJoin::Round);
                    let painter = match &tint {
                        Some(tint) => {
                            let color = color(tint.get());
                            painter.stroke_color(color).fill_color(color)
                        }
                        None => painter
                            .stroke_style_from_view(SvgPaint::CurrentColor)
                            .fill_style_from_view(SvgPaint::CurrentColor),
                    };
                    name.draw(painter)
                })
                .size_full(),
            )
            .into_element()
    }
}

/// The point (`x`, `y`) in the 20 × 20 box.
fn pt(x: f32, y: f32) -> Point<Points> {
    point(px(x), px(y))
}

/// The drawing operations the glyphs are made of, in the 20 × 20 box (y
/// grows downward). The view draws them with nsh's canvas [`Painter`]; the
/// macOS menu bar menu draws the same glyphs with AppKit
/// (`platform::status_item`), so both show one icon set.
///
/// Each operation consumes and returns the pen, like [`Painter`]'s builder.
/// A path starts with [`Pen::begin`] and ends with [`Pen::stroke`] or
/// [`Pen::fill`]; the line width (1.6), round caps and joins are the pen's.
pub trait Pen: Sized {
    fn begin(self) -> Self;
    fn move_to(self, x: f32, y: f32) -> Self;
    fn line_to(self, x: f32, y: f32) -> Self;
    /// A quadratic Bézier from the current point to (`x`, `y`).
    fn quad_to(self, cx: f32, cy: f32, x: f32, y: f32) -> Self;
    fn close(self) -> Self;
    /// A circular arc around (`x`, `y`) from `start` to `end` radians,
    /// angles growing clockwise on screen (y down), as on a canvas.
    fn arc(self, x: f32, y: f32, r: f32, start: f32, end: f32) -> Self;
    fn round_rect(self, x: f32, y: f32, w: f32, h: f32, r: f32) -> Self;
    /// A full ellipse centered on (`cx`, `cy`).
    fn ellipse(self, cx: f32, cy: f32, rx: f32, ry: f32) -> Self;
    fn stroke(self) -> Self;
    fn fill(self) -> Self;
}

impl Pen for Painter {
    fn begin(self) -> Self {
        self.begin_path()
    }
    fn move_to(self, x: f32, y: f32) -> Self {
        Painter::move_to(self, x, y)
    }
    fn line_to(self, x: f32, y: f32) -> Self {
        Painter::line_to(self, x, y)
    }
    fn quad_to(self, cx: f32, cy: f32, x: f32, y: f32) -> Self {
        self.quadratic_curve_to(pt(cx, cy), pt(x, y))
    }
    fn close(self) -> Self {
        self.close_path()
    }
    fn arc(self, x: f32, y: f32, r: f32, start: f32, end: f32) -> Self {
        Painter::arc(self, x, y, r, start, end)
    }
    fn round_rect(self, x: f32, y: f32, w: f32, h: f32, r: f32) -> Self {
        Painter::round_rect(self, bounds(pt(x, y), size(px(w), px(h))), px(r))
    }
    fn ellipse(self, cx: f32, cy: f32, rx: f32, ry: f32) -> Self {
        Painter::ellipse(
            self,
            pt(cx, cy),
            size(px(rx), px(ry)),
            Radians(0.0),
            Radians(0.0),
            Radians(TAU),
            false,
        )
    }
    fn stroke(self) -> Self {
        Painter::stroke(self)
    }
    fn fill(self) -> Self {
        Painter::fill(self)
    }
}

/// A stroked polyline through `points` (`x, y, x, y, …`).
fn line<P: Pen>(pen: P, points: &[f32]) -> P {
    stroke(pen, points, false)
}

/// A closed, stroked polygon through `points`.
fn poly<P: Pen>(pen: P, points: &[f32]) -> P {
    stroke(pen, points, true)
}

fn stroke<P: Pen>(pen: P, points: &[f32], close: bool) -> P {
    let Some(first) = points.first_chunk::<2>() else {
        return pen;
    };
    let mut pen = pen.begin().move_to(first[0], first[1]);
    for pair in points[2..].chunks_exact(2) {
        pen = pen.line_to(pair[0], pair[1]);
    }
    if close {
        pen = pen.close();
    }
    pen.stroke()
}

/// A stroked, rounded rectangle.
fn round_rect<P: Pen>(pen: P, x: f32, y: f32, w: f32, h: f32, r: f32) -> P {
    pen.begin().round_rect(x, y, w, h, r).stroke()
}

/// A stroked circle.
fn circle<P: Pen>(pen: P, x: f32, y: f32, r: f32) -> P {
    pen.begin().arc(x, y, r, 0.0, TAU).stroke()
}

/// A filled dot.
fn dot<P: Pen>(pen: P, x: f32, y: f32, r: f32) -> P {
    pen.begin().arc(x, y, r, 0.0, TAU).fill()
}

impl IconName {
    /// Draws the glyph in a 20 × 20 box with any [`Pen`].
    pub fn draw<P: Pen>(self, painter: P) -> P {
        match self {
            IconName::Send => {
                let p = poly(painter, &[17.2, 3.4, 3.2, 9.2, 9.0, 11.4, 16.8, 4.2]);
                poly(p, &[11.4, 17.0, 9.0, 11.4, 3.2, 9.2])
            }
            IconName::Receive => {
                let p = line(painter, &[10.0, 3.0, 10.0, 12.4, 5.6, 8.0]);
                let p = line(p, &[10.0, 12.4, 14.4, 8.0]);
                line(p, &[3.4, 13.6, 3.4, 16.4, 16.6, 16.4, 16.6, 13.6])
            }
            IconName::History => {
                let p = circle(painter, 10.0, 10.0, 6.4);
                line(p, &[10.0, 5.9, 10.0, 10.0, 13.0, 11.8])
            }
            IconName::Settings => {
                // Two sliders: legible at 16 pt where a gear turns to mush.
                let p = line(painter, &[7.0, 3.4, 7.0, 16.6]);
                let p = line(p, &[13.0, 3.4, 13.0, 16.6]);
                let p = dot(p, 7.0, 7.2, 2.4);
                dot(p, 13.0, 12.8, 2.4)
            }
            IconName::Folder => poly(
                painter,
                &[
                    3.0, 15.6, 3.0, 5.2, 7.8, 5.2, 9.4, 7.6, 17.0, 7.6, 17.0, 15.6,
                ],
            ),
            IconName::File => {
                let p = poly(
                    painter,
                    &[5.4, 17.0, 5.4, 3.0, 11.4, 3.0, 14.6, 6.4, 14.6, 17.0],
                );
                line(p, &[11.4, 3.0, 11.4, 6.4, 14.6, 6.4])
            }
            IconName::Image => {
                let p = round_rect(painter, 3.0, 3.6, 14.0, 12.8, 2.8);
                let p = line(p, &[3.4, 13.8, 7.8, 9.4, 12.0, 13.4]);
                let p = line(p, &[10.4, 11.8, 12.8, 9.4, 16.6, 13.6]);
                dot(p, 12.9, 7.4, 1.2)
            }
            IconName::Text => {
                let p = line(painter, &[4.0, 5.0, 16.0, 5.0]);
                let p = line(p, &[10.0, 5.0, 10.0, 16.0]);
                line(p, &[7.6, 16.0, 12.4, 16.0])
            }
            IconName::Close => {
                let p = line(painter, &[5.4, 5.4, 14.6, 14.6]);
                line(p, &[14.6, 5.4, 5.4, 14.6])
            }
            IconName::Check => line(painter, &[4.4, 10.5, 8.3, 14.2, 15.6, 5.8]),
            IconName::ChevronRight => line(painter, &[7.8, 4.6, 13.2, 10.0, 7.8, 15.4]),
            IconName::ChevronDown => line(painter, &[4.6, 7.8, 10.0, 13.2, 15.4, 7.8]),
            IconName::ArrowUp => {
                let p = line(painter, &[10.0, 16.4, 10.0, 3.6]);
                line(p, &[5.0, 8.6, 10.0, 3.6, 15.0, 8.6])
            }
            IconName::ArrowDown => {
                let p = line(painter, &[10.0, 3.6, 10.0, 16.4]);
                line(p, &[5.0, 11.4, 10.0, 16.4, 15.0, 11.4])
            }
            IconName::Laptop => {
                let p = round_rect(painter, 3.4, 4.6, 13.2, 8.8, 1.6);
                line(p, &[1.8, 16.6, 18.2, 16.6])
            }
            IconName::Phone => {
                let p = round_rect(painter, 6.0, 2.6, 8.0, 14.8, 2.2);
                line(p, &[8.9, 14.9, 11.1, 14.9])
            }
            IconName::Tablet => {
                let p = round_rect(painter, 4.4, 2.6, 11.2, 14.8, 2.2);
                dot(p, 10.0, 14.7, 0.75)
            }
            IconName::Globe => {
                let p = circle(painter, 10.0, 10.0, 6.6);
                let p = p.begin().ellipse(10.0, 10.0, 3.0, 6.6).stroke();
                line(p, &[3.6, 10.0, 16.4, 10.0])
            }
            IconName::Lock => {
                let p = round_rect(painter, 4.6, 9.0, 10.8, 8.0, 2.2);
                let p = p.begin().arc(10.0, 9.0, 3.3, PI, TAU).stroke();
                line(p, &[10.0, 12.0, 10.0, 14.4])
            }
            IconName::Plus => {
                let p = line(painter, &[10.0, 3.8, 10.0, 16.2]);
                line(p, &[3.8, 10.0, 16.2, 10.0])
            }
            IconName::Trash => {
                let p = line(painter, &[3.4, 5.6, 16.6, 5.6]);
                let p = line(p, &[7.8, 5.6, 7.8, 3.4, 12.2, 3.4, 12.2, 5.6]);
                let p = poly(p, &[5.2, 5.6, 5.9, 16.6, 14.1, 16.6, 14.8, 5.6]);
                let p = line(p, &[8.5, 8.6, 8.7, 13.8]);
                line(p, &[11.5, 8.6, 11.3, 13.8])
            }
            IconName::Copy => {
                let p = round_rect(painter, 7.2, 3.0, 9.8, 9.8, 1.6);
                round_rect(p, 3.0, 7.2, 9.8, 9.8, 1.6)
            }
            IconName::ExternalLink => {
                let p = line(
                    painter,
                    &[9.4, 4.0, 4.0, 4.0, 4.0, 16.0, 16.0, 16.0, 16.0, 10.6],
                );
                let p = line(p, &[10.6, 9.4, 16.4, 3.6]);
                line(p, &[12.2, 3.6, 16.4, 3.6, 16.4, 7.8])
            }
            IconName::Cancel => {
                let p = circle(painter, 10.0, 10.0, 6.6);
                let p = line(p, &[7.4, 7.4, 12.6, 12.6]);
                line(p, &[12.6, 7.4, 7.4, 12.6])
            }
            IconName::Refresh => {
                let p = painter
                    .begin()
                    .arc(10.0, 10.0, 6.0, 0.3 * PI, 1.9 * PI)
                    .stroke();
                line(p, &[12.5, 7.8, 15.8, 8.2, 16.2, 4.9])
            }
            IconName::Search => {
                let p = circle(painter, 8.6, 8.6, 5.0);
                line(p, &[12.3, 12.3, 16.6, 16.6])
            }
            IconName::Pin => {
                let p = circle(painter, 10.0, 7.8, 4.1);
                let p = line(p, &[6.9, 10.7, 10.0, 17.2]);
                line(p, &[13.1, 10.7, 10.0, 17.2])
            }
            IconName::Users => {
                let p = circle(painter, 7.6, 7.0, 2.7);
                let p = p
                    .begin()
                    .move_to(3.2, 16.6)
                    .quad_to(3.2, 13.4, 5.6, 12.7)
                    .quad_to(9.6, 12.7, 12.0, 13.4)
                    .quad_to(12.0, 13.4, 12.0, 16.6)
                    .stroke();
                let p = circle(p, 13.9, 8.2, 2.2);
                line(p, &[13.4, 12.9, 16.0, 12.9, 16.8, 16.6])
            }
            IconName::Upload => {
                let p = line(painter, &[10.0, 12.6, 10.0, 3.0, 14.4, 7.2]);
                let p = line(p, &[10.0, 3.0, 5.6, 7.2]);
                line(p, &[4.0, 17.2, 16.0, 17.2])
            }
            IconName::Warning => {
                let p = poly(painter, &[10.0, 3.2, 17.4, 16.4, 2.6, 16.4]);
                let p = line(p, &[10.0, 8.0, 10.0, 11.8]);
                dot(p, 10.0, 14.0, 0.85)
            }
            IconName::Bolt => poly(
                painter,
                &[
                    11.6, 2.6, 5.2, 11.2, 9.6, 11.2, 8.4, 17.4, 14.8, 8.8, 10.4, 8.8,
                ],
            ),
        }
    }
}
