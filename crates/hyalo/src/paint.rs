//! Draws a laid-out page for the settings window, dark, over the DWM
//! backdrop: cards a shade lighter than the glass, text in the launcher's
//! type, its blue for what is on, listening or typed into, and every
//! control in the look the panel gives it (hovered, pressed, ringed,
//! dimmed).
//!
//! The `windows` crate marks every COM call `unsafe`. The invariant all of
//! them rely on: each interface is a live COM object owned by `Painter`,
//! used only from the UI thread that created it.

use std::ops::Range;

use windows::Win32::Foundation::HWND;
use windows::Win32::Graphics::Direct2D::Common::{D2D_RECT_F, D2D1_COLOR_F};
use windows::Win32::Graphics::Direct2D::{
    D2D1_ANTIALIAS_MODE_ALIASED, D2D1_ELLIPSE, D2D1_ROUNDED_RECT,
};
use windows::Win32::Graphics::DirectWrite::{
    DWRITE_FONT_WEIGHT_NORMAL, DWRITE_FONT_WEIGHT_SEMI_BOLD, DWRITE_TEXT_ALIGNMENT_CENTER,
    DWRITE_TEXT_METRICS, IDWriteFactory, IDWriteTextFormat, IDWriteTextLayout,
};
use windows::core::Result;
use windows_numerics::{Matrix3x2, Vector2};

use crate::field;
use crate::page::{
    Area, CAP_GAP, CAP_PAD, FIELD_INSET, KEYS_PAD, Laid, Mark, TITLE_SIZE, Target, What,
    field_inside,
};
use crate::panel::Look;
use crate::surface::{
    DISPLAY_FONT, SELECTED_TEXT, Surface, TEXT_FONT, caret_x, hit, line_width, rgba, text_format,
    trimmed,
};

const TEXT: D2D1_COLOR_F = rgba(1.0, 1.0, 1.0, 0.92);
const DIM: D2D1_COLOR_F = rgba(1.0, 1.0, 1.0, 0.6);
const FAINT: D2D1_COLOR_F = rgba(1.0, 1.0, 1.0, 0.36);
const WARNING: D2D1_COLOR_F = rgba(1.0, 0.64, 0.58, 1.0);
/// How far the one-line `layout` is scrolled left in `inside` so the
/// caret before the UTF-16 unit `caret` shows: no further than that.
fn scrolled(layout: &IDWriteTextLayout, caret: u32, inside: Area) -> Result<f32> {
    let x = caret_x(layout, caret)?;
    Ok(field::scrolled(
        0.0,
        x,
        line_width(layout)?,
        inside.width - 2.0,
    ))
}

/// The launcher's blue: its caret and matched letters.
const ACCENT: D2D1_COLOR_F = rgba(0.45, 0.68, 1.0, 1.0);
const ACCENT_HOVER: D2D1_COLOR_F = rgba(0.55, 0.76, 1.0, 1.0);
const CARD: D2D1_COLOR_F = rgba(1.0, 1.0, 1.0, 0.05);
const CARD_EDGE: D2D1_COLOR_F = rgba(0.0, 0.0, 0.0, 0.18);
const ALARM: D2D1_COLOR_F = rgba(0.86, 0.32, 0.28, 0.22);
const ALARM_EDGE: D2D1_COLOR_F = rgba(1.0, 0.45, 0.40, 0.40);
const DIVIDER: D2D1_COLOR_F = rgba(0.0, 0.0, 0.0, 0.26);
const CONTROL: D2D1_COLOR_F = rgba(1.0, 1.0, 1.0, 0.07);
const CONTROL_HOVER: D2D1_COLOR_F = rgba(1.0, 1.0, 1.0, 0.10);
const CONTROL_PRESSED: D2D1_COLOR_F = rgba(1.0, 1.0, 1.0, 0.04);
const CONTROL_EDGE: D2D1_COLOR_F = rgba(1.0, 1.0, 1.0, 0.08);
/// A field or recorder that takes keys: darker, as Windows draws them.
const TYPING: D2D1_COLOR_F = rgba(0.0, 0.0, 0.0, 0.28);
const CAP: D2D1_COLOR_F = rgba(1.0, 1.0, 1.0, 0.10);
const KNOB_ON: D2D1_COLOR_F = rgba(0.07, 0.07, 0.09, 1.0);
const TRACK_OFF: D2D1_COLOR_F = rgba(0.0, 0.0, 0.0, 0.12);
const RING: D2D1_COLOR_F = rgba(1.0, 1.0, 1.0, 0.9);
const SCROLLBAR: D2D1_COLOR_F = rgba(1.0, 1.0, 1.0, 0.32);

const CARD_RADIUS: f32 = 6.0;
const CONTROL_RADIUS: f32 = 4.0;
const HEADER_SIZE: f32 = 14.0;
const NAME_SIZE: f32 = 14.0;
const HELP_SIZE: f32 = 12.0;
const LABEL_SIZE: f32 = 14.0;
/// A key cap's height in the recorder.
const CAP_HEIGHT: f32 = 22.0;
/// What the recorder shows while it listens and nothing is held yet.
const LISTENING: &str = "Press keys";

pub struct Painter {
    surface: Surface,
    fonts: Fonts,
}

/// The text formats a page is drawn in.
struct Fonts {
    title: IDWriteTextFormat,
    header: IDWriteTextFormat,
    name: IDWriteTextFormat,
    help: IDWriteTextFormat,
    /// Entries and what is typed: one line, cut short with an ellipsis.
    plain: IDWriteTextFormat,
    /// What is typed in the field with the focus: one line, whole, for
    /// the caret to move through.
    typed: IDWriteTextFormat,
    /// Labels on buttons and key caps, centred.
    label: IDWriteTextFormat,
}

impl Fonts {
    fn new(dwrite: &IDWriteFactory) -> Result<Self> {
        let (normal, semi_bold) = (DWRITE_FONT_WEIGHT_NORMAL, DWRITE_FONT_WEIGHT_SEMI_BOLD);
        let cut = |size: f32| -> Result<IDWriteTextFormat> {
            trimmed(dwrite, text_format(dwrite, TEXT_FONT, size, normal)?)
        };
        Ok(Self {
            title: text_format(dwrite, DISPLAY_FONT, TITLE_SIZE, semi_bold)?,
            header: text_format(dwrite, TEXT_FONT, HEADER_SIZE, semi_bold)?,
            name: cut(NAME_SIZE)?,
            help: cut(HELP_SIZE)?,
            plain: cut(LABEL_SIZE)?,
            typed: text_format(dwrite, TEXT_FONT, LABEL_SIZE, normal)?,
            label: centred(dwrite)?,
        })
    }
}

/// Labels on buttons and key caps: centred on them.
fn centred(dwrite: &IDWriteFactory) -> Result<IDWriteTextFormat> {
    let label = text_format(dwrite, TEXT_FONT, LABEL_SIZE, DWRITE_FONT_WEIGHT_NORMAL)?;
    unsafe { label.SetTextAlignment(DWRITE_TEXT_ALIGNMENT_CENTER)? };
    Ok(label)
}

impl Painter {
    pub fn new(hwnd: HWND, width: u32, height: u32, dpi: u32) -> Result<Self> {
        let surface = Surface::new(hwnd, width, height, dpi)?;
        let fonts = Fonts::new(&surface.dwrite)?;
        Ok(Self { surface, fonts })
    }

    pub fn resize(&mut self, width: u32, height: u32, dpi: u32) -> Result<()> {
        self.surface.resize(width, height, dpi)
    }

    /// The view's size in DIPs.
    pub fn view(&self) -> (f32, f32) {
        self.surface.size()
    }

    /// How wide `text` is as a control's label, in DIPs.
    pub fn measure(&self, text: &str) -> f32 {
        let width = || -> Result<f32> {
            let layout = self
                .surface
                .layout(text, &self.fonts.plain, f32::MAX, 32.0)?;
            let mut metrics = DWRITE_TEXT_METRICS::default();
            unsafe { layout.GetMetrics(&mut metrics)? };
            Ok(metrics.widthIncludingTrailingWhitespace)
        };
        // Without DirectWrite's answer, nothing: a button keeps its least
        // width.
        width().unwrap_or_default()
    }

    /// Draws `laid` scrolled down by `scroll`, each control as `look`
    /// says it looks.
    pub fn draw(&self, laid: &Laid, scroll: f32, look: &dyn Fn(Target) -> Look) -> Result<()> {
        self.surface.begin();
        let drawn = self.draw_page(laid, scroll, look);
        let identity = Matrix3x2::identity();
        unsafe { self.surface.dc.SetTransform(&identity) };
        self.surface.end(drawn)
    }

    fn draw_page(&self, laid: &Laid, scroll: f32, look: &dyn Fn(Target) -> Look) -> Result<()> {
        unsafe {
            let moved = Matrix3x2::translation(0.0, -scroll);
            self.surface.dc.SetTransform(&moved);
        }
        for mark in &laid.marks {
            self.draw_mark(mark, look)?;
        }
        unsafe { self.surface.dc.SetTransform(&Matrix3x2::identity()) };
        self.draw_scrollbar(laid.height, scroll)
    }

    fn draw_mark(&self, mark: &Mark, look: &dyn Fn(Target) -> Look) -> Result<()> {
        match mark.what.target() {
            Some(target) => self.draw_control(mark, look(target)),
            None => self.draw_text(mark),
        }
    }

    /// A mark that only shows: text, a card, a hairline.
    fn draw_text(&self, mark: &Mark) -> Result<()> {
        match &mark.what {
            What::Title(title) => self.draw_title(title, mark.at),
            What::Card { alarm, .. } => self.draw_card(mark.at, *alarm),
            What::Divider => self.fill(mark.at, DIVIDER),
            what => match self.words(what, mark.muted) {
                Some((text, format, color)) => self.write(text, format, mark.at, color),
                None => Ok(()),
            },
        }
    }

    /// The text a mark writes, in what format and colour; what is on a
    /// muted card is faint.
    fn words<'a>(
        &'a self,
        what: &'a What,
        muted: bool,
    ) -> Option<(&'a str, &'a IDWriteTextFormat, D2D1_COLOR_F)> {
        let text = if muted { FAINT } else { TEXT };
        let fonts = &self.fonts;
        match what {
            What::Header(header) => Some((header, &fonts.header, TEXT)),
            What::Name(name) => Some((name, &fonts.name, text)),
            What::Help { text: help, alarm } => {
                let color = if *alarm { WARNING } else { DIM };
                Some((help, &fonts.help, color))
            }
            What::Entry(entry) | What::Line(entry) => Some((entry, &fonts.plain, text)),
            _ => None,
        }
    }

    /// A control, as `look` says it looks.
    fn draw_control(&self, mark: &Mark, look: Look) -> Result<()> {
        let at = mark.at;
        match &mark.what {
            What::Button { label, .. } => self.draw_button(label, at, look),
            What::Toggle { on, .. } => self.draw_toggle(*on, at, look),
            What::Keys {
                caps, listening, ..
            } => self.draw_keys(caps, *listening, at, look),
            What::Remove(_) => self.draw_remove(at, look),
            What::Field {
                text,
                caret,
                selection,
                placeholder,
                ..
            } => self.draw_field(text, placeholder, (*caret, selection), at, look),
            _ => Ok(()),
        }
    }

    /// hyalo's lens, as the launcher's search icon draws it, and the title.
    fn draw_title(&self, title: &str, at: Area) -> Result<()> {
        let center = Vector2 {
            X: at.x + 13.0,
            Y: at.y + at.height / 2.0 - 3.0,
        };
        let lens = D2D1_ELLIPSE {
            point: center,
            radiusX: 10.0,
            radiusY: 10.0,
        };
        let handle = (
            Vector2 {
                X: center.X + 7.2,
                Y: center.Y + 7.2,
            },
            Vector2 {
                X: center.X + 13.5,
                Y: center.Y + 13.5,
            },
        );
        let brush = self.surface.solid(ACCENT)?;
        unsafe {
            self.surface.dc.DrawEllipse(&lens, &brush, 2.5, None);
            self.surface
                .dc
                .DrawLine(handle.0, handle.1, &brush, 3.0, None);
        }
        // The title is short and never cut: its line may run past the
        // page's edge.
        let text = Area {
            x: at.x + 40.0,
            ..at
        };
        self.write(title, &self.fonts.title, text, TEXT)
    }

    fn draw_card(&self, at: Area, alarm: bool) -> Result<()> {
        let (fill, edge) = if alarm {
            (ALARM, ALARM_EDGE)
        } else {
            (CARD, CARD_EDGE)
        };
        self.round(at, CARD_RADIUS, Some(fill), Some(edge))
    }

    fn draw_button(&self, label: &str, at: Area, look: Look) -> Result<()> {
        let rest = if look.enabled {
            CONTROL
        } else {
            CONTROL_PRESSED
        };
        let fill = lit(look).unwrap_or(rest);
        self.round(at, CONTROL_RADIUS, Some(fill), Some(CONTROL_EDGE))?;
        let color = if look.enabled { TEXT } else { FAINT };
        self.write(label, &self.fonts.label, at, color)?;
        self.ring(at, CONTROL_RADIUS, look)
    }

    /// The switch: a pill with a knob, on the right and blue when on.
    fn draw_toggle(&self, on: bool, at: Area, look: Look) -> Result<()> {
        let radius = at.height / 2.0;
        let (fill, edge) = match (on, look.hovered) {
            (true, true) => (ACCENT_HOVER, None),
            (true, false) => (ACCENT, None),
            (false, _) => (TRACK_OFF, Some(DIM)),
        };
        self.round(at, radius, Some(fill), edge)?;
        let (x, color) = match on {
            true => (at.x + at.width - radius, KNOB_ON),
            false => (at.x + radius, TEXT),
        };
        let knob = if look.hovered { 7.0 } else { 6.0 };
        let disc = D2D1_ELLIPSE {
            point: Vector2 {
                X: x,
                Y: at.y + radius,
            },
            radiusX: knob,
            radiusY: knob,
        };
        let brush = self.surface.solid(color)?;
        unsafe { self.surface.dc.FillEllipse(&disc, &brush) };
        self.ring(at, radius, look)
    }

    /// The recorder: its keys as caps, side by side; blue-edged while it
    /// listens, with a hint after the keys held so far.
    fn draw_keys(&self, caps: &[String], listening: bool, at: Area, look: Look) -> Result<()> {
        let edge = if listening { ACCENT } else { CONTROL_EDGE };
        self.round(at, CONTROL_RADIUS, Some(fill(look, listening)), Some(edge))?;
        let color = if look.enabled { TEXT } else { FAINT };
        let end = self.draw_caps(caps, at, color)?;
        if listening {
            let hint = if caps.is_empty() { LISTENING } else { "…" };
            // A word or an ellipsis, never cut.
            let rest = Area {
                x: end + FIELD_INSET / 2.0 - CAP_GAP,
                ..at
            };
            self.write(hint, &self.fonts.plain, rest, DIM)?;
        }
        self.ring(at, CONTROL_RADIUS, look)
    }

    /// Each key a cap, from the left of the recorder `at`; where the next
    /// would start.
    fn draw_caps(&self, caps: &[String], at: Area, color: D2D1_COLOR_F) -> Result<f32> {
        let mut x = at.x + KEYS_PAD;
        let top = at.y + (at.height - CAP_HEIGHT) / 2.0;
        for cap in caps {
            let width = self.measure(cap) + 2.0 * CAP_PAD;
            let key = Area {
                x,
                y: top,
                width,
                height: CAP_HEIGHT,
            };
            self.round(key, CONTROL_RADIUS, Some(CAP), None)?;
            self.write(cap, &self.fonts.label, key, color)?;
            x += width + CAP_GAP;
        }
        Ok(x)
    }

    /// The × that removes an entry: a square button, lit when pointed at,
    /// with the diagonals of a small square in its middle.
    fn draw_remove(&self, at: Area, look: Look) -> Result<()> {
        self.round(at, CONTROL_RADIUS, lit(look), None)?;
        let (x, y) = at.middle();
        let arm = 5.0;
        let cross = rect(Area {
            x: x - arm,
            y: y - arm,
            width: 2.0 * arm,
            height: 2.0 * arm,
        });
        let color = if look.enabled { TEXT } else { FAINT };
        let brush = self.surface.solid(color)?;
        let point = |x: f32, y: f32| Vector2 { X: x, Y: y };
        unsafe {
            let dc = &self.surface.dc;
            let (top_left, bottom_right) = (
                point(cross.left, cross.top),
                point(cross.right, cross.bottom),
            );
            let (bottom_left, top_right) = (
                point(cross.left, cross.bottom),
                point(cross.right, cross.top),
            );
            dc.DrawLine(top_left, bottom_right, &brush, 1.25, None);
            dc.DrawLine(bottom_left, top_right, &brush, 1.25, None);
        }
        self.ring(at, CONTROL_RADIUS, look)
    }

    /// A text field: what is typed, or the placeholder; with the focus,
    /// a blue line under it, what is selected and the caret, the text
    /// scrolled so the caret shows.
    fn draw_field(
        &self,
        typed: &str,
        placeholder: &str,
        (caret, selection): (u32, &Range<u32>),
        at: Area,
        look: Look,
    ) -> Result<()> {
        let typing = look.focused;
        self.round(
            at,
            CONTROL_RADIUS,
            Some(fill(look, typing)),
            Some(CONTROL_EDGE),
        )?;
        self.draw_underline(at, typing)?;
        let inside = field_inside(at);
        match typing && !typed.is_empty() {
            true => self.draw_typing(typed, (caret, selection), inside)?,
            false => self.draw_words(typed, placeholder, inside, typing)?,
        }
        self.ring(at, CONTROL_RADIUS, look)
    }

    /// What a field holds, cut short, or its placeholder; with the focus
    /// (the field is empty then), the caret at its start.
    fn draw_words(&self, typed: &str, placeholder: &str, inside: Area, typing: bool) -> Result<()> {
        let (words, color) = match typed.is_empty() {
            true => (placeholder, DIM),
            false => (typed, TEXT),
        };
        self.write(words, &self.fonts.plain, inside, color)?;
        match typing {
            true => self.draw_caret(inside.x + 1.0, inside),
            false => Ok(()),
        }
    }

    /// What is typed in the field with the focus, inside `inside`: the
    /// selection behind it and the caret, scrolled so the caret shows.
    fn draw_typing(
        &self,
        typed: &str,
        (caret, selection): (u32, &Range<u32>),
        inside: Area,
    ) -> Result<()> {
        let layout = self.typed(typed, inside)?;
        let left = inside.x - scrolled(&layout, caret, inside)?;
        let dc = &self.surface.dc;
        unsafe { dc.PushAxisAlignedClip(&rect(inside), D2D1_ANTIALIAS_MODE_ALIASED) };
        let drawn = self
            .draw_selection(&layout, selection, left, inside)
            .and_then(|()| self.surface.draw_text(&layout, left, inside.y, TEXT))
            .and_then(|()| self.draw_caret(left + caret_x(&layout, caret)? + 1.0, inside));
        unsafe { dc.PopAxisAlignedClip() };
        drawn
    }

    /// The band behind the letters selected, `left` the line's start.
    fn draw_selection(
        &self,
        layout: &IDWriteTextLayout,
        selection: &Range<u32>,
        left: f32,
        inside: Area,
    ) -> Result<()> {
        let start = left + caret_x(layout, selection.start)?;
        let band = Area {
            x: start,
            y: inside.y + 6.0,
            width: left + caret_x(layout, selection.end)? - start,
            height: (inside.height - 12.0).max(0.0),
        };
        self.fill(band, SELECTED_TEXT)
    }

    /// Where a click `x` DIPs from the page's left, in the field at `at`
    /// that holds `typed` with the caret at `caret`, puts the caret: a
    /// UTF-16 position, as the field is drawn scrolled.
    pub fn field_hit(&self, typed: &str, caret: u32, at: Area, x: f32) -> Result<u32> {
        let inside = field_inside(at);
        let layout = self.typed(typed, inside)?;
        let scroll = scrolled(&layout, caret, inside)?;
        hit(&layout, x - inside.x + scroll, inside.height / 2.0)
    }

    /// `typed` laid out whole on one line as high as `inside`.
    fn typed(&self, typed: &str, inside: Area) -> Result<IDWriteTextLayout> {
        self.surface
            .layout(typed, &self.fonts.typed, 0.0, inside.height)
    }

    /// The line under a field: blue and thicker while it takes keys.
    fn draw_underline(&self, at: Area, typing: bool) -> Result<()> {
        let (line, thickness) = match typing {
            true => (ACCENT, 2.0),
            false => (DIM, 1.0),
        };
        let under = Area {
            x: at.x + 1.0,
            y: at.bottom() - thickness,
            width: (at.width - 2.0).max(0.0),
            height: thickness,
        };
        self.fill(under, line)
    }

    /// The caret at `x`, in the field's inside `inside`.
    fn draw_caret(&self, x: f32, inside: Area) -> Result<()> {
        let caret = Area {
            x,
            y: inside.y + 8.0,
            width: 1.5,
            height: (inside.height - 16.0).max(0.0),
        };
        self.fill(caret, ACCENT)
    }

    /// The ring that shows where the keyboard's focus is.
    fn ring(&self, at: Area, radius: f32, look: Look) -> Result<()> {
        if !look.ring {
            return Ok(());
        }
        let around = Area {
            x: at.x - 3.0,
            y: at.y - 3.0,
            width: at.width + 6.0,
            height: at.height + 6.0,
        };
        let brush = self.surface.solid(RING)?;
        let shape = rounded(around, radius + 3.0, 1.0);
        unsafe {
            self.surface
                .dc
                .DrawRoundedRectangle(&shape, &brush, 2.0, None)
        };
        Ok(())
    }

    /// A thin bar on the right that shows where the view is on the page,
    /// when the page is taller than the view.
    fn draw_scrollbar(&self, content: f32, scroll: f32) -> Result<()> {
        let (width, view) = self.view();
        if content <= view {
            return Ok(());
        }
        let bar = Area {
            x: width - 6.0,
            y: scroll / content * view + 2.0,
            width: 3.0,
            height: view / content * view - 4.0,
        };
        self.round(bar, 1.5, Some(SCROLLBAR), None)
    }

    fn write(
        &self,
        text: &str,
        format: &IDWriteTextFormat,
        at: Area,
        color: D2D1_COLOR_F,
    ) -> Result<()> {
        let layout = self.surface.layout(text, format, at.width, at.height)?;
        self.surface.draw_text(&layout, at.x, at.y, color)
    }

    fn fill(&self, at: Area, color: D2D1_COLOR_F) -> Result<()> {
        let brush = self.surface.solid(color)?;
        unsafe { self.surface.dc.FillRectangle(&rect(at), &brush) };
        Ok(())
    }

    /// A rounded rectangle, filled, edged, or both.
    fn round(
        &self,
        at: Area,
        radius: f32,
        fill: Option<D2D1_COLOR_F>,
        edge: Option<D2D1_COLOR_F>,
    ) -> Result<()> {
        if let Some(fill) = fill {
            let brush = self.surface.solid(fill)?;
            unsafe {
                self.surface
                    .dc
                    .FillRoundedRectangle(&rounded(at, radius, 0.0), &brush)
            };
        }
        if let Some(edge) = edge {
            let brush = self.surface.solid(edge)?;
            let shape = rounded(at, radius, 0.5);
            unsafe {
                self.surface
                    .dc
                    .DrawRoundedRectangle(&shape, &brush, 1.0, None)
            };
        }
        Ok(())
    }
}

/// What a button or a × is lit with, when it may be used: pressed, or
/// under the pointer.
fn lit(look: Look) -> Option<D2D1_COLOR_F> {
    match (look.enabled, look.pressed, look.hovered) {
        (true, true, _) => Some(CONTROL_PRESSED),
        (true, false, true) => Some(CONTROL_HOVER),
        _ => None,
    }
}

/// What a field or the recorder is filled with: darker while it takes
/// keys (`typing`), lighter under the pointer when it may be used.
fn fill(look: Look, typing: bool) -> D2D1_COLOR_F {
    match (typing, look.hovered && look.enabled) {
        (true, _) => TYPING,
        (false, true) => CONTROL_HOVER,
        (false, false) => CONTROL,
    }
}

fn rect(at: Area) -> D2D_RECT_F {
    D2D_RECT_F {
        left: at.x,
        top: at.y,
        right: at.x + at.width,
        bottom: at.y + at.height,
    }
}

/// `at` with corners of `radius`, `inset` in from each side: half a line,
/// so a line of 1 DIP falls on whole pixels.
fn rounded(at: Area, radius: f32, inset: f32) -> D2D1_ROUNDED_RECT {
    let outer = rect(at);
    D2D1_ROUNDED_RECT {
        rect: D2D_RECT_F {
            left: outer.left + inset,
            top: outer.top + inset,
            right: outer.right - inset,
            bottom: outer.bottom - inset,
        },
        radiusX: radius - inset,
        radiusY: radius - inset,
    }
}

#[cfg(test)]
impl Painter {
    pub fn read_frame(&self) -> Result<crate::surface::Frame> {
        self.surface.read_frame()
    }
}

#[cfg(test)]
mod tests {
    use hyalo_core::config::{self, DEFAULT, List};
    use windows::Win32::UI::Input::KeyboardAndMouse::VK_TAB;

    use super::*;
    use crate::page::lay_out;
    use crate::panel::Panel;
    use crate::recorder::Mods;
    use crate::reference::assert_looks_like;
    use crate::surface::{Canvas, Frame};
    use crate::test_desktop::on_hidden_desktop;

    /// What `panel` looks like `width` by `height` pixels at `dpi`, its
    /// scroll kept within the page, as the window keeps it.
    fn painted(panel: &mut Panel, (width, height): (u32, u32), dpi: u32) -> Frame {
        let canvas = Canvas::new();
        let painter = Painter::new(canvas.0, width, height, dpi).expect("a painter");
        let (view, tall) = painter.view();
        let laid = lay_out(&panel.page(), view, &|text| painter.measure(text));
        panel.fit(laid.height, tall);
        painter
            .draw(&laid, panel.scroll(), &|target| panel.look(target))
            .expect("draw");
        painter.read_frame().expect("read the frame back")
    }

    fn written(text: &str) -> config::Written {
        config::written(text).expect("good settings")
    }

    #[test]
    fn the_settings_at_100_percent() {
        on_hidden_desktop(|| {
            let mut panel = Panel::new(Ok(written(DEFAULT)), Some(true));
            let laid = lay_out(&panel.page(), 640.0, &|text| {
                text.chars().count() as f32 * 8.0
            });
            panel.key(VK_TAB.0, Mods::default(), &laid, 2000.0);
            panel.key(VK_TAB.0, Mods::default(), &laid, 2000.0);
            panel.hover(Some(Target::Pick(List::Exclude)));
            let frame = painted(&mut panel, (640, 1040), 96);
            assert_looks_like("settings-100", &frame);
        });
    }

    #[test]
    fn a_problem_in_the_file_at_100_percent() {
        on_hidden_desktop(|| {
            let mut panel = Panel::new(Ok(written(DEFAULT)), None);
            let problem = config::written("\nhotkye = 1\n").expect_err("a problem");
            panel.reload(Err(problem));
            panel.press(Some(Target::Fix));
            let frame = painted(&mut panel, (640, 400), 96);
            assert_looks_like("settings-problem-100", &frame);
        });
    }

    #[test]
    fn recording_typing_and_the_focus_ring_at_150_percent() {
        on_hidden_desktop(|| {
            let text = "hotkey = 'F4'\n[folders]\nrank_lower = ['target', 'C:\\Users\\Me\\AppData\\Local\\Some Vendor\\a folder whose path is long enough to be cut short']\n";
            let mut panel = Panel::new(Ok(written(text)), Some(false));
            panel.press(Some(Target::Name(List::RankLower)));
            panel.release(Some(Target::Name(List::RankLower)));
            let long = r"D:\Projects\a folder whose name is far too long for the field it is typed in, and longer still";
            for unit in long.encode_utf16() {
                panel.char(unit);
            }
            panel.press(Some(Target::Hotkey));
            panel.release(Some(Target::Hotkey));
            let ctrl = Mods {
                ctrl: true,
                ..Mods::default()
            };
            panel.key_up(ctrl);
            let frame = painted(&mut panel, (960, 1050), 144);
            assert_looks_like("settings-recording-150", &frame);
        });
    }

    #[test]
    fn typing_a_name_in_a_page_scrolled_down_at_125_percent() {
        on_hidden_desktop(|| {
            let mut panel = Panel::new(Ok(written(DEFAULT)), None);
            panel.press(Some(Target::Name(List::RankHigher)));
            panel.release(Some(Target::Name(List::RankHigher)));
            for unit in "notes".encode_utf16() {
                panel.char(unit);
            }
            panel.hover(Some(Target::AddName(List::RankHigher)));
            panel.wheel(-1200, 2000.0, 400.0);
            let frame = painted(&mut panel, (800, 500), 120);
            assert_looks_like("settings-typing-125", &frame);
        });
    }

    #[test]
    fn a_selection_in_a_field_and_a_long_name_scrolled_to_its_caret() {
        on_hidden_desktop(|| {
            let mut panel = Panel::new(Ok(written(DEFAULT)), None);
            panel.press(Some(Target::Name(List::RankLower)));
            panel.release(Some(Target::Name(List::RankLower)));
            let long = r"D:\Projects\a folder whose name is far too long for the field it is typed in, and longer";
            for unit in long.encode_utf16() {
                panel.char(unit);
            }
            panel.press(Some(Target::Name(List::RankHigher)));
            panel.release(Some(Target::Name(List::RankHigher)));
            for unit in "my notes here".encode_utf16() {
                panel.char(unit);
            }
            panel.select_word(5);
            panel.wheel(-1200, 2000.0, 400.0);
            let frame = painted(&mut panel, (640, 400), 96);
            assert_looks_like("settings-field-selected-100", &frame);
            panel.press(Some(Target::Name(List::RankLower)));
            panel.release(Some(Target::Name(List::RankLower)));
            let frame = painted(&mut panel, (640, 400), 96);
            assert_looks_like("settings-field-scrolled-100", &frame);
        });
    }

    #[test]
    fn a_page_as_tall_as_the_view_has_no_scrollbar() {
        on_hidden_desktop(|| {
            let canvas = Canvas::new();
            let painter = Painter::new(canvas.0, 640, 300, 96).expect("a painter");
            let bar = |height: f32| {
                let laid = Laid {
                    marks: Vec::new(),
                    height,
                };
                painter
                    .draw(&laid, 0.0, &|_| Look::default())
                    .expect("draw");
                let frame = painter.read_frame().expect("read the frame back");
                (0..300).any(|y| frame.pixel(635, y)[3] > 0)
            };
            assert!(!bar(300.0));
            assert!(bar(301.0));
        });
    }

    #[test]
    fn a_page_taller_than_the_view_shows_where_the_view_is() {
        on_hidden_desktop(|| {
            let mut panel = Panel::new(Ok(written(DEFAULT)), Some(true));
            let short = painted(&mut panel, (640, 300), 96);
            let bar = |frame: &Frame, y: usize| frame.pixel(635, y)[3];
            assert!(bar(&short, 10) > 40, "the bar at the top");
            assert_eq!(bar(&short, 290), 0, "and not at the bottom");
            panel.wheel(-12000, 2000.0, 300.0);
            let scrolled = painted(&mut panel, (640, 300), 96);
            assert!(bar(&scrolled, 290) > 40, "at the bottom, scrolled down");
            let tall = painted(&mut panel, (640, 1200), 96);
            assert!(
                (0..1200).all(|y| bar(&tall, y) == 0),
                "no bar when all fits"
            );
        });
    }
}
