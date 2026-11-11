//! What the settings window shows and how it answers the user, apart
//! from Win32: the settings as config.toml writes them, the recorder, the
//! name fields, the focus and the pointer. Each input gives what the
//! window must do about it, if anything: write a change to the file, open
//! the folder picker, switch starting with Windows, open the file, close.
//!
//! The page is built from a list of sections, each from the panel: the
//! updates section is one entry near the top, for a newer release, and
//! one near the end, for the switch alone.

use std::collections::HashMap;

use hyalo_core::config::{Change, List, Problem, Why, Written};
use hyalo_core::text::{Language, Text};
use hyalo_core::update::{Progress, Update, Version};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    VIRTUAL_KEY, VK_ESCAPE, VK_RETURN, VK_SPACE, VK_TAB,
};

use crate::field::{self, Edit, Edited, Field, Key};
use crate::keys::Held;
use crate::page::{self, Button, Card, Choice, Control, Laid, Page, Section, Strip, Target};
use crate::recorder::{Heard, Mods, Recorder};

/// What the window must do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Effect {
    /// Write the change to config.toml; a new hotkey is registered first.
    Change(Change),
    /// Open the folder picker, and add what is picked to the list.
    Pick(List),
    /// Start with Windows, or stop.
    StartWithWindows(bool),
    /// Open config.toml in an editor.
    OpenConfig,
    /// Put this text on the clipboard: what was copied or cut from a
    /// field.
    Copy(String),
    /// Paste the clipboard's text into the field with the focus.
    Paste,
    /// Install the newer release.
    Install,
    Close,
}

/// What the updates section shows: the version that runs, and the newer
/// release hyalo tells of, if any.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Versions {
    pub current: Version,
    pub update: Option<Update>,
}

/// How a control looks now.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Look {
    pub hovered: bool,
    pub pressed: bool,
    /// It has the focus: a field shows its caret.
    pub focused: bool,
    /// The focus came from the keyboard, so it is ringed.
    pub ring: bool,
    pub enabled: bool,
}

/// Where a message about a change shows: the help line of its setting.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Spot {
    Hotkey,
    Language,
    List(List),
    Updates,
}

/// The page's sections, top to bottom; each is left out when it has
/// nothing to show.
const SECTIONS: [fn(&Panel) -> Option<Section>; 7] = [
    Panel::problem,
    Panel::news,
    Panel::launcher,
    Panel::folders,
    Panel::search,
    Panel::updates,
    Panel::file,
];

/// How far one notch of the mouse wheel scrolls, in DIPs.
const WHEEL_STEP: f32 = 48.0;

/// One notch, as Windows counts the wheel.
const WHEEL_DELTA: f32 = 120.0;

#[derive(Debug, Default)]
pub struct Panel {
    /// The last good settings the file held; `None` before it held any.
    written: Option<Written>,
    /// What is wrong with the file now, if anything: nothing is changed
    /// until it is fixed.
    problem: Option<Problem>,
    /// Whether hyalo starts with Windows; `None` where it may not.
    starts: Option<bool>,
    recorder: Recorder,
    /// What is typed in each list's name field.
    names: HashMap<List, Field>,
    /// What went wrong with the last change, and where it shows.
    note: Option<(Spot, Why)>,
    focus: Option<Target>,
    /// Whether the focus is ringed: it came from the keyboard.
    ring: bool,
    hover: Option<Target>,
    pressed: Option<Target>,
    /// How far the page is scrolled, in DIPs.
    scroll: f32,
    /// The language Windows is shown in, which a file without one speaks.
    windows: Language,
    /// The versions, where hyalo updates itself; `None` shows no updates
    /// section.
    updates: Option<Versions>,
}

impl Panel {
    /// The panel for a file that holds `file`, for starting with Windows
    /// as `starts` says, speaking the file's language or `windows`, the
    /// language Windows is shown in.
    pub fn new(file: Result<Written, Problem>, starts: Option<bool>, windows: Language) -> Panel {
        let mut panel = Panel {
            starts,
            windows,
            ..Panel::default()
        };
        panel.reload(file);
        panel
    }

    /// What the page says things in: the language the file names, or
    /// Windows'; while the file is broken, the last good one's.
    pub fn language(&self) -> Language {
        let named = self.written.as_ref().and_then(|written| written.language);
        named.unwrap_or(self.windows)
    }

    /// The window's title.
    pub fn title(&self) -> &'static str {
        self.language().say(Text::WindowTitle)
    }

    /// `text` in the page's language.
    fn say(&self, text: Text) -> String {
        self.language().say(text).to_string()
    }

    /// What the page shows now.
    pub fn page(&self) -> Page {
        Page {
            title: self.say(Text::Settings),
            sections: SECTIONS
                .iter()
                .filter_map(|section| section(self))
                .collect(),
        }
    }

    pub fn look(&self, target: Target) -> Look {
        let focused = self.focus == Some(target);
        Look {
            hovered: self.hover == Some(target),
            pressed: self.pressed == Some(target),
            focused,
            ring: focused && self.ring,
            enabled: self.enabled(target),
        }
    }

    pub fn scroll(&self) -> f32 {
        self.scroll
    }

    pub fn listening(&self) -> bool {
        self.recorder.listening()
    }

    /// The file holds something new: good settings, shown at once, or a
    /// problem, shown until it is fixed. What is typed in a field stays.
    pub fn reload(&mut self, file: Result<Written, Problem>) {
        match file {
            Ok(written) => {
                self.written = Some(written);
                self.problem = None;
            }
            Err(problem) => self.problem = Some(problem),
        }
        self.settle_focus();
    }

    /// Whether hyalo starts with Windows now.
    pub fn set_starts(&mut self, starts: Option<bool>) {
        self.starts = starts;
        self.settle_focus();
    }

    /// What is known of the versions now.
    pub fn set_updates(&mut self, updates: Option<Versions>) {
        self.updates = updates;
        self.settle_focus();
    }

    /// `change` is in the file, which now holds `written`, or held it
    /// already (`None`): a name added leaves its field.
    pub fn done(&mut self, change: &Change, written: Option<Written>) {
        self.note = None;
        if let Change::Add(list, entry) = change
            && let Some(name) = self.names.get_mut(list)
            && name.text().trim() == entry
        {
            name.clear();
        }
        if let Some(written) = written {
            self.reload(Ok(written));
        }
    }

    /// `change` was not made, for the reason `why`, told by its setting.
    pub fn refused(&mut self, change: &Change, why: Why) {
        let spot = match change {
            Change::Hotkey(_) => Spot::Hotkey,
            Change::Language(_) => Spot::Language,
            Change::CheckForUpdates(_) => Spot::Updates,
            Change::Add(list, _) | Change::Remove(list, _) => Spot::List(*list),
        };
        self.note = Some((spot, why));
    }

    /// The pointer is over `target`, or no control; whether that changed
    /// the look.
    pub fn hover(&mut self, target: Option<Target>) -> bool {
        let target = target.filter(|&target| self.enabled(target));
        std::mem::replace(&mut self.hover, target) != target
    }

    /// The button went down over `target`: it takes the focus, without a
    /// ring; a recorder that listens elsewhere stops.
    pub fn press(&mut self, target: Option<Target>) {
        let target = target.filter(|&target| self.enabled(target));
        if target != Some(Target::Hotkey) {
            self.recorder.stop();
        }
        self.pressed = target;
        self.focus = target;
        self.ring = false;
    }

    /// The button went up over `target`: a click, when it went down there.
    pub fn release(&mut self, target: Option<Target>) -> Option<Effect> {
        let pressed = self.pressed.take()?;
        (Some(pressed) == target).then(|| self.activate(pressed))?
    }

    /// The key `key` went down with `mods` held, on the page `laid` seen
    /// in a view `view` DIPs high.
    pub fn key(&mut self, key: u16, mods: Mods, laid: &Laid, view: f32) -> Option<Effect> {
        if self.recorder.listening() {
            return self.record(key, mods);
        }
        match self.focus {
            _ if key == VK_TAB.0 => {
                self.tab(laid, view, mods.shift);
                None
            }
            _ if key == VK_ESCAPE.0 => Some(Effect::Close),
            Some(Target::Name(list)) if key == VK_RETURN.0 => self.activate(Target::AddName(list)),
            Some(Target::Name(_)) => self.edit(key, mods),
            Some(target) if key == VK_RETURN.0 || key == VK_SPACE.0 => self.activate(target),
            _ => None,
        }
    }

    /// A key went up, and `mods` are held now.
    pub fn key_up(&mut self, mods: Mods) {
        self.recorder.held(mods);
    }

    /// Types one UTF-16 unit into the field with the focus; whether it
    /// changed what the field holds.
    pub fn char(&mut self, unit: u16) -> bool {
        self.typing().is_some_and(|name| name.type_unit(unit))
    }

    /// Pastes `text` into the field with the focus; whether it changed
    /// what the field holds.
    pub fn paste(&mut self, text: &str) -> bool {
        self.typing().is_some_and(|name| name.paste(text))
    }

    /// Puts the caret of the field with the focus before the character
    /// `at`, or with `extend` selects up to there: a click or a drag in
    /// it. Whether that changed it.
    pub fn place(&mut self, at: usize, extend: bool) -> bool {
        self.typing()
            .is_some_and(|name| name.place(at, extend) != Edited::Nothing)
    }

    /// Selects the word at `at` in the field with the focus: a
    /// double-click. Whether that changed it.
    pub fn select_word(&mut self, at: usize) -> bool {
        self.typing()
            .is_some_and(|name| name.select_word(at) != Edited::Nothing)
    }

    /// Selects all of the field with the focus: a triple-click. Whether
    /// that changed it.
    pub fn select_all(&mut self) -> bool {
        self.typing()
            .is_some_and(|name| name.apply(Edit::SelectAll) != Edited::Nothing)
    }

    /// The list whose name field the button went down in, while it is
    /// down: moving the pointer selects in it.
    pub fn dragging(&self) -> Option<List> {
        match self.pressed {
            Some(Target::Name(list)) => Some(list),
            _ => None,
        }
    }

    /// The name field with the focus, when it may be typed in.
    fn typing(&mut self) -> Option<&mut Field> {
        let Some(Target::Name(list)) = self.focus else {
            return None;
        };
        self.enabled(Target::Name(list))
            .then(|| self.names.entry(list).or_default())
    }

    /// A key in the name field with the focus: it edits the field, or
    /// copies or cuts from it, or pastes into it.
    fn edit(&mut self, key: u16, mods: Mods) -> Option<Effect> {
        let held = Held {
            ctrl: mods.ctrl,
            shift: mods.shift,
        };
        let pressed = field::key(VIRTUAL_KEY(key), held)?;
        let name = self.typing()?;
        match pressed {
            Key::Edit(edit) => {
                name.apply(edit);
                None
            }
            Key::Copy => Some(name.selected().to_string())
                .filter(|selected| !selected.is_empty())
                .map(Effect::Copy),
            Key::Cut => name.cut().map(Effect::Copy),
            Key::Paste => Some(Effect::Paste),
        }
    }

    /// The hotkey itself was pressed, which Windows sends hyalo instead of
    /// the keys: while the recorder listens, that is the hotkey it has,
    /// and nothing changes. Whether the recorder took it.
    pub fn hotkey_pressed(&mut self) -> bool {
        let listening = self.recorder.listening();
        self.recorder.stop();
        if listening {
            self.note = None;
        }
        listening
    }

    /// The wheel turned by `delta`, as Windows counts it, over a page
    /// `content` DIPs high in a view `view` high.
    pub fn wheel(&mut self, delta: i32, content: f32, view: f32) {
        let scroll = self.scroll - delta as f32 / WHEEL_DELTA * WHEEL_STEP;
        self.scroll = scroll.clamp(0.0, page::max_scroll(content, view));
    }

    /// Keeps the scroll within a page `content` high in a view `view`
    /// high, as after the window or the page changed size.
    pub fn fit(&mut self, content: f32, view: f32) {
        self.scroll = self.scroll.clamp(0.0, page::max_scroll(content, view));
    }

    /// The key `key` went down with `mods` held, for the recorder; what
    /// it heard, as `key` says.
    pub fn record(&mut self, key: u16, mods: Mods) -> Option<Effect> {
        let heard = self.recorder.key(key, mods);
        self.note = None;
        match heard {
            Heard::Nothing | Heard::Cancelled => None,
            Heard::Close => Some(Effect::Close),
            Heard::Wrong(why) => {
                self.note = Some((Spot::Hotkey, Why::Hotkey(why)));
                None
            }
            Heard::Hotkey(hotkey) => {
                let current = self.written.as_ref().map(|written| written.hotkey);
                (current != Some(hotkey)).then_some(Effect::Change(Change::Hotkey(hotkey)))
            }
        }
    }

    /// Moves the focus to the next control that may be used, or the one
    /// before, round the page; the ring shows and the control comes into
    /// view.
    fn tab(&mut self, laid: &Laid, view: f32, back: bool) {
        let mut order: Vec<Target> = laid.targets();
        order.retain(|&target| self.enabled(target));
        if back {
            order.reverse();
        }
        let at = self
            .focus
            .and_then(|focus| order.iter().position(|&target| target == focus));
        // `at` is a place in `order`, which so is not empty.
        let next = at.map_or(0, |at| (at + 1) % order.len());
        self.focus = order.get(next).copied();
        self.ring = true;
        if let Some(area) = self.focus.and_then(|focus| laid.area(focus)) {
            self.scroll = page::reveal(self.scroll, area, view);
        }
    }

    /// What using `target` does, when it may be used.
    fn activate(&mut self, target: Target) -> Option<Effect> {
        if !self.enabled(target) {
            return None;
        }
        self.note = None;
        match target {
            Target::Hotkey => self.toggle_recorder(),
            Target::Remove(list, at) => self.removal(list, at),
            Target::AddName(list) => self.addition(list),
            target => self.plain(target),
        }
    }

    /// The recorder starts listening, or, listening, stops.
    fn toggle_recorder(&mut self) -> Option<Effect> {
        match self.recorder.listening() {
            true => self.recorder.stop(),
            false => self.recorder.listen(),
        }
        None
    }

    /// Taking out the entry at `at` of `list`, by its text.
    fn removal(&self, list: List, at: usize) -> Option<Effect> {
        let entry = self.entries(list).get(at)?.clone();
        Some(Effect::Change(Change::Remove(list, entry)))
    }

    /// Adding the name typed in `list`'s field, if there is one.
    fn addition(&self, list: List) -> Option<Effect> {
        let name = self.names.get(&list)?.text().trim().to_string();
        (!name.is_empty()).then_some(Effect::Change(Change::Add(list, name)))
    }

    /// What the controls that only ask the window for something do; a
    /// field only takes the focus.
    fn plain(&self, target: Target) -> Option<Effect> {
        match target {
            Target::Fix | Target::OpenConfig => Some(Effect::OpenConfig),
            Target::StartWithWindows => self.starts.map(|on| Effect::StartWithWindows(!on)),
            Target::Install => Some(Effect::Install),
            Target::CheckUpdates => self
                .written
                .as_ref()
                .map(|written| Effect::Change(Change::CheckForUpdates(!written.check_for_updates))),
            Target::Pick(list) => Some(Effect::Pick(list)),
            Target::Language(language) => {
                (language != self.language()).then_some(Effect::Change(Change::Language(language)))
            }
            _ => None,
        }
    }

    /// Whether `target` is on the page and may be used: while the file
    /// has a problem, only the way to the file and Install, which is no
    /// setting; Install not while it installs.
    fn enabled(&self, target: Target) -> bool {
        let free = match target {
            Target::Fix | Target::OpenConfig => true,
            Target::Install => self
                .update()
                .is_some_and(|update| update.progress != Progress::Installing),
            _ => self.problem.is_none(),
        };
        self.has(target) && free
    }

    /// Whether the page has `target`.
    fn has(&self, target: Target) -> bool {
        match target {
            Target::Fix => self.problem.is_some(),
            Target::OpenConfig => true,
            Target::Install => self.update().is_some(),
            Target::Remove(list, at) => at < self.entries(list).len(),
            setting => self.written.is_some() && self.shows(setting),
        }
    }

    /// Whether the page, showing good settings, has the control of the
    /// setting `target`: Add folder… only for the lists of folders.
    fn shows(&self, target: Target) -> bool {
        match target {
            Target::StartWithWindows => self.starts.is_some(),
            Target::Name(list) | Target::AddName(list) => list != List::Exclude,
            Target::Pick(list) => list != List::Search,
            Target::CheckUpdates => self.updates.is_some(),
            _ => true,
        }
    }

    /// The newer release the page tells of, if any.
    fn update(&self) -> Option<&Update> {
        self.updates.as_ref()?.update.as_ref()
    }

    /// A focus on a control the page no longer has moves to the entry
    /// before it in its list, or to the list's Add folder…, or goes.
    fn settle_focus(&mut self) {
        let Some(focus) = self.focus.filter(|&focus| !self.has(focus)) else {
            return;
        };
        self.focus = match focus {
            Target::Remove(list, at) => {
                let entries = self.entries(list).len();
                let before = Target::Remove(list, at.min(entries).saturating_sub(1));
                Some(if entries > 0 {
                    before
                } else {
                    Target::Pick(list)
                })
            }
            _ => None,
        }
        .filter(|&focus| self.has(focus));
    }

    fn entries(&self, list: List) -> &[String] {
        self.written
            .as_ref()
            .map_or(&[], |written| written.list(list))
    }

    /// The help line of the setting at `spot`: what went wrong there, as
    /// a warning, or `help`.
    fn help(&self, spot: Spot, help: Text) -> (String, bool) {
        self.noted(spot, self.say(help))
    }

    /// The same, for help said already.
    fn noted(&self, spot: Spot, help: String) -> (String, bool) {
        match &self.note {
            Some((noted, why)) if *noted == spot => (why.said(self.language()), true),
            _ => (help, false),
        }
    }

    /// The problem in the file, with the way to fix it.
    fn problem(&self) -> Option<Section> {
        let problem = self.problem.as_ref()?;
        let help = match self.written {
            Some(_) => Text::KeepsUntilFixed,
            None => Text::DefaultsUntilFixed,
        };
        let fix = Control::Button(button(&self.say(Text::OpenConfig), Target::Fix));
        let title = setting(&self.say(Text::Problem), (self.say(help), false), Some(fix));
        let (language, line) = (self.language(), problem.line.to_string());
        let text = language.fill(Text::Line, &[&line, &problem.why.said(language)]);
        Some(Section {
            title: None,
            cards: vec![card(true, false, vec![title, Strip::Text { text }])],
        })
    }

    /// The hotkey, starting with Windows, and the language.
    fn launcher(&self) -> Option<Section> {
        let written = self.written.as_ref()?;
        let listening = self.recorder.listening();
        let help = match listening {
            true => Text::Listening,
            false => Text::HotkeyHelp,
        };
        let keys = Control::Keys {
            target: Target::Hotkey,
            caps: self.recorder.caps(written.hotkey),
            listening,
            prompt: self.say(Text::PressKeys),
        };
        let muted = self.problem.is_some();
        let hotkey = setting(
            &self.say(Text::Hotkey),
            self.help(Spot::Hotkey, help),
            Some(keys),
        );
        let mut cards = vec![card(false, muted, vec![hotkey])];
        if let Some(on) = self.starts {
            let toggle = Control::Toggle {
                target: Target::StartWithWindows,
                on,
            };
            let help = (self.say(Text::StartsInTray), false);
            let strip = setting(&self.say(Text::StartWithWindows), help, Some(toggle));
            cards.push(card(false, muted, vec![strip]));
        }
        cards.push(self.languages(muted));
        Some(Section {
            title: Some(self.say(Text::Launcher)),
            cards,
        })
    }

    /// The choice of language: each language by its own name, the one
    /// spoken on.
    fn languages(&self, muted: bool) -> Card {
        let options = Language::ALL.map(|language| Choice {
            label: language.name().to_string(),
            target: Target::Language(language),
            on: language == self.language(),
        });
        let help = self.help(Spot::Language, Text::LanguageHelp);
        let choice = Control::Choice(options.to_vec());
        let strip = setting(&self.say(Text::LanguageName), help, Some(choice));
        card(false, muted, vec![strip])
    }

    /// The three lists of folders.
    fn folders(&self) -> Option<Section> {
        self.written.as_ref()?;
        let cards = List::FOLDERS.map(|list| self.list(list)).to_vec();
        Some(Section {
            title: Some(self.say(Text::Folders)),
            cards,
        })
    }

    /// The user's web searches.
    fn search(&self) -> Option<Section> {
        self.written.as_ref()?;
        Some(Section {
            title: Some(self.say(Text::Web)),
            cards: vec![self.list(List::Search)],
        })
    }

    /// A list: its setting, with Add folder… for a list of folders, its
    /// entries, and for the lists that take a name or a web search, a
    /// field for one.
    fn list(&self, list: List) -> Card {
        let (name, help) = match list {
            List::Exclude => (Text::Exclude, Text::ExcludeHelp),
            List::RankLower => (Text::RankLower, Text::RankLowerHelp),
            List::RankHigher => (Text::RankHigher, Text::RankHigherHelp),
            List::Search => (Text::Searches, Text::SearchesHelp),
        };
        let pick = Control::Button(button(&self.say(Text::AddFolder), Target::Pick(list)));
        let pick = (list != List::Search).then_some(pick);
        let help = self.help(Spot::List(list), help);
        let mut strips = vec![setting(&self.say(name), help, pick)];
        let entries = self.entries(list).iter().enumerate();
        strips.extend(entries.map(|(at, text)| Strip::Entry {
            text: text.clone(),
            remove: Target::Remove(list, at),
        }));
        if list != List::Exclude {
            let empty = Field::default();
            let name = self.names.get(&list).unwrap_or(&empty);
            strips.push(Strip::Field {
                target: Target::Name(list),
                text: name.text().to_string(),
                caret: name.caret_utf16(),
                selection: name.selection_utf16(),
                placeholder: self.placeholder(list),
                add: button(&self.say(Text::Add), Target::AddName(list)),
            });
        }
        card(false, self.problem.is_some(), strips)
    }

    /// A newer release, first after a problem in the file: what it is
    /// with Install, its notes, and the switch.
    fn news(&self) -> Option<Section> {
        let update = self.update()?;
        let mut cards = vec![self.newer(update)];
        cards.extend(self.checking());
        Some(Section {
            title: Some(self.say(Text::Updates)),
            cards,
        })
    }

    /// The switch alone, near the end, while there is no newer release.
    fn updates(&self) -> Option<Section> {
        if self.update().is_some() {
            return None;
        }
        Some(Section {
            title: Some(self.say(Text::Updates)),
            cards: vec![self.checking()?],
        })
    }

    /// The newer release: its version with Install, how installing it
    /// goes, and its notes.
    fn newer(&self, update: &Update) -> Card {
        let language = self.language();
        let current = self.updates.as_ref().map(|versions| versions.current);
        let current = current
            .map(|version| version.to_string())
            .unwrap_or_default();
        let help = match &update.progress {
            Progress::Offered => (language.fill(Text::InstallHelp, &[&current]), false),
            Progress::Installing => (self.say(Text::InstallingHelp), false),
            Progress::Failed(failure) => {
                let why = failure.said(language);
                (language.fill(Text::FailedHelp, &[&why]), true)
            }
        };
        let version = update.release.version.to_string();
        let name = language.fill(Text::UpdateOut, &[&version]);
        let install = Control::Button(button(&self.say(Text::Install), Target::Install));
        let mut strips = vec![setting(&name, help, Some(install))];
        let notes = update.release.lines();
        if !notes.is_empty() {
            strips.push(Strip::Text {
                text: notes.join("\n"),
            });
        }
        card(false, false, strips)
    }

    /// The switch for checking for updates, with the version that runs.
    fn checking(&self) -> Option<Card> {
        let written = self.written.as_ref()?;
        let current = self.updates.as_ref()?.current.to_string();
        let help = self.language().fill(Text::CheckHelp, &[&current]);
        let toggle = Control::Toggle {
            target: Target::CheckUpdates,
            on: written.check_for_updates,
        };
        let help = self.noted(Spot::Updates, help);
        let strip = setting(&self.say(Text::CheckUpdates), help, Some(toggle));
        Some(card(false, self.problem.is_some(), vec![strip]))
    }

    /// What the field of `list` shows while nothing is typed in it.
    fn placeholder(&self, list: List) -> String {
        let example = match list {
            List::Search => return self.say(Text::Engine),
            List::RankHigher => "notes",
            _ => "target",
        };
        self.language().fill(Text::FolderName, &[example])
    }

    /// The way to the file itself.
    fn file(&self) -> Option<Section> {
        let open = Control::Button(button(&self.say(Text::OpenConfig), Target::OpenConfig));
        let help = (self.say(Text::EditFileHelp), false);
        let strip = setting(&self.say(Text::EditFile), help, Some(open));
        Some(Section {
            title: Some("config.toml".to_string()),
            cards: vec![card(false, false, vec![strip])],
        })
    }
}

fn button(label: &str, target: Target) -> Button {
    Button {
        label: label.to_string(),
        target,
    }
}

fn setting(name: &str, (help, alarm): (String, bool), control: Option<Control>) -> Strip {
    Strip::Setting {
        name: name.to_string(),
        help,
        alarm,
        control,
    }
}

fn card(alarm: bool, muted: bool, strips: Vec<Strip>) -> Card {
    Card {
        alarm,
        muted,
        strips,
    }
}

/// The keys of the hotkey a panel shows, as the recorder names them.
#[cfg(test)]
pub fn caps(text: &str) -> Vec<String> {
    crate::recorder::caps(hyalo_core::hotkey::Hotkey::parse(text).expect("a hotkey"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use hyalo_core::config::{self, DEFAULT};
    use hyalo_core::hotkey::Hotkey;
    use hyalo_core::text::Language;
    use hyalo_core::update::{Progress, Update, Version};
    use std::ops::Range;
    use windows::Win32::UI::Input::KeyboardAndMouse::{
        VK_A, VK_C, VK_DELETE, VK_END, VK_F4, VK_HOME, VK_K, VK_LEFT, VK_SPACE, VK_V, VK_X,
    };

    use crate::page::{Area, What, lay_out};

    fn written(text: &str) -> Written {
        config::written(text).expect("good settings")
    }

    fn panel() -> Panel {
        Panel::new(Ok(written(DEFAULT)), Some(false), Language::English)
    }

    fn broken() -> Problem {
        config::written("\nhotkye = 1\n").expect_err("a problem")
    }

    /// Every letter 8 DIPs wide.
    fn laid(panel: &Panel) -> Laid {
        lay_out(&panel.page(), 640.0, &|text| {
            text.chars().count() as f32 * 8.0
        })
    }

    fn click(panel: &mut Panel, target: Target) -> Option<Effect> {
        panel.press(Some(target));
        panel.release(Some(target))
    }

    fn press(panel: &mut Panel, key: u16, mods: Mods) -> Option<Effect> {
        let laid = laid(panel);
        panel.key(key, mods, &laid, 2000.0)
    }

    fn plain(panel: &mut Panel, key: u16) -> Option<Effect> {
        press(panel, key, Mods::default())
    }

    fn type_text(panel: &mut Panel, text: &str) {
        for unit in text.encode_utf16() {
            panel.char(unit);
        }
    }

    const SHIFT: Mods = Mods {
        ctrl: false,
        alt: false,
        shift: true,
        win: false,
    };

    const CTRL_SHIFT: Mods = Mods {
        ctrl: true,
        alt: false,
        shift: true,
        win: false,
    };

    /// Each mark's text, in order, for the marks that hold text.
    fn texts(panel: &Panel) -> Vec<String> {
        let marks = laid(panel).marks.into_iter();
        marks
            .filter_map(|mark| match mark.what {
                What::Title(text)
                | What::Header(text)
                | What::Name(text)
                | What::Entry(text)
                | What::Line(text) => Some(text),
                What::Help { text, alarm: true } => Some(format!("! {text}")),
                What::Field { text, .. } if !text.is_empty() => Some(format!("[{text}]")),
                _ => None,
            })
            .collect()
    }

    fn keys(panel: &Panel) -> (Vec<String>, bool) {
        let marks = laid(panel).marks.into_iter();
        let mut keys = marks.filter_map(|mark| match mark.what {
            What::Keys {
                caps, listening, ..
            } => Some((caps, listening)),
            _ => None,
        });
        keys.next().expect("the recorder")
    }

    fn toggle(panel: &Panel) -> Option<bool> {
        let marks = laid(panel).marks.into_iter();
        let mut toggles = marks.filter_map(|mark| match mark.what {
            What::Toggle { on, .. } => Some(on),
            _ => None,
        });
        toggles.next()
    }

    #[test]
    fn the_page_shows_every_setting_the_file_writes() {
        let panel = panel();
        let shown = [
            "Settings",
            "Launcher",
            "Hotkey",
            "Start with Windows",
            "Language",
            "Folders",
            "Leave out of the index",
            r"C:\Windows\WinSxS",
            "Rank lower",
            "target",
            "Rank higher",
            "Web search",
            "Search prefixes",
            "g https://www.google.com/search?q={query}",
            "gh https://github.com/search?q={query}",
            "w https://en.wikipedia.org/w/index.php?search={query}",
            "ya https://yandex.ru/search/?text={query}",
            "yt https://www.youtube.com/results?search_query={query}",
            "config.toml",
            "Edit the file instead",
        ];
        assert_eq!(texts(&panel), shown);
        assert_eq!(keys(&panel), (caps("Alt+Space"), false));
        assert_eq!(toggle(&panel), Some(false));
        let on = Panel::new(Ok(written(DEFAULT)), Some(true), Language::English);
        assert_eq!(toggle(&on), Some(true));
    }

    #[test]
    fn where_hyalo_may_not_start_with_windows_there_is_no_switch() {
        let panel = Panel::new(Ok(written(DEFAULT)), None, Language::English);
        assert_eq!(toggle(&panel), None);
        assert!(!texts(&panel).contains(&"Start with Windows".to_string()));
    }

    #[test]
    fn the_lists_that_take_a_name_have_a_field_for_one() {
        let laid = laid(&panel());
        let fields: Vec<(Target, String)> = laid
            .marks
            .iter()
            .filter_map(|mark| match &mark.what {
                What::Field {
                    target,
                    placeholder,
                    ..
                } => Some((*target, placeholder.clone())),
                _ => None,
            })
            .collect();
        let folder = |list, name: &str| {
            let placeholder = format!("A folder name, such as {name}");
            (Target::Name(list), placeholder)
        };
        assert_eq!(
            fields,
            [
                folder(List::RankLower, "target"),
                folder(List::RankHigher, "notes"),
                (
                    Target::Name(List::Search),
                    "A prefix and an address with {query}".to_string()
                ),
            ]
        );
    }

    const SO: &str = "so https://stackoverflow.com/search?q={query}";

    /// What the page shows after the list of web searches' name.
    fn after_searches(panel: &Panel) -> Option<String> {
        let shown = texts(panel);
        let at = shown.iter().position(|text| text == "Search prefixes")?;
        shown.get(at + 1).cloned()
    }

    #[test]
    fn a_web_search_typed_is_added_and_its_cross_takes_it_out() {
        let mut panel = panel();
        assert!(!panel.look(Target::Pick(List::Search)).enabled, "no folder");
        click(&mut panel, Target::Name(List::Search));
        type_text(&mut panel, SO);
        let add = Some(Effect::Change(Change::Add(List::Search, SO.to_string())));
        assert_eq!(plain(&mut panel, VK_RETURN.0), add);
        let change = Change::Add(List::Search, SO.to_string());
        let after = config::edit(DEFAULT, &change).expect("edited");
        panel.done(&change, Some(written(&after)));
        assert!(texts(&panel).contains(&SO.to_string()), "shown");
        let listed = written(&after).list(List::Search).to_vec();
        let at = listed.iter().position(|entry| entry == SO).expect("listed");
        let remove = Change::Remove(List::Search, SO.to_string());
        assert_eq!(
            click(&mut panel, Target::Remove(List::Search, at)),
            Some(Effect::Change(remove))
        );
    }

    #[test]
    fn a_web_search_refused_says_why_by_its_list() {
        let mut panel = panel();
        click(&mut panel, Target::Name(List::Search));
        type_text(&mut panel, "so");
        let change = Change::Add(List::Search, "so".to_string());
        let wanted = hyalo_core::config::Wanted::Engine;
        panel.refused(&change, Why::Value(wanted, "so".to_string()));
        assert!(texts(&panel).contains(&"[so]".to_string()));
        let said = after_searches(&panel).expect("a warning");
        assert!(
            said.starts_with("! `so` is not a prefix and an address"),
            "{said}"
        );
    }

    #[test]
    fn a_problem_in_the_file_comes_first_and_only_the_file_may_be_opened() {
        let mut panel = panel();
        panel.reload(Err(broken()));
        let shown = texts(&panel);
        assert_eq!(shown[1], "config.toml has a problem");
        assert_eq!(
            shown[2..4],
            [
                "Line 2: unknown field `hotkye`, expected one of `hotkey`, `language`,",
                "`check_for_updates`, `folders`, `search`"
            ]
        );
        assert!(
            shown.contains(&"Hotkey".to_string()),
            "the last good settings stay"
        );
        for target in [
            Target::Hotkey,
            Target::Pick(List::Exclude),
            Target::Remove(List::Exclude, 0),
        ] {
            assert!(!panel.look(target).enabled, "{target:?}");
            assert_eq!(click(&mut panel, target), None, "{target:?}");
        }
        assert!(!panel.listening());
        assert_eq!(click(&mut panel, Target::Fix), Some(Effect::OpenConfig));
        assert_eq!(
            click(&mut panel, Target::OpenConfig),
            Some(Effect::OpenConfig)
        );
        panel.reload(Ok(written(DEFAULT)));
        assert_eq!(texts(&panel)[1], "Launcher", "fixed");
        assert_eq!(laid(&panel).area(Target::Fix), None);
    }

    #[test]
    fn while_the_file_has_a_problem_the_settings_are_muted() {
        let muted = |panel: &Panel| {
            let marks = laid(panel).marks.into_iter();
            let cards = marks.filter_map(|mark| match mark.what {
                What::Card { alarm } => Some((alarm, mark.muted)),
                _ => None,
            });
            cards.collect::<Vec<_>>()
        };
        let mut panel = panel();
        assert!(muted(&panel).iter().all(|&card| card == (false, false)));
        panel.reload(Err(broken()));
        let cards = [
            (true, false),
            (false, true),
            (false, true),
            (false, true),
            (false, true),
            (false, true),
            (false, true),
            (false, true),
            (false, false),
        ];
        assert_eq!(muted(&panel), cards);
    }

    #[test]
    fn a_file_broken_from_the_start_shows_its_problem_and_the_way_to_it() {
        let panel = Panel::new(Err(broken()), Some(true), Language::English);
        let shown = texts(&panel);
        assert_eq!(shown.len(), 6, "{shown:?}");
        assert!(shown[2].starts_with("Line 2: "));
        assert_eq!(laid(&panel).targets(), [Target::Fix, Target::OpenConfig]);
        let help = laid(&panel)
            .marks
            .into_iter()
            .find_map(|mark| match mark.what {
                What::Help { text, .. } => Some(text),
                _ => None,
            });
        assert!(help.expect("help").contains("runs on its defaults"));
    }

    #[test]
    fn a_click_on_the_hotkey_listens_and_a_new_combination_is_the_change() {
        let mut panel = panel();
        assert_eq!(click(&mut panel, Target::Hotkey), None);
        assert!(panel.listening());
        assert_eq!(keys(&panel), (Vec::new(), true));
        assert!(texts(&panel).iter().all(|text| !text.starts_with('!')));
        let shown = laid(&panel).marks.into_iter().any(|mark| {
            mark.what
                == What::Help {
                    text: "Press the new combination; Esc keeps the one you have".to_string(),
                    alarm: false,
                }
        });
        assert!(shown);
        panel.key_up(CTRL_SHIFT);
        assert_eq!(keys(&panel).0, ["Ctrl", "Shift"]);
        let hotkey = Hotkey::parse("Ctrl+Shift+K").expect("a hotkey");
        let heard = press(&mut panel, VK_K.0, CTRL_SHIFT);
        assert_eq!(heard, Some(Effect::Change(Change::Hotkey(hotkey))));
        assert!(!panel.listening());
    }

    #[test]
    fn the_hotkey_it_has_changes_nothing() {
        let mut panel = Panel::new(Ok(written("hotkey = 'F4'\n")), None, Language::English);
        click(&mut panel, Target::Hotkey);
        assert_eq!(plain(&mut panel, VK_F4.0), None);
        assert!(!panel.listening());
        click(&mut panel, Target::Hotkey);
        assert!(panel.hotkey_pressed(), "the hotkey itself, as WM_HOTKEY");
        assert!(!panel.listening());
        assert!(!panel.hotkey_pressed(), "not listening: the launcher's");
    }

    #[test]
    fn esc_while_listening_keeps_the_hotkey_and_the_window() {
        let mut panel = panel();
        click(&mut panel, Target::Hotkey);
        assert_eq!(plain(&mut panel, VK_ESCAPE.0), None);
        assert!(!panel.listening());
        assert_eq!(keys(&panel), (caps("Alt+Space"), false));
        assert_eq!(plain(&mut panel, VK_ESCAPE.0), Some(Effect::Close));
    }

    #[test]
    fn alt_f4_while_listening_closes_the_window_rather_than_taking_alt_f4_everywhere() {
        let mut panel = panel();
        click(&mut panel, Target::Hotkey);
        let alt = Mods {
            alt: true,
            ..Mods::default()
        };
        assert_eq!(press(&mut panel, VK_F4.0, alt), Some(Effect::Close));
        assert!(!panel.listening());
        click(&mut panel, Target::Hotkey);
        let ctrl_alt = Mods { ctrl: true, ..alt };
        let hotkey = Hotkey::parse("Ctrl+Alt+F4").expect("a hotkey");
        let heard = press(&mut panel, VK_F4.0, ctrl_alt);
        assert_eq!(
            heard,
            Some(Effect::Change(Change::Hotkey(hotkey))),
            "with more held, a combination"
        );
    }

    #[test]
    fn a_combination_that_may_not_be_a_hotkey_is_told_and_it_listens_on() {
        let mut panel = panel();
        click(&mut panel, Target::Hotkey);
        assert_eq!(plain(&mut panel, VK_SPACE.0), None);
        let warning = texts(&panel).into_iter().find(|text| text.starts_with('!'));
        assert!(
            warning
                .expect("a warning")
                .contains("needs Ctrl, Alt or Win")
        );
        assert!(panel.listening());
        panel.key_up(Mods::default());
        assert_eq!(
            plain(&mut panel, VK_F4.0),
            Some(Effect::Change(Change::Hotkey(
                Hotkey::parse("F4").expect("F4")
            )))
        );
        assert!(texts(&panel).iter().all(|text| !text.starts_with('!')));
    }

    #[test]
    fn a_click_elsewhere_or_on_the_field_again_stops_listening() {
        let mut panel = panel();
        click(&mut panel, Target::Hotkey);
        panel.press(None);
        assert!(!panel.listening());
        click(&mut panel, Target::Hotkey);
        click(&mut panel, Target::Hotkey);
        assert!(!panel.listening());
    }

    #[test]
    fn a_hotkey_windows_refused_is_told_by_the_hotkey() {
        let mut panel = panel();
        let change = Change::Hotkey(Hotkey::parse("Ctrl+K").expect("a hotkey"));
        panel.refused(
            &change,
            Why::Taken(Hotkey::parse("Ctrl+K").expect("a hotkey")),
        );
        assert!(texts(&panel).contains(&"! Ctrl+K is taken by another app".to_string()));
        assert_eq!(keys(&panel).0, caps("Alt+Space"));
        click(&mut panel, Target::Hotkey);
        assert!(
            texts(&panel).iter().all(|text| !text.starts_with('!')),
            "a new try"
        );
    }

    /// The help line under the setting named `name`.
    fn help_of(panel: &Panel, name: &str) -> What {
        let marks = laid(panel).marks;
        let at = marks
            .iter()
            .position(|mark| mark.what == What::Name(name.to_string()));
        let after = &marks[at.expect("the setting") + 1..];
        let help = after
            .iter()
            .find(|mark| matches!(mark.what, What::Help { .. }));
        help.expect("its help").what.clone()
    }

    #[test]
    fn what_went_wrong_shows_by_its_own_setting_only() {
        let mut panel = panel();
        let why = Why::Taken(Hotkey::parse("Ctrl+K").expect("a hotkey"));
        let change = Change::Hotkey(Hotkey::parse("Ctrl+K").expect("a hotkey"));
        panel.refused(&change, why.clone());
        let warned = What::Help {
            text: "Ctrl+K is taken by another app".to_string(),
            alarm: true,
        };
        assert_eq!(help_of(&panel, "Hotkey"), warned);
        assert!(matches!(
            help_of(&panel, "Rank lower"),
            What::Help { alarm: false, .. }
        ));
        let change = Change::Add(List::RankLower, "x".to_string());
        panel.refused(&change, why.clone());
        assert_eq!(help_of(&panel, "Rank lower"), warned);
        assert!(matches!(
            help_of(&panel, "Hotkey"),
            What::Help { alarm: false, .. }
        ));
        assert!(matches!(
            help_of(&panel, "Rank higher"),
            What::Help { alarm: false, .. }
        ));
    }

    #[test]
    fn the_list_of_full_paths_has_no_field_for_a_name() {
        let panel = panel();
        for target in [Target::Name(List::Exclude), Target::AddName(List::Exclude)] {
            assert!(!panel.look(target).enabled, "{target:?}");
        }
        assert!(panel.look(Target::Name(List::RankLower)).enabled);
    }

    #[test]
    fn the_switch_turns_starting_with_windows_the_other_way() {
        let mut panel = panel();
        assert_eq!(
            click(&mut panel, Target::StartWithWindows),
            Some(Effect::StartWithWindows(true))
        );
        panel.set_starts(Some(true));
        assert_eq!(
            click(&mut panel, Target::StartWithWindows),
            Some(Effect::StartWithWindows(false))
        );
    }

    #[test]
    fn add_folder_picks_one_and_the_cross_removes_its_entry_by_its_text() {
        let mut panel = panel();
        assert_eq!(
            click(&mut panel, Target::Pick(List::Exclude)),
            Some(Effect::Pick(List::Exclude))
        );
        let remove = Change::Remove(List::Exclude, r"C:\Windows\WinSxS".to_string());
        assert_eq!(
            click(&mut panel, Target::Remove(List::Exclude, 0)),
            Some(Effect::Change(remove))
        );
        assert_eq!(
            click(&mut panel, Target::Remove(List::Exclude, 1)),
            None,
            "no such entry"
        );
    }

    #[test]
    fn a_name_typed_is_added_with_enter_or_its_button_and_leaves_the_field_once_written() {
        let mut panel = panel();
        click(&mut panel, Target::Name(List::RankLower));
        type_text(&mut panel, "  vendor ");
        assert!(texts(&panel).contains(&"[  vendor ]".to_string()));
        let add = Some(Effect::Change(Change::Add(
            List::RankLower,
            "vendor".to_string(),
        )));
        assert_eq!(plain(&mut panel, VK_RETURN.0), add);
        assert_eq!(click(&mut panel, Target::AddName(List::RankLower)), add);
        let change = Change::Add(List::RankLower, "vendor".to_string());
        let after = config::edit(DEFAULT, &change).expect("edited");
        panel.done(&change, Some(written(&after)));
        assert!(texts(&panel).contains(&"vendor".to_string()));
        assert!(!texts(&panel).contains(&"[  vendor ]".to_string()));
        assert_eq!(
            click(&mut panel, Target::AddName(List::RankLower)),
            None,
            "nothing to add"
        );
        assert_eq!(
            click(&mut panel, Target::AddName(List::RankHigher)),
            None,
            "never typed in"
        );
    }

    #[test]
    fn a_name_refused_stays_in_its_field_and_says_why() {
        let mut panel = panel();
        click(&mut panel, Target::Name(List::RankHigher));
        type_text(&mut panel, r"old\stuff");
        let change = Change::Add(List::RankHigher, r"old\stuff".to_string());
        let place = hyalo_core::config::Wanted::Place;
        panel.refused(&change, Why::Value(place, r"old\stuff".to_string()));
        let shown = texts(&panel);
        assert!(shown.contains(&r"[old\stuff]".to_string()), "{shown:?}");
        assert!(shown.iter().any(|text| text.starts_with("! `old")));
        let other = Change::Add(List::RankLower, "x".to_string());
        panel.done(&other, None);
        assert!(
            texts(&panel).contains(&r"[old\stuff]".to_string()),
            "another field's change"
        );
    }

    #[test]
    fn typing_goes_to_the_field_with_the_focus_and_nowhere_else() {
        let mut panel = panel();
        assert!(!panel.char(u16::from(b'x')));
        click(&mut panel, Target::Pick(List::RankLower));
        assert!(!panel.char(u16::from(b'x')));
        click(&mut panel, Target::Name(List::RankHigher));
        assert!(panel.char(u16::from(b'x')));
        assert_eq!(
            plain(&mut panel, VK_SPACE.0),
            None,
            "a space is typed, not a click"
        );
        assert!(panel.char(u16::from(b' ')));
        assert!(texts(&panel).contains(&"[x ]".to_string()));
        panel.reload(Err(broken()));
        assert!(!panel.char(u16::from(b'y')), "not while the file is broken");
    }

    #[test]
    fn what_is_typed_stays_while_the_file_changes_under_it() {
        let mut panel = panel();
        click(&mut panel, Target::Name(List::RankLower));
        type_text(&mut panel, "ven");
        panel.reload(Ok(written("hotkey = 'F4'\n")));
        assert!(texts(&panel).contains(&"[ven]".to_string()));
        assert_eq!(keys(&panel).0, ["F4"]);
        assert!(panel.char(u16::from(b'd')));
        assert!(panel.look(Target::Name(List::RankLower)).focused);
    }

    #[test]
    fn tab_rings_each_control_in_turn_and_shift_tab_goes_back() {
        let mut panel = panel();
        let order = laid(&panel).targets();
        assert_eq!(order[0], Target::Hotkey);
        for &target in order.iter().chain(&order[..1]) {
            plain(&mut panel, VK_TAB.0);
            let look = panel.look(target);
            assert!(look.focused && look.ring, "{target:?}");
        }
        press(&mut panel, VK_TAB.0, SHIFT);
        assert!(panel.look(*order.last().expect("controls")).ring);
        let mut fresh = Panel::new(Ok(written(DEFAULT)), Some(false), Language::English);
        press(&mut fresh, VK_TAB.0, SHIFT);
        assert!(
            fresh.look(*order.last().expect("controls")).ring,
            "from nothing, the last"
        );
    }

    #[test]
    fn a_click_takes_the_focus_without_a_ring() {
        let mut panel = panel();
        plain(&mut panel, VK_TAB.0);
        click(&mut panel, Target::Pick(List::RankHigher));
        let look = panel.look(Target::Pick(List::RankHigher));
        assert!(look.focused && !look.ring);
        assert!(!panel.look(Target::Hotkey).focused);
    }

    #[test]
    fn space_and_enter_use_the_control_with_the_focus() {
        let mut panel = panel();
        plain(&mut panel, VK_TAB.0);
        plain(&mut panel, VK_TAB.0);
        assert!(panel.look(Target::StartWithWindows).focused);
        assert_eq!(
            plain(&mut panel, VK_SPACE.0),
            Some(Effect::StartWithWindows(true))
        );
        assert_eq!(
            plain(&mut panel, VK_RETURN.0),
            Some(Effect::StartWithWindows(true))
        );
        assert_eq!(plain(&mut panel, VK_A.0), None);
        assert_eq!(
            plain(
                &mut Panel::new(Ok(written(DEFAULT)), None, Language::English),
                VK_SPACE.0
            ),
            None,
            "no focus"
        );
    }

    #[test]
    fn tab_passes_over_what_may_not_be_used() {
        let mut panel = panel();
        panel.reload(Err(broken()));
        plain(&mut panel, VK_TAB.0);
        assert!(panel.look(Target::Fix).ring);
        plain(&mut panel, VK_TAB.0);
        assert!(panel.look(Target::OpenConfig).ring);
        plain(&mut panel, VK_TAB.0);
        assert!(panel.look(Target::Fix).ring);
    }

    #[test]
    fn removing_the_entry_with_the_focus_hands_it_to_the_one_before() {
        let text = "[folders]\nrank_lower = ['a', 'b']\n";
        let mut panel = Panel::new(Ok(written(text)), None, Language::English);
        click(&mut panel, Target::Remove(List::RankLower, 1));
        panel.reload(Ok(written("[folders]\nrank_lower = ['a']\n")));
        assert!(panel.look(Target::Remove(List::RankLower, 0)).focused);
        panel.reload(Ok(written("[folders]\nrank_lower = []\n")));
        assert!(panel.look(Target::Pick(List::RankLower)).focused);
        click(&mut panel, Target::StartWithWindows);
        assert!(!panel.look(Target::StartWithWindows).focused, "none here");
        let mut starting = Panel::new(Ok(written(text)), Some(true), Language::English);
        click(&mut starting, Target::StartWithWindows);
        starting.set_starts(None);
        assert_eq!(
            (0..4)
                .filter(|&at| starting.look(Target::Remove(List::RankLower, at)).focused)
                .count(),
            0
        );
        assert!(!starting.look(Target::StartWithWindows).focused);
    }

    #[test]
    fn a_click_is_a_press_and_a_release_on_the_same_control() {
        let mut panel = panel();
        panel.press(Some(Target::Pick(List::Exclude)));
        assert!(panel.look(Target::Pick(List::Exclude)).pressed);
        assert_eq!(panel.release(Some(Target::Pick(List::RankLower))), None);
        assert!(!panel.look(Target::Pick(List::Exclude)).pressed);
        assert_eq!(
            panel.release(Some(Target::Pick(List::Exclude))),
            None,
            "released already"
        );
        panel.press(None);
        assert_eq!(panel.release(None), None);
    }

    #[test]
    fn the_pointer_lights_what_it_is_over_and_says_when_that_changes() {
        let mut panel = panel();
        assert!(panel.hover(Some(Target::Hotkey)));
        assert!(panel.look(Target::Hotkey).hovered);
        assert!(!panel.hover(Some(Target::Hotkey)));
        assert!(panel.hover(None));
        assert!(!panel.look(Target::Hotkey).hovered);
        panel.reload(Err(broken()));
        assert!(
            !panel.hover(Some(Target::Hotkey)),
            "not what may not be used"
        );
    }

    #[test]
    fn the_wheel_scrolls_within_the_page() {
        let mut panel = panel();
        panel.wheel(-120, 1000.0, 600.0);
        assert_eq!(panel.scroll(), 48.0);
        panel.wheel(-1200, 1000.0, 600.0);
        assert_eq!(panel.scroll(), 400.0);
        panel.wheel(240, 1000.0, 600.0);
        assert_eq!(panel.scroll(), 304.0);
        panel.fit(800.0, 600.0);
        assert_eq!(panel.scroll(), 200.0);
        panel.wheel(1200, 1000.0, 600.0);
        assert_eq!(panel.scroll(), 0.0);
    }

    #[test]
    fn tab_brings_the_control_it_rings_into_view() {
        let mut panel = panel();
        let laid = laid(&panel);
        let last = *laid.targets().last().expect("controls");
        let area: Area = laid.area(last).expect("placed");
        panel.key(VK_TAB.0, SHIFT, &laid, 300.0);
        assert_eq!(panel.scroll(), area.bottom() + 12.0 - 300.0);
        panel.key(VK_TAB.0, Mods::default(), &laid, 300.0);
        assert_eq!(panel.scroll(), 0.0, "round to the first");
    }

    const CTRL: Mods = Mods {
        ctrl: true,
        alt: false,
        shift: false,
        win: false,
    };

    /// The name field of `list` as the page shows it: its text, the caret
    /// and the selection in UTF-16 units.
    fn field(panel: &Panel, list: List) -> (String, u32, Range<u32>) {
        let marks = laid(panel).marks.into_iter();
        let mut fields = marks.filter_map(|mark| match mark.what {
            What::Field {
                text,
                target,
                caret,
                selection,
                ..
            } if target == Target::Name(list) => Some((text, caret, selection)),
            _ => None,
        });
        fields.next().expect("the field")
    }

    #[test]
    fn the_keys_edit_the_field_with_the_focus_as_they_do_the_search_bar() {
        let mut panel = panel();
        let lower = List::RankLower;
        click(&mut panel, Target::Name(lower));
        type_text(&mut panel, "vendor");
        assert_eq!(field(&panel, lower), ("vendor".to_string(), 6, 6..6));
        plain(&mut panel, VK_LEFT.0);
        plain(&mut panel, VK_LEFT.0);
        type_text(&mut panel, "X");
        assert_eq!(field(&panel, lower), ("vendXor".to_string(), 5, 5..5));
        plain(&mut panel, VK_HOME.0);
        press(&mut panel, VK_END.0, SHIFT);
        assert_eq!(field(&panel, lower), ("vendXor".to_string(), 7, 0..7));
        type_text(&mut panel, "lib");
        plain(&mut panel, VK_HOME.0);
        plain(&mut panel, VK_DELETE.0);
        assert_eq!(field(&panel, lower), ("ib".to_string(), 0, 0..0));
        assert_eq!(field(&panel, List::RankHigher).0, "", "the other field");
    }

    #[test]
    fn editing_keys_do_nothing_without_a_field_to_edit() {
        let mut panel = panel();
        click(&mut panel, Target::Name(List::RankLower));
        type_text(&mut panel, "vendor");
        click(&mut panel, Target::Pick(List::RankLower));
        for key in [VK_LEFT, VK_HOME, VK_DELETE] {
            assert_eq!(plain(&mut panel, key.0), None);
        }
        assert_eq!(press(&mut panel, VK_A.0, CTRL), None);
        assert_eq!(
            field(&panel, List::RankLower),
            ("vendor".to_string(), 6, 6..6)
        );
    }

    #[test]
    fn copy_and_cut_hand_the_selection_over_and_paste_asks_for_the_clipboard() {
        let mut panel = panel();
        let higher = List::RankHigher;
        click(&mut panel, Target::Name(higher));
        type_text(&mut panel, "notes");
        assert_eq!(press(&mut panel, VK_C.0, CTRL), None, "nothing selected");
        assert_eq!(press(&mut panel, VK_X.0, CTRL), None, "nothing selected");
        press(&mut panel, VK_LEFT.0, CTRL_SHIFT);
        let copied = Some(Effect::Copy("notes".to_string()));
        assert_eq!(press(&mut panel, VK_C.0, CTRL), copied);
        assert_eq!(field(&panel, higher).0, "notes");
        assert_eq!(press(&mut panel, VK_X.0, CTRL), copied);
        assert_eq!(field(&panel, higher).0, "");
        assert_eq!(press(&mut panel, VK_V.0, CTRL), Some(Effect::Paste));
        assert!(panel.paste(
            "my
notes
"
        ));
        assert_eq!(field(&panel, higher), ("my notes".to_string(), 8, 8..8));
        click(&mut panel, Target::Pick(higher));
        assert!(!panel.paste("more"), "no field has the focus");
    }

    #[test]
    fn the_mouse_places_the_caret_selects_a_word_or_everything() {
        let mut panel = panel();
        let lower = List::RankLower;
        assert!(!panel.place(1, false), "no field has the focus");
        click(&mut panel, Target::Name(lower));
        type_text(&mut panel, "my vendor");
        assert!(panel.place(1, false));
        assert_eq!(field(&panel, lower).2, 1..1);
        assert!(panel.place(5, true));
        assert_eq!(field(&panel, lower).2, 1..5);
        assert!(!panel.place(5, true), "already there");
        assert!(panel.select_word(4));
        assert_eq!(field(&panel, lower).2, 3..9);
        assert!(panel.select_all());
        assert_eq!(field(&panel, lower).2, 0..9);
        assert_eq!(panel.dragging(), None, "the button came up");
        panel.press(Some(Target::Name(lower)));
        assert_eq!(panel.dragging(), Some(lower));
        panel.release(Some(Target::Name(lower)));
        assert_eq!(panel.dragging(), None);
    }

    #[test]
    fn the_field_shows_its_text_only_from_the_name_typed() {
        let mut panel = panel();
        click(&mut panel, Target::Name(List::RankLower));
        assert_eq!(field(&panel, List::RankLower), (String::new(), 0, 0..0));
        type_text(&mut panel, "a🔍");
        assert_eq!(field(&panel, List::RankLower), ("a🔍".to_string(), 3, 3..3));
    }

    fn russian() -> Panel {
        let text = config::default_file(Language::Russian);
        Panel::new(Ok(written(&text)), Some(false), Language::English)
    }

    /// Each option of the language choice: its label, and whether it is
    /// on.
    fn choices(panel: &Panel) -> Vec<(String, bool)> {
        let marks = laid(panel).marks.into_iter();
        let options = marks.filter_map(|mark| match mark.what {
            What::Choice { label, on, .. } => Some((label, on)),
            _ => None,
        });
        options.collect()
    }

    #[test]
    fn the_page_speaks_the_language_the_file_names() {
        let panel = russian();
        let shown = [
            "Параметры",
            "Основные",
            "Сочетание клавиш",
            "Запускать вместе с Windows",
            "Язык",
            "Папки",
            "Не индексировать",
            r"C:\Windows\WinSxS",
            "Показывать ниже",
            "target",
            "Показывать выше",
            "Поиск в интернете",
            "Префиксы поиска",
            "g https://www.google.com/search?q={query}",
            "gh https://github.com/search?q={query}",
            "w https://ru.wikipedia.org/w/index.php?search={query}",
            "ya https://yandex.ru/search/?text={query}",
            "yt https://www.youtube.com/results?search_query={query}",
            "config.toml",
            "Редактировать файл напрямую",
        ];
        assert_eq!(texts(&panel), shown);
        assert_eq!(panel.title(), "Параметры hyalo");
        assert_eq!(panel.language(), Language::Russian);
    }

    #[test]
    fn a_file_without_a_language_speaks_the_one_windows_is_shown_in() {
        let windows = Panel::new(Ok(written("hotkey = 'F4'\n")), None, Language::Russian);
        assert_eq!(windows.language(), Language::Russian);
        assert_eq!(texts(&windows)[0], "Параметры");
        let named = Panel::new(Ok(written(DEFAULT)), None, Language::Russian);
        assert_eq!(
            named.language(),
            Language::English,
            "the file's comes first"
        );
        let broken = Panel::new(Err(broken()), None, Language::Russian);
        assert_eq!(broken.language(), Language::Russian);
    }

    #[test]
    fn the_language_spoken_is_on_and_choosing_the_other_writes_it() {
        let mut panel = panel();
        let options = [
            ("English".to_string(), true),
            ("Русский".to_string(), false),
        ];
        assert_eq!(choices(&panel), options);
        let english = Target::Language(Language::English);
        assert_eq!(click(&mut panel, english), None, "spoken already");
        let change = Change::Language(Language::Russian);
        let written_ru = Some(Effect::Change(change.clone()));
        assert_eq!(
            click(&mut panel, Target::Language(Language::Russian)),
            written_ru
        );
        let after = config::edit(DEFAULT, &change).expect("edited");
        panel.done(&change, Some(written(&after)));
        assert_eq!(texts(&panel)[0], "Параметры");
        let options = [
            ("English".to_string(), false),
            ("Русский".to_string(), true),
        ];
        assert_eq!(choices(&panel), options);
    }

    #[test]
    fn tab_goes_through_the_language_choice() {
        let mut panel = panel();
        let laid = laid(&panel);
        let order = laid.targets();
        let at = order
            .iter()
            .position(|&target| target == Target::Language(Language::English))
            .expect("on the page");
        assert_eq!(order[at + 1], Target::Language(Language::Russian));
        click(&mut panel, Target::Language(Language::English));
        plain(&mut panel, VK_TAB.0);
        let space = plain(&mut panel, VK_SPACE.0);
        let russian = Some(Effect::Change(Change::Language(Language::Russian)));
        assert_eq!(space, russian);
    }

    #[test]
    fn what_went_wrong_and_the_recorders_prompt_are_said_in_the_pages_language() {
        let mut panel = russian();
        let ctrl_k = Hotkey::parse("Ctrl+K").expect("a hotkey");
        panel.refused(&Change::Hotkey(ctrl_k), Why::Taken(ctrl_k));
        let warning = "! Ctrl+K уже занято другим приложением".to_string();
        assert!(texts(&panel).contains(&warning));
        click(&mut panel, Target::Hotkey);
        let prompt = laid(&panel)
            .marks
            .into_iter()
            .find_map(|mark| match mark.what {
                What::Keys { prompt, .. } => Some(prompt),
                _ => None,
            });
        assert_eq!(prompt.as_deref(), Some("Нажмите клавиши"));
        let help = What::Help {
            text: "Нажмите новое сочетание; Esc оставит текущее".to_string(),
            alarm: false,
        };
        assert_eq!(help_of(&panel, "Сочетание клавиш"), help);
        panel.reload(Err(broken()));
        let shown = texts(&panel);
        assert_eq!(shown[1], "В config.toml ошибка");
        assert!(
            shown[2].starts_with("Строка 2: unknown field `hotkye`"),
            "{shown:?}"
        );
    }

    fn version(text: &str) -> Version {
        Version::parse(text).expect("a version")
    }

    /// hyalo 0.0.1, and a newer release 0.0.2 with `notes`, as far as
    /// `progress`.
    fn offered(notes: &str, progress: Progress) -> Versions {
        let release = hyalo_core::update::Release {
            version: version("0.0.2"),
            notes: notes.to_string(),
            exe: String::new(),
            sum: String::new(),
        };
        Versions {
            current: version("0.0.1"),
            update: Some(Update { release, progress }),
        }
    }

    fn newest() -> Versions {
        Versions {
            current: version("0.0.1"),
            update: None,
        }
    }

    /// The headers of the page's sections, in order.
    fn headers(panel: &Panel) -> Vec<String> {
        let marks = laid(panel).marks.into_iter();
        let headers = marks.filter_map(|mark| match mark.what {
            What::Header(text) => Some(text),
            _ => None,
        });
        headers.collect()
    }

    /// Whether the switch for `target` is on, if the page has it.
    fn switch(panel: &Panel, target: Target) -> Option<bool> {
        let marks = laid(panel).marks.into_iter();
        let mut switches = marks.filter_map(|mark| match mark.what {
            What::Toggle { on, target: at } if at == target => Some(on),
            _ => None,
        });
        switches.next()
    }

    #[test]
    fn where_hyalo_does_not_update_itself_there_is_no_updates_section() {
        let panel = panel();
        assert_eq!(
            headers(&panel),
            ["Launcher", "Folders", "Web search", "config.toml"]
        );
        assert_eq!(switch(&panel, Target::CheckUpdates), None);
        assert!(laid(&panel).area(Target::Install).is_none());
    }

    #[test]
    fn with_no_newer_release_the_switch_comes_last_but_for_the_file() {
        let mut panel = panel();
        panel.set_updates(Some(newest()));
        let headers = headers(&panel);
        assert_eq!(
            headers,
            [
                "Launcher",
                "Folders",
                "Web search",
                "Updates",
                "config.toml"
            ]
        );
        assert_eq!(switch(&panel, Target::CheckUpdates), Some(true));
        let help = "You have hyalo 0.0.1; GitHub is asked once a day, nothing installs by itself";
        assert_eq!(
            help_of(&panel, "Check for updates"),
            What::Help {
                text: help.to_string(),
                alarm: false
            }
        );
        assert!(laid(&panel).area(Target::Install).is_none());
    }

    #[test]
    fn a_newer_release_comes_first_with_install_its_notes_and_the_switch() {
        let mut panel = panel();
        panel.set_updates(Some(offered(
            "## What's new\n* Updates itself",
            Progress::Offered,
        )));
        assert_eq!(
            headers(&panel),
            [
                "Updates",
                "Launcher",
                "Folders",
                "Web search",
                "config.toml"
            ]
        );
        let shown = texts(&panel);
        let at = shown.iter().position(|text| text == "hyalo 0.0.2 is out");
        let at = at.expect("the release");
        assert_eq!(
            shown[at..at + 4],
            [
                "hyalo 0.0.2 is out",
                "What's new",
                "• Updates itself",
                "Check for updates"
            ]
        );
        let help = "You have 0.0.1: Install downloads it, checks it and restarts hyalo";
        assert_eq!(
            help_of(&panel, "hyalo 0.0.2 is out"),
            What::Help {
                text: help.to_string(),
                alarm: false
            }
        );
        assert_eq!(click(&mut panel, Target::Install), Some(Effect::Install));
    }

    #[test]
    fn a_release_without_notes_shows_none() {
        let mut panel = panel();
        panel.set_updates(Some(offered("", Progress::Offered)));
        let shown = texts(&panel);
        let at = shown.iter().position(|text| text == "hyalo 0.0.2 is out");
        assert_eq!(shown[at.expect("the release") + 1], "Check for updates");
    }

    #[test]
    fn install_is_used_once_and_a_failure_is_told_by_it() {
        let mut panel = panel();
        panel.set_updates(Some(offered("", Progress::Installing)));
        let help = "Downloading and checking it; hyalo restarts once it is in place";
        assert_eq!(
            help_of(&panel, "hyalo 0.0.2 is out"),
            What::Help {
                text: help.to_string(),
                alarm: false
            }
        );
        assert!(!panel.look(Target::Install).enabled);
        assert_eq!(click(&mut panel, Target::Install), None);
        let failed = Progress::Failed(hyalo_core::update::Failure::Mismatch);
        panel.set_updates(Some(offered("", failed)));
        let said = "! Not installed: the download is not the build its SHA-256 names";
        assert!(
            texts(&panel).contains(&said.to_string()),
            "{:?}",
            texts(&panel)
        );
        assert!(panel.look(Target::Install).enabled, "to try again");
        assert_eq!(click(&mut panel, Target::Install), Some(Effect::Install));
    }

    #[test]
    fn the_switch_turns_checking_the_other_way_and_a_refusal_shows_by_it() {
        let mut panel = panel();
        panel.set_updates(Some(newest()));
        let off = Change::CheckForUpdates(false);
        assert_eq!(
            click(&mut panel, Target::CheckUpdates),
            Some(Effect::Change(off.clone()))
        );
        let text = DEFAULT.replace("check_for_updates = true", "check_for_updates = false");
        panel.done(&off, Some(written(&text)));
        assert_eq!(switch(&panel, Target::CheckUpdates), Some(false));
        let on = Change::CheckForUpdates(true);
        assert_eq!(
            click(&mut panel, Target::CheckUpdates),
            Some(Effect::Change(on.clone()))
        );
        panel.refused(&on, Why::Unsaved("the disk is full".to_string()));
        assert_eq!(
            help_of(&panel, "Check for updates"),
            What::Help {
                text: "cannot save config.toml: the disk is full".to_string(),
                alarm: true
            }
        );
    }

    #[test]
    fn while_the_file_is_broken_install_still_works_but_the_switch_does_not() {
        let mut panel = panel();
        panel.set_updates(Some(offered("", Progress::Offered)));
        panel.reload(Err(broken()));
        assert_eq!(
            headers(&panel)[..2],
            ["Updates".to_string(), "Launcher".to_string()]
        );
        assert!(panel.look(Target::Install).enabled);
        assert!(!panel.look(Target::CheckUpdates).enabled);
        assert_eq!(click(&mut panel, Target::CheckUpdates), None);
        assert_eq!(click(&mut panel, Target::Install), Some(Effect::Install));
    }

    #[test]
    fn a_file_broken_from_the_start_shows_no_switch() {
        let mut panel = Panel::new(Err(broken()), None, Language::English);
        panel.set_updates(Some(newest()));
        assert_eq!(switch(&panel, Target::CheckUpdates), None);
        assert!(!headers(&panel).contains(&"Updates".to_string()));
    }

    #[test]
    fn tab_reaches_install_first_and_the_focus_leaves_it_when_the_release_goes() {
        let mut panel = panel();
        panel.set_updates(Some(offered("", Progress::Offered)));
        plain(&mut panel, VK_TAB.0);
        assert!(panel.look(Target::Install).ring);
        plain(&mut panel, VK_TAB.0);
        assert!(panel.look(Target::CheckUpdates).ring);
        plain(&mut panel, VK_TAB.0);
        assert!(
            panel.look(Target::Hotkey).ring,
            "then the launcher's settings"
        );
        let mut panel = Panel::new(Ok(written(DEFAULT)), None, Language::English);
        panel.set_updates(Some(offered("", Progress::Offered)));
        panel.press(Some(Target::Install));
        assert!(panel.look(Target::Install).focused);
        panel.set_updates(Some(newest()));
        assert!(!panel.look(Target::Install).focused);
        assert_eq!(
            plain(&mut panel, VK_RETURN.0),
            None,
            "nothing has the focus"
        );
    }

    #[test]
    fn the_updates_section_speaks_russian() {
        let mut panel = russian();
        panel.set_updates(Some(offered("", Progress::Offered)));
        assert_eq!(headers(&panel)[0], "Обновления");
        let shown = texts(&panel);
        assert!(
            shown.contains(&"Вышла hyalo 0.0.2".to_string()),
            "{shown:?}"
        );
        assert!(
            shown.contains(&"Проверять обновления".to_string()),
            "{shown:?}"
        );
        let help = "У вас 0.0.1: «Установить» скачает её, проверит и перезапустит hyalo";
        assert_eq!(
            help_of(&panel, "Вышла hyalo 0.0.2"),
            What::Help {
                text: help.to_string(),
                alarm: false
            }
        );
        let marks = laid(&panel).marks;
        let install = marks.iter().find_map(|mark| match &mark.what {
            What::Button { label, target } if *target == Target::Install => Some(label.clone()),
            _ => None,
        });
        assert_eq!(install.as_deref(), Some("Установить"));
    }
}
