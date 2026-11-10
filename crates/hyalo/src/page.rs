//! A page as the settings window shows it: a title, then sections of
//! cards, each card a stack of strips (a setting with its control, an
//! entry of a list, a field to add one). Pure: `lay_out` places every
//! piece in DIPs, and from that the window draws them, finds what is under
//! the pointer and moves the focus.

use std::ops::Range;

use hyalo_core::config::List;
use hyalo_core::text::Language;

/// What a control does when used.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Target {
    /// The hotkey recorder.
    Hotkey,
    /// The switch for starting with Windows.
    StartWithWindows,
    /// "Add folder…": the system's folder picker, for a list.
    Pick(List),
    /// The × on the entry of a list at this index.
    Remove(List, usize),
    /// The field for a folder name, for a list.
    Name(List),
    /// The button that adds the name in the field.
    AddName(List),
    /// "Open config.toml" in its own card.
    OpenConfig,
    /// "Open config.toml" in the card that tells a problem.
    Fix,
    /// The option of the language choice for this language.
    Language(Language),
}

/// What the window shows.
#[derive(Debug, Clone, PartialEq)]
pub struct Page {
    pub title: String,
    pub sections: Vec<Section>,
}

/// Cards under a header, if they have one.
#[derive(Debug, Clone, PartialEq)]
pub struct Section {
    pub title: Option<String>,
    pub cards: Vec<Card>,
}

/// Strips that belong together, on one rounded card.
#[derive(Debug, Clone, PartialEq)]
pub struct Card {
    /// A card that tells of a problem.
    pub alarm: bool,
    /// A card whose settings cannot be changed now, shown dim.
    pub muted: bool,
    pub strips: Vec<Strip>,
}

/// One row of a card.
#[derive(Debug, Clone, PartialEq)]
pub enum Strip {
    /// A setting: its name, a line of help (a warning when `alarm`), and
    /// its control on the right.
    Setting {
        name: String,
        help: String,
        alarm: bool,
        control: Option<Control>,
    },
    /// An entry of a list, with a × that removes it.
    Entry { text: String, remove: Target },
    /// A field to type in, with its caret and selection in UTF-16 units,
    /// and a button that adds what it holds.
    Field {
        target: Target,
        text: String,
        caret: u32,
        selection: Range<u32>,
        placeholder: String,
        add: Button,
    },
    /// A paragraph, broken into lines that fit the card.
    Text { text: String },
}

/// What sits on the right of a setting.
#[derive(Debug, Clone, PartialEq)]
pub enum Control {
    Button(Button),
    Toggle {
        target: Target,
        on: bool,
    },
    /// The hotkey recorder: the keys it shows, whether it listens, and
    /// what it asks for while it listens and no key is held.
    Keys {
        target: Target,
        caps: Vec<String>,
        listening: bool,
        prompt: String,
    },
    /// Options side by side, one of them on.
    Choice(Vec<Choice>),
}

/// One option of a choice.
#[derive(Debug, Clone, PartialEq)]
pub struct Choice {
    pub label: String,
    pub target: Target,
    pub on: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Button {
    pub label: String,
    pub target: Target,
}

/// A rectangle in DIPs, from the top of the page.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Area {
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
}

impl Area {
    pub fn contains(self, x: f32, y: f32) -> bool {
        (self.x..self.x + self.width).contains(&x) && (self.y..self.y + self.height).contains(&y)
    }

    pub fn bottom(self) -> f32 {
        self.y + self.height
    }

    pub fn middle(self) -> (f32, f32) {
        (self.x + self.width / 2.0, self.y + self.height / 2.0)
    }
}

/// A piece of the page, placed.
#[derive(Debug, Clone, PartialEq)]
pub struct Mark {
    pub at: Area,
    pub what: What,
    /// It is on a card whose settings cannot be changed now: drawn dim.
    pub muted: bool,
}

/// What a mark is.
#[derive(Debug, Clone, PartialEq)]
pub enum What {
    Title(String),
    Header(String),
    Card {
        alarm: bool,
    },
    /// The hairline between two strips of a card.
    Divider,
    /// A setting's name.
    Name(String),
    Help {
        text: String,
        alarm: bool,
    },
    Entry(String),
    Button {
        label: String,
        target: Target,
    },
    Toggle {
        on: bool,
        target: Target,
    },
    Keys {
        caps: Vec<String>,
        listening: bool,
        prompt: String,
        target: Target,
    },
    /// An option of a choice: on, or there to be chosen.
    Choice {
        label: String,
        on: bool,
        target: Target,
    },
    Remove(Target),
    Field {
        text: String,
        caret: u32,
        selection: Range<u32>,
        placeholder: String,
        target: Target,
    },
    /// A line of a paragraph.
    Line(String),
}

impl What {
    /// What it does when used, if it is a control.
    pub fn target(&self) -> Option<Target> {
        match self {
            What::Button { target, .. }
            | What::Toggle { target, .. }
            | What::Keys { target, .. }
            | What::Choice { target, .. }
            | What::Field { target, .. }
            | What::Remove(target) => Some(*target),
            _ => None,
        }
    }
}

/// The page laid out: its marks, top to bottom, and its height.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Laid {
    pub marks: Vec<Mark>,
    pub height: f32,
}

impl Laid {
    /// The control at (x, y), in DIPs from the top of the page.
    pub fn hit(&self, x: f32, y: f32) -> Option<Target> {
        let controls = self.marks.iter().filter(|mark| mark.at.contains(x, y));
        controls.filter_map(|mark| mark.what.target()).next()
    }

    /// The controls in the order Tab moves through them.
    pub fn targets(&self) -> Vec<Target> {
        self.marks
            .iter()
            .filter_map(|mark| mark.what.target())
            .collect()
    }

    /// Where the control `target` is.
    pub fn area(&self, target: Target) -> Option<Area> {
        let mark = self
            .marks
            .iter()
            .find(|mark| mark.what.target() == Some(target));
        mark.map(|mark| mark.at)
    }
}

/// Where a field's text starts, from its left edge.
pub const FIELD_INSET: f32 = 10.0;

/// The inside of a field at `at`: where its text goes.
pub fn field_inside(at: Area) -> Area {
    Area {
        x: at.x + FIELD_INSET,
        width: (at.width - 2.0 * FIELD_INSET).max(0.0),
        ..at
    }
}

/// Space around the page's content.
pub const MARGIN: f32 = 24.0;
/// The page title's strip, and the size of its text.
const TITLE: f32 = 60.0;
pub const TITLE_SIZE: f32 = 28.0;
/// A section header: room above it, and the line it is on.
const HEADER_ABOVE: f32 = 14.0;
const HEADER: f32 = 34.0;
/// Room between two cards.
const CARD_GAP: f32 = 4.0;
/// Room inside a card, left and right.
const PAD: f32 = 16.0;
/// The strips' heights.
const SETTING: f32 = 68.0;
const ENTRY: f32 = 44.0;
const FIELD: f32 = 56.0;
/// A paragraph's lines, and the room under the last.
const LINE: f32 = 20.0;
const TEXT_BELOW: f32 = 14.0;
/// A setting's name and help lines: top edge in the strip, height.
const NAME_LINE: (f32, f32) = (13.0, 22.0);
const HELP_LINE: (f32, f32) = (35.0, 19.0);
/// Room between text and the control on its right.
const GAP: f32 = 12.0;
/// Buttons and fields.
pub const CONTROL_HEIGHT: f32 = 32.0;
const BUTTON_PAD: f32 = 12.0;
const BUTTON_MIN: f32 = 80.0;
/// The switch.
pub const TOGGLE: (f32, f32) = (40.0, 20.0);
/// The recorder's keys: room around each, between them and inside the
/// field; the field's least width.
pub const CAP_PAD: f32 = 8.0;
pub const CAP_GAP: f32 = 4.0;
pub const KEYS_PAD: f32 = 6.0;
const KEYS_MIN: f32 = 160.0;
/// The × on an entry, square.
const REMOVE: f32 = 32.0;
/// Room between the options of a choice.
const CHOICE_GAP: f32 = 4.0;

/// Places every piece of `page` on a page `width` DIPs wide; `measure`
/// tells how wide a control's label is.
pub fn lay_out(page: &Page, width: f32, measure: &dyn Fn(&str) -> f32) -> Laid {
    let mut out = Out {
        marks: Vec::new(),
        y: MARGIN,
        left: MARGIN,
        width: (width - 2.0 * MARGIN).max(0.0),
        measure,
    };
    out.put(TITLE, What::Title(page.title.clone()));
    for section in &page.sections {
        out.section(section);
    }
    let height = out.y - CARD_GAP + MARGIN;
    Laid {
        marks: out.marks,
        height,
    }
}

/// The marks placed so far, and where the next goes.
struct Out<'a> {
    marks: Vec<Mark>,
    y: f32,
    left: f32,
    width: f32,
    measure: &'a dyn Fn(&str) -> f32,
}

impl Out<'_> {
    /// A mark across the page, `height` high, and the next below it.
    fn put(&mut self, height: f32, what: What) {
        let at = Area {
            x: self.left,
            y: self.y,
            width: self.width,
            height,
        };
        self.mark(at, what);
        self.y += height;
    }

    /// A mark at `at`; the card it is on, if any, says whether it is
    /// muted.
    fn mark(&mut self, at: Area, what: What) {
        self.marks.push(Mark {
            at,
            what,
            muted: false,
        });
    }

    fn section(&mut self, section: &Section) {
        if let Some(title) = &section.title {
            self.y += HEADER_ABOVE;
            self.put(HEADER, What::Header(title.clone()));
        }
        for card in &section.cards {
            self.card(card);
        }
    }

    /// The card behind its strips, as high as they are together.
    fn card(&mut self, card: &Card) {
        let at = self.marks.len();
        let top = self.y;
        let alarm = card.alarm;
        self.put(0.0, What::Card { alarm });
        for (index, strip) in card.strips.iter().enumerate() {
            // A paragraph reads on from the strip above it.
            if index > 0 && !matches!(strip, Strip::Text { .. }) {
                let line = Area {
                    x: self.left,
                    y: self.y,
                    width: self.width,
                    height: 1.0,
                };
                self.mark(line, What::Divider);
            }
            self.strip(strip);
        }
        self.marks[at].at.height = self.y - top;
        for mark in &mut self.marks[at..] {
            mark.muted = card.muted;
        }
        self.y += CARD_GAP;
    }

    fn strip(&mut self, strip: &Strip) {
        match strip {
            Strip::Setting {
                name,
                help,
                alarm,
                control,
            } => self.setting(name, help, *alarm, control.as_ref()),
            Strip::Entry { text, remove } => self.entry(text, *remove),
            Strip::Field {
                target,
                text,
                caret,
                selection,
                placeholder,
                add,
            } => {
                let field = What::Field {
                    text: text.clone(),
                    caret: *caret,
                    selection: selection.clone(),
                    placeholder: placeholder.clone(),
                    target: *target,
                };
                self.field(field, add)
            }
            Strip::Text { text } => self.text(text),
        }
    }

    fn text(&mut self, text: &str) {
        let width = (self.width - 2.0 * PAD).max(0.0);
        for line in wrap(text, width, self.measure) {
            let at = Area {
                x: self.left + PAD,
                y: self.y,
                width,
                height: LINE,
            };
            self.mark(at, What::Line(line));
            self.y += LINE;
        }
        self.y += TEXT_BELOW;
    }

    fn setting(&mut self, name: &str, help: &str, alarm: bool, control: Option<&Control>) {
        let right = self.left + self.width - PAD;
        let text_right = match control {
            Some(control) => {
                let (width, height) = self.size(control);
                let at = self.at(right - width, SETTING, width, height);
                for (place, what) in self.control_marks(control, at) {
                    self.mark(place, what);
                }
                at.x - GAP
            }
            None => right,
        };
        let text = |(top, height): (f32, f32)| Area {
            x: self.left + PAD,
            y: self.y + top,
            width: (text_right - self.left - PAD).max(0.0),
            height,
        };
        let (name_at, help_at) = (text(NAME_LINE), text(HELP_LINE));
        self.mark(name_at, What::Name(name.to_string()));
        let help = help.to_string();
        self.mark(help_at, What::Help { text: help, alarm });
        self.y += SETTING;
    }

    fn entry(&mut self, text: &str, remove: Target) {
        let right = self.left + self.width - PAD / 2.0;
        let cross = self.at(right - REMOVE, ENTRY, REMOVE, REMOVE);
        self.mark(cross, What::Remove(remove));
        let line = Area {
            x: self.left + PAD,
            y: self.y,
            width: (cross.x - GAP - self.left - PAD).max(0.0),
            height: ENTRY,
        };
        self.mark(line, What::Entry(text.to_string()));
        self.y += ENTRY;
    }

    fn field(&mut self, field: What, add: &Button) {
        let right = self.left + self.width - PAD;
        let width = self.button_width(&add.label);
        let button = self.at(right - width, FIELD, width, CONTROL_HEIGHT);
        let left = self.left + PAD;
        let at = self.at(
            left,
            FIELD,
            (button.x - 8.0 - left).max(0.0),
            CONTROL_HEIGHT,
        );
        self.mark(at, field);
        self.mark(button, button_mark(add));
        self.y += FIELD;
    }

    /// A control `width` by `height` at `x`, in the middle of a strip
    /// `strip` high that starts here.
    fn at(&self, x: f32, strip: f32, width: f32, height: f32) -> Area {
        Area {
            x,
            y: self.y + (strip - height) / 2.0,
            width,
            height,
        }
    }

    fn size(&self, control: &Control) -> (f32, f32) {
        match control {
            Control::Button(button) => (self.button_width(&button.label), CONTROL_HEIGHT),
            Control::Toggle { .. } => TOGGLE,
            Control::Keys { caps, .. } => (self.keys_width(caps), CONTROL_HEIGHT),
            Control::Choice(choices) => (self.choice_width(choices), CONTROL_HEIGHT),
        }
    }

    /// The marks of `control` at `at`: one, or one per option of a
    /// choice, side by side.
    fn control_marks(&self, control: &Control, at: Area) -> Vec<(Area, What)> {
        let what = match control {
            Control::Choice(choices) => return self.options(choices, at),
            Control::Button(button) => button_mark(button),
            Control::Toggle { target, on } => What::Toggle {
                on: *on,
                target: *target,
            },
            Control::Keys {
                target,
                caps,
                listening,
                prompt,
            } => What::Keys {
                caps: caps.clone(),
                listening: *listening,
                prompt: prompt.clone(),
                target: *target,
            },
        };
        vec![(at, what)]
    }

    /// The options of a choice from the left of `at`, each as wide as a
    /// button with its label.
    fn options(&self, choices: &[Choice], at: Area) -> Vec<(Area, What)> {
        let mut x = at.x;
        let mut marks = Vec::new();
        for choice in choices {
            let width = self.button_width(&choice.label);
            let what = What::Choice {
                label: choice.label.clone(),
                on: choice.on,
                target: choice.target,
            };
            marks.push((Area { x, width, ..at }, what));
            x += width + CHOICE_GAP;
        }
        marks
    }

    /// A choice: each option as wide as a button with its label.
    fn choice_width(&self, choices: &[Choice]) -> f32 {
        let each = choices
            .iter()
            .map(|choice| self.button_width(&choice.label));
        let gaps = choices.len().saturating_sub(1) as f32 * CHOICE_GAP;
        each.sum::<f32>() + gaps
    }

    fn button_width(&self, label: &str) -> f32 {
        ((self.measure)(label) + 2.0 * BUTTON_PAD).max(BUTTON_MIN)
    }

    /// The recorder: each key a cap, side by side.
    fn keys_width(&self, caps: &[String]) -> f32 {
        let each = caps.iter().map(|cap| (self.measure)(cap) + 2.0 * CAP_PAD);
        let gaps = caps.len().saturating_sub(1) as f32 * CAP_GAP;
        (each.sum::<f32>() + gaps + 2.0 * KEYS_PAD).max(KEYS_MIN)
    }
}

fn button_mark(button: &Button) -> What {
    What::Button {
        label: button.label.clone(),
        target: button.target,
    }
}

/// `text` broken at spaces into lines no wider than `width`, by
/// `measure`, and at its line breaks; a word wider than a line has a line
/// of its own, cut short where it is drawn.
pub fn wrap(text: &str, width: f32, measure: &dyn Fn(&str) -> f32) -> Vec<String> {
    let mut lines = Vec::new();
    for paragraph in text.split('\n') {
        let mut line = String::new();
        for word in paragraph.split(' ') {
            let longer = format!("{line} {word}");
            match line.is_empty() {
                true => line = word.to_string(),
                false if measure(&longer) <= width => line = longer,
                false => lines.push(std::mem::replace(&mut line, word.to_string())),
            }
        }
        lines.push(line);
    }
    lines
}

/// The furthest a page `content` DIPs high scrolls in a view `view` high.
pub fn max_scroll(content: f32, view: f32) -> f32 {
    (content - view).max(0.0)
}

/// Room kept between a control brought into view and the view's edge.
const REVEAL_MARGIN: f32 = 12.0;

/// `scroll`, moved as little as it takes to bring `area` into a view
/// `view` high; up to the top of the page when it fits in the view with
/// the top.
pub fn reveal(scroll: f32, area: Area, view: f32) -> f32 {
    let (top, bottom) = (area.y - REVEAL_MARGIN, area.bottom() + REVEAL_MARGIN);
    if top < scroll {
        return match bottom <= view {
            true => 0.0,
            false => top.max(0.0),
        };
    }
    // Down only as far as the control's bottom needs.
    scroll.max(bottom - view)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every letter 10 DIPs wide.
    fn measure(text: &str) -> f32 {
        text.chars().count() as f32 * 10.0
    }

    fn button(label: &str, target: Target) -> Button {
        Button {
            label: label.to_string(),
            target,
        }
    }

    fn setting(name: &str, control: Option<Control>) -> Strip {
        Strip::Setting {
            name: name.to_string(),
            help: format!("{name} help"),
            alarm: false,
            control,
        }
    }

    fn entry(text: &str, index: usize) -> Strip {
        Strip::Entry {
            text: text.to_string(),
            remove: Target::Remove(List::RankLower, index),
        }
    }

    /// A page with a header, a card of a toggle, and a card of a list.
    fn page() -> Page {
        let toggle = Control::Toggle {
            target: Target::StartWithWindows,
            on: true,
        };
        let pick = Control::Button(button("Add folder…", Target::Pick(List::RankLower)));
        let field = Strip::Field {
            target: Target::Name(List::RankLower),
            text: String::new(),
            caret: 0,
            selection: 0..0,
            placeholder: "Folder name".to_string(),
            add: button("Add", Target::AddName(List::RankLower)),
        };
        Page {
            title: "Settings".to_string(),
            sections: vec![Section {
                title: Some("Launcher".to_string()),
                cards: vec![
                    Card {
                        alarm: false,
                        muted: false,
                        strips: vec![setting("Start with Windows", Some(toggle))],
                    },
                    Card {
                        alarm: false,
                        muted: true,
                        strips: vec![
                            setting("Rank lower", Some(pick)),
                            entry("target", 0),
                            entry("vendor", 1),
                            field,
                        ],
                    },
                ],
            }],
        }
    }

    fn laid() -> Laid {
        lay_out(&page(), 600.0, &measure)
    }

    fn marks(laid: &Laid, wanted: fn(&What) -> bool) -> Vec<Area> {
        let found = laid.marks.iter().filter(|mark| wanted(&mark.what));
        found.map(|mark| mark.at).collect()
    }

    fn area(x: f32, y: f32, width: f32, height: f32) -> Area {
        Area {
            x,
            y,
            width,
            height,
        }
    }

    #[test]
    fn the_title_then_a_header_then_cards_across_the_page_inside_its_margins() {
        let laid = laid();
        assert_eq!(laid.marks[0].what, What::Title("Settings".to_string()));
        assert_eq!(laid.marks[0].at, area(24.0, 24.0, 552.0, 60.0));
        assert_eq!(laid.marks[1].what, What::Header("Launcher".to_string()));
        assert_eq!(laid.marks[1].at, area(24.0, 98.0, 552.0, 34.0));
        let cards = marks(&laid, |what| matches!(what, What::Card { .. }));
        assert_eq!(cards[0], area(24.0, 132.0, 552.0, 68.0));
        let list = 68.0 + 44.0 + 44.0 + 56.0;
        assert_eq!(cards[1], area(24.0, 204.0, 552.0, list));
        let muted: Vec<&What> = laid
            .marks
            .iter()
            .filter(|mark| mark.muted)
            .map(|mark| &mark.what)
            .collect();
        assert_eq!(
            muted[0],
            &What::Card { alarm: false },
            "the card and all on it"
        );
        assert_eq!(
            muted.len(),
            1 + 3 + 2 * 2 + 2 + 3,
            "the card, a setting, two entries, a field, three hairlines"
        );
        let after = laid
            .marks
            .iter()
            .position(|mark| mark.muted)
            .expect("muted marks");
        assert!(
            laid.marks[..after].iter().all(|mark| !mark.muted),
            "the title, header and first card are not"
        );
        assert_eq!(laid.height, 204.0 + list + 24.0);
    }

    #[test]
    fn strips_of_a_card_are_parted_by_hairlines() {
        let laid = laid();
        let lines = marks(&laid, |what| *what == What::Divider);
        let tops: Vec<f32> = lines.iter().map(|line| line.y).collect();
        assert_eq!(tops, [272.0, 316.0, 360.0]);
        assert!(
            lines
                .iter()
                .all(|line| line.x == 24.0 && line.width == 552.0)
        );
    }

    #[test]
    fn a_control_sits_on_the_right_in_the_middle_of_its_strip() {
        let laid = laid();
        let toggle = laid.area(Target::StartWithWindows).expect("the switch");
        assert_eq!(toggle, area(576.0 - 16.0 - 40.0, 132.0 + 24.0, 40.0, 20.0));
        let pick = laid
            .area(Target::Pick(List::RankLower))
            .expect("the button");
        let width = 11.0 * 10.0 + 24.0;
        assert_eq!(pick, area(560.0 - width, 204.0 + 18.0, width, 32.0));
    }

    #[test]
    fn a_settings_name_and_help_stop_short_of_its_control() {
        let laid = laid();
        let names = marks(&laid, |what| matches!(what, What::Name(_)));
        assert_eq!(
            names[0],
            area(40.0, 132.0 + 13.0, 520.0 - 12.0 - 40.0, 22.0)
        );
        let helps = marks(&laid, |what| matches!(what, What::Help { .. }));
        assert_eq!(helps[0], area(40.0, 132.0 + 35.0, 468.0, 19.0));
    }

    #[test]
    fn a_setting_without_a_control_has_the_card_for_its_text() {
        let page = Page {
            title: String::new(),
            sections: vec![Section {
                title: None,
                cards: vec![Card {
                    alarm: true,
                    muted: false,
                    strips: vec![setting("Alone", None)],
                }],
            }],
        };
        let laid = lay_out(&page, 600.0, &measure);
        let names = marks(&laid, |what| matches!(what, What::Name(_)));
        assert_eq!(names[0].width, 552.0 - 32.0);
        assert!(laid.marks.contains(&Mark {
            at: area(24.0, 84.0, 552.0, 68.0),
            what: What::Card { alarm: true },
            muted: false,
        }));
    }

    #[test]
    fn an_entry_has_its_cross_at_the_right_edge() {
        let laid = laid();
        let cross = laid.area(Target::Remove(List::RankLower, 1)).expect("a ×");
        assert_eq!(cross, area(576.0 - 8.0 - 32.0, 316.0 + 6.0, 32.0, 32.0));
        let entries = marks(&laid, |what| matches!(what, What::Entry(_)));
        assert_eq!(entries[1], area(40.0, 316.0, cross.x - 12.0 - 40.0, 44.0));
    }

    #[test]
    fn a_field_fills_the_strip_up_to_its_button() {
        let laid = laid();
        let add = laid.area(Target::AddName(List::RankLower)).expect("Add");
        assert_eq!(
            add,
            area(560.0 - 80.0, 360.0 + 12.0, 80.0, 32.0),
            "at least 80 wide"
        );
        let field = laid.area(Target::Name(List::RankLower)).expect("the field");
        assert_eq!(field, area(40.0, 372.0, 480.0 - 8.0 - 40.0, 32.0));
    }

    #[test]
    fn a_choice_lays_its_options_out_side_by_side_on_the_right() {
        let option = |label: &str, language, on| Choice {
            label: label.to_string(),
            target: Target::Language(language),
            on,
        };
        let choice = Control::Choice(vec![
            option("English", Language::English, true),
            option("Русский", Language::Russian, false),
        ]);
        let page = Page {
            title: String::new(),
            sections: vec![Section {
                title: None,
                cards: vec![Card {
                    alarm: false,
                    muted: false,
                    strips: vec![setting("Language", Some(choice))],
                }],
            }],
        };
        let laid = lay_out(&page, 600.0, &measure);
        let english = laid
            .area(Target::Language(Language::English))
            .expect("English");
        let russian = laid
            .area(Target::Language(Language::Russian))
            .expect("Russian");
        let top = MARGIN + TITLE + (SETTING - CONTROL_HEIGHT) / 2.0;
        assert_eq!(russian, area(600.0 - 24.0 - 16.0 - 94.0, top, 94.0, 32.0));
        assert_eq!(english, area(russian.x - 4.0 - 94.0, top, 94.0, 32.0));
        let targets = [Language::English, Language::Russian].map(Target::Language);
        assert_eq!(laid.targets(), targets);
        let on: Vec<bool> = laid
            .marks
            .iter()
            .filter_map(|mark| match mark.what {
                What::Choice { on, .. } => Some(on),
                _ => None,
            })
            .collect();
        assert_eq!(on, [true, false]);
        let name = laid
            .marks
            .iter()
            .find(|mark| mark.what == What::Name("Language".into()));
        let name = name.expect("the setting's name").at;
        assert_eq!(
            name.x + name.width,
            english.x - 12.0,
            "its text stops short of it"
        );
    }

    #[test]
    fn the_recorder_is_as_wide_as_its_keys_and_at_least_160() {
        let keys = |caps: &[&str]| Control::Keys {
            target: Target::Hotkey,
            caps: caps.iter().map(|cap| cap.to_string()).collect(),
            listening: false,
            prompt: String::new(),
        };
        let width = |control| {
            let page = Page {
                title: String::new(),
                sections: vec![Section {
                    title: None,
                    cards: vec![Card {
                        alarm: false,
                        muted: false,
                        strips: vec![setting("Hotkey", Some(control))],
                    }],
                }],
            };
            let laid = lay_out(&page, 600.0, &measure);
            laid.area(Target::Hotkey).expect("the recorder").width
        };
        assert_eq!(width(keys(&["F4"])), 160.0);
        let wide = ["Win", "Ctrl", "Alt", "Shift", "F12"];
        let caps: f32 = [30.0, 40.0, 30.0, 50.0, 30.0]
            .iter()
            .map(|text| text + 16.0)
            .sum();
        assert_eq!(width(keys(&wide)), caps + 4.0 * 4.0 + 12.0);
    }

    #[test]
    fn a_point_on_a_control_hits_it_and_one_beside_it_hits_nothing() {
        let laid = laid();
        for target in laid.targets() {
            let (x, y) = laid.area(target).expect("placed").middle();
            assert_eq!(laid.hit(x, y), Some(target), "{target:?}");
        }
        let toggle = laid.area(Target::StartWithWindows).expect("the switch");
        assert_eq!(laid.hit(toggle.x - 1.0, toggle.y + 1.0), None);
        assert_eq!(laid.hit(toggle.x, toggle.y), Some(Target::StartWithWindows));
        assert_eq!(laid.hit(toggle.x + 40.0, toggle.y), None);
        assert_eq!(laid.hit(30.0, 140.0), None, "the card's own space");
        assert_eq!(laid.hit(-5.0, -5.0), None);
    }

    #[test]
    fn tab_goes_through_the_controls_top_to_bottom_left_to_right() {
        let order = [
            Target::StartWithWindows,
            Target::Pick(List::RankLower),
            Target::Remove(List::RankLower, 0),
            Target::Remove(List::RankLower, 1),
            Target::Name(List::RankLower),
            Target::AddName(List::RankLower),
        ];
        assert_eq!(laid().targets(), order);
        assert_eq!(laid().area(Target::Hotkey), None);
    }

    #[test]
    fn a_narrow_page_keeps_its_controls_and_gives_up_text() {
        let laid = lay_out(&page(), 200.0, &measure);
        let pick = laid
            .area(Target::Pick(List::RankLower))
            .expect("the button");
        assert_eq!(pick.x + pick.width, 200.0 - 24.0 - 16.0);
        let names = marks(&laid, |what| matches!(what, What::Name(_)));
        assert_eq!(names[1].width, 0.0);
        let narrowest = lay_out(&page(), 10.0, &measure);
        assert_eq!(narrowest.marks[0].at.width, 0.0);
    }

    #[test]
    fn text_breaks_into_lines_that_fit_at_spaces() {
        let lines = wrap("one two three four", 75.0, &measure);
        assert_eq!(lines, ["one two", "three", "four"]);
        assert_eq!(wrap("one two", 70.0, &measure), ["one two"], "just fits");
        assert_eq!(wrap("one two", 69.0, &measure), ["one", "two"]);
    }

    #[test]
    fn a_word_wider_than_a_line_has_a_line_of_its_own() {
        let lines = wrap(r"a C:\very\long\path b", 50.0, &measure);
        assert_eq!(lines, ["a", r"C:\very\long\path", "b"]);
    }

    #[test]
    fn a_line_break_in_the_text_starts_a_line() {
        assert_eq!(
            wrap("one\ntwo three", 1000.0, &measure),
            ["one", "two three"]
        );
        assert_eq!(wrap("", 100.0, &measure), [""]);
    }

    #[test]
    fn a_paragraph_on_a_card_takes_a_line_each_and_no_hairline_above() {
        let page = Page {
            title: String::new(),
            sections: vec![Section {
                title: None,
                cards: vec![Card {
                    alarm: true,
                    muted: false,
                    strips: vec![
                        setting("Problem", None),
                        Strip::Text {
                            text: "one two three".to_string(),
                        },
                    ],
                }],
            }],
        };
        let laid = lay_out(&page, 24.0 * 2.0 + 16.0 * 2.0 + 75.0, &measure);
        let lines: Vec<(String, Area)> = laid
            .marks
            .iter()
            .filter_map(|mark| match &mark.what {
                What::Line(text) => Some((text.clone(), mark.at)),
                _ => None,
            })
            .collect();
        let top = 84.0 + 68.0;
        assert_eq!(
            lines[0],
            ("one two".to_string(), area(40.0, top, 75.0, 20.0))
        );
        assert_eq!(
            lines[1],
            ("three".to_string(), area(40.0, top + 20.0, 75.0, 20.0))
        );
        assert!(!laid.marks.iter().any(|mark| mark.what == What::Divider));
        let card = laid
            .marks
            .iter()
            .find(|mark| matches!(mark.what, What::Card { .. }));
        assert_eq!(card.expect("a card").at.height, 68.0 + 2.0 * 20.0 + 14.0);
    }

    #[test]
    fn a_page_scrolls_as_far_as_its_content_outgrows_the_view() {
        assert_eq!(max_scroll(900.0, 600.0), 300.0);
        assert_eq!(max_scroll(500.0, 600.0), 0.0);
    }

    #[test]
    fn revealing_a_control_scrolls_as_little_as_it_takes() {
        let control = area(0.0, 500.0, 100.0, 32.0);
        assert_eq!(reveal(0.0, control, 600.0), 0.0, "in view");
        assert_eq!(reveal(0.0, control, 520.0), 532.0 + 12.0 - 520.0, "below");
        assert_eq!(reveal(600.0, control, 400.0), 488.0, "above");
        let top = area(0.0, 4.0, 10.0, 10.0);
        assert_eq!(reveal(100.0, top, 400.0), 0.0, "never above the page");
        assert_eq!(
            reveal(0.0, area(0.0, 12.0, 10.0, 10.0), 400.0),
            0.0,
            "just in view"
        );
        assert_eq!(
            reveal(0.0, area(0.0, 0.0, 10.0, 378.0), 400.0),
            0.0,
            "just fits"
        );
    }

    #[test]
    fn a_control_in_view_keeps_the_scroll_however_near_an_edge() {
        let at_the_margin = area(0.0, 112.0, 10.0, 10.0);
        assert_eq!(
            reveal(100.0, at_the_margin, 300.0),
            100.0,
            "just in view, below the top"
        );
        let at_the_bottom = area(0.0, 378.0, 10.0, 10.0);
        assert_eq!(
            reveal(100.0, at_the_bottom, 300.0),
            100.0,
            "just in view, above the bottom"
        );
        let inside_the_margin = area(0.0, 105.0, 10.0, 10.0);
        assert_eq!(
            reveal(100.0, inside_the_margin, 400.0),
            0.0,
            "too near the top: the top"
        );
    }

    #[test]
    fn revealing_a_control_near_the_top_shows_the_top_of_the_page() {
        let near_top = area(0.0, 150.0, 100.0, 32.0);
        assert_eq!(reveal(400.0, near_top, 300.0), 0.0);
        assert_eq!(
            reveal(400.0, near_top, 194.0),
            0.0,
            "just fits under the top"
        );
        assert_eq!(reveal(400.0, near_top, 193.0), 138.0, "does not");
    }
}
