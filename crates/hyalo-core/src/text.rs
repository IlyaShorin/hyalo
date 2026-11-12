//! What hyalo says, in the language the user chose: one table with every
//! sentence in English and in Russian. A sentence without its Russian
//! does not compile: the table is written through a macro that wants
//! both. Words that fill a sentence go in its `{0}`, `{1}`, ...; the
//! Russian may put them in another order.
//!
//! Names stay as they are in either language: hyalo, config.toml, the
//! keys and the config's settings, files, apps.

/// A language hyalo speaks.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub enum Language {
    #[default]
    English,
    Russian,
}

impl Language {
    /// Every language, in the order the settings window offers them.
    pub const ALL: [Language; 2] = [Language::English, Language::Russian];

    /// Its name in the config.
    pub fn code(self) -> &'static str {
        NAMES[self as usize].0
    }

    /// The language the config names, in any case.
    pub fn parse(code: &str) -> Option<Language> {
        let code = code.trim();
        Language::ALL
            .into_iter()
            .find(|language| language.code().eq_ignore_ascii_case(code))
    }

    /// The language a config without one speaks: Russian when Windows
    /// shows itself in Russian (`language`, a LANGID from
    /// `GetUserDefaultUILanguage`, of any Russian locale), else English.
    pub fn of_windows(language: u16) -> Language {
        match language & PRIMARY {
            RUSSIAN => Language::Russian,
            _ => Language::English,
        }
    }

    /// What the language calls itself, the same in either language.
    pub fn name(self) -> &'static str {
        NAMES[self as usize].1
    }

    /// `text` in this language.
    pub fn say(self, text: Text) -> &'static str {
        TABLE[text as usize][self as usize]
    }

    /// `text` in this language, with `words` in its places.
    pub fn fill(self, text: Text, words: &[&str]) -> String {
        fill(self.say(text), words)
    }
}

/// Each language's code in the config and what it calls itself, in
/// `Language`'s order.
const NAMES: [(&str, &str); 2] = [("en", "English"), ("ru", "Русский")];

/// The primary language of a LANGID, and Russian's.
const PRIMARY: u16 = 0x3FF;
const RUSSIAN: u16 = 0x19;

/// `sentence` with each `{n}` in it replaced by `words[n]`, in one pass,
/// so a word holding braces is left as it is.
fn fill(sentence: &str, words: &[&str]) -> String {
    let mut out = String::with_capacity(sentence.len());
    let mut rest = sentence;
    while let Some(open) = rest.find('{') {
        let (before, from) = rest.split_at(open);
        out.push_str(before);
        let place = from[1..].split_once('}').and_then(|(number, after)| {
            let word = words.get(number.parse::<usize>().ok()?)?;
            Some((word, after))
        });
        match place {
            Some((word, after)) => {
                out.push_str(word);
                rest = after;
            }
            None => {
                out.push('{');
                rest = &from[1..];
            }
        }
    }
    out.push_str(rest);
    out
}

/// Writes `Text`, one name per sentence, and the table of its English and
/// Russian, so neither can be left out.
macro_rules! table {
    ($($(#[$doc:meta])* $key:ident => $english:literal, $russian:literal;)+) => {
        /// Every sentence hyalo shows, by what it says.
        #[derive(Debug, Clone, Copy, PartialEq, Eq)]
        pub enum Text {
            $($(#[$doc])* $key,)+
        }

        impl Text {
            /// Every sentence, in the table's order.
            pub const ALL: &[Text] = &[$(Text::$key,)+];
        }

        /// Each sentence in English and in Russian, in `Text`'s order.
        const TABLE: &[[&str; 2]] = &[$([$english, $russian],)+];
    };
}

table! {
    // The launcher.
    /// The search bar while nothing is typed.
    Placeholder => "Search apps, files, settings", "Поиск приложений, файлов и настроек";
    /// The second line of an app that has no path to show.
    App => "App", "Приложение";
    Calculator => "Calculator · Enter to copy", "Калькулятор · Enter — скопировать";
    /// The second line of a page of Windows' Settings app.
    WindowsSettings => "Settings", "Параметры Windows";
    /// The second line of a Control Panel item.
    ControlPanel => "Control Panel", "Панель управления";
    /// The second line of a system command.
    SystemCommand => "System command", "Системная команда";
    Lock => "Lock", "Заблокировать";
    Sleep => "Sleep", "Спящий режим";
    Hibernate => "Hibernate", "Гибернация";
    Restart => "Restart", "Перезагрузка";
    ShutDown => "Shut down", "Завершение работы";
    SignOut => "Sign out", "Выйти";
    EmptyRecycleBin => "Empty Recycle Bin", "Очистить корзину";
    /// A system command's second line after the first Enter.
    AskSleep => "Press Enter again to sleep",
        "Нажмите Enter ещё раз, чтобы перевести компьютер в спящий режим";
    AskHibernate => "Press Enter again to hibernate",
        "Нажмите Enter ещё раз, чтобы перевести компьютер в гибернацию";
    AskRestart => "Press Enter again to restart",
        "Нажмите Enter ещё раз, чтобы перезагрузить компьютер";
    AskShutDown => "Press Enter again to shut down",
        "Нажмите Enter ещё раз, чтобы выключить компьютер";
    AskSignOut => "Press Enter again to sign out",
        "Нажмите Enter ещё раз, чтобы выйти из системы";
    AskEmptyRecycleBin => "Press Enter again to empty the Recycle Bin",
        "Нажмите Enter ещё раз, чтобы очистить корзину";
    /// An app's second line after Shift+Delete: what is uninstalled.
    AskUninstall => "Press Enter to uninstall {0}", "Нажмите Enter, чтобы удалить {0}";
    /// An amount, its currency, the target, the rates' date.
    Currency => "{0} {1} → {2} · Bank of Russia, {3} · Enter to copy",
        "{0} {1} → {2} · Банк России, {3} · Enter — скопировать";
    /// A web search engine, and the words it is asked for.
    SearchWeb => "Search {0} for “{1}”", "Искать в {0}: «{1}»";
    WebSearch => "Web search · Enter to open in the browser",
        "Поиск в интернете · Enter — открыть в браузере";

    // A row's menu: what Enter does with the row, then what other keys do.
    Open => "Open", "Открыть";
    Copy => "Copy", "Копировать";
    Run => "Run", "Выполнить";
    SwitchTo => "Switch to", "Перейти";
    RunAsAdministrator => "Run as administrator", "Запуск от имени администратора";
    ShowInFolder => "Show in folder", "Показать в папке";
    CopyPath => "Copy path", "Копировать путь";
    /// It asks first, as Shift+Delete does.
    Uninstall => "Uninstall…", "Удалить…";

    // The tray.
    Show => "Show hyalo", "Показать hyalo";
    Settings => "Settings", "Параметры";
    StartWithWindows => "Start with Windows", "Запускать вместе с Windows";
    Exit => "Exit", "Выход";

    // Notices: under the empty search bar and in the tray's tooltip.
    /// What is wrong in the config, with its line.
    NoticeConfig => "config.toml, {0}", "config.toml, {0}";
    /// A line of the config, and what is wrong there.
    ProblemAt => "line {0}: {1}", "строка {0}: {1}";
    /// The hotkey refused, and the one that still shows hyalo.
    TakenKept => "{0} is taken: {1} still shows hyalo",
        "{0} уже занято: hyalo по-прежнему открывается по {1}";
    Taken => "{0} is taken by another app", "{0} уже занято другим приложением";
    KeepsSettings => "hyalo keeps its last good settings",
        "Действуют последние исправные настройки";
    ChooseHotkey => "Choose another hotkey", "Выберите другое сочетание клавиш";
    /// What to do about a notice.
    OpenSettings => "{0} · Enter to open settings", "{0} · Enter — открыть параметры";
    /// A newer release, by its version; also the name of its setting.
    UpdateOut => "hyalo {0} is out", "Вышла hyalo {0}";
    SeeWhatsNew => "See what is new and install it", "Посмотрите, что нового, и установите";
    /// The release that did not install, and why.
    NotInstalled => "hyalo {0} is not installed: {1}", "hyalo {0} не установлена: {1}";
    /// The version that runs on.
    KeepsVersion => "hyalo {0} keeps running", "Работает прежняя hyalo {0}";
    /// The tray's line for a newer release, and while it installs.
    InstallUpdate => "Install hyalo {0}", "Установить hyalo {0}";
    Installing => "Installing hyalo {0}…", "Устанавливается hyalo {0}…";

    // The settings window.
    WindowTitle => "hyalo settings", "Параметры hyalo";
    Problem => "config.toml has a problem", "В config.toml ошибка";
    KeepsUntilFixed => "hyalo keeps its last good settings until it is fixed",
        "Пока она не исправлена, действуют последние исправные настройки";
    DefaultsUntilFixed => "hyalo runs on its defaults until it is fixed",
        "Пока она не исправлена, действуют настройки по умолчанию";
    OpenConfig => "Open config.toml", "Открыть config.toml";
    /// A line of the config, and what is wrong there, on its own line.
    Line => "Line {0}: {1}", "Строка {0}: {1}";
    Launcher => "Launcher", "Основные";
    Hotkey => "Hotkey", "Сочетание клавиш";
    HotkeyHelp => "Shows and hides the launcher: click, then press the keys",
        "Показывает и скрывает панель поиска: щёлкните и нажмите клавиши";
    Listening => "Press the new combination; Esc keeps the one you have",
        "Нажмите новое сочетание; Esc оставит текущее";
    PressKeys => "Press keys", "Нажмите клавиши";
    StartsInTray => "Starts hyalo in the tray when you sign in",
        "hyalo запускается в области уведомлений при входе в систему";
    LanguageName => "Language", "Язык";
    LanguageHelp => "For this window, the tray menu and the launcher's rows",
        "Для этого окна, меню в области уведомлений и строк поиска";
    Folders => "Folders", "Папки";
    Exclude => "Leave out of the index", "Не индексировать";
    ExcludeHelp => "Full paths: the file index leaves out these folders and all in them",
        "Полные пути: эти папки и всё в них не попадают в индекс файлов";
    RankLower => "Rank lower", "Показывать ниже";
    RankLowerHelp => "Files in these rank lower: a folder name, wherever it is, or a full path",
        "Файлы в них ниже в поиске: имя папки (где угодно) или полный путь";
    RankHigher => "Rank higher", "Показывать выше";
    RankHigherHelp => "Files in these rank higher, as in Desktop, Documents and Downloads",
        "Файлы в них выше в поиске, как в «Документах» и «Загрузках»";
    AddFolder => "Add folder…", "Добавить папку…";
    Add => "Add", "Добавить";
    /// The example of a folder name.
    FolderName => "A folder name, such as {0}", "Имя папки, например {0}";
    Web => "Web search", "Поиск в интернете";
    Searches => "Search prefixes", "Префиксы поиска";
    SearchesHelp => "Type a prefix, a space and words; × takes a search out",
        "Префикс, пробел и слова; × убирает поиск";
    Engine => "A prefix and an address with {query}", "Префикс и адрес с {query}";
    EditFile => "Edit the file instead", "Редактировать файл напрямую";
    EditFileHelp => "Changes here are saved to it at once, and what you save there shows up here",
        "Изменения отсюда сразу попадают в файл, а из файла — сюда";
    PickExclude => "Leave a folder out of the index", "Папка, которую не индексировать";
    PickLower => "Rank a folder lower", "Папка, файлы из которой показывать ниже";
    PickHigher => "Rank a folder higher", "Папка, файлы из которой показывать выше";
    Updates => "Updates", "Обновления";
    /// The version that runs.
    InstallHelp => "You have {0}: Install downloads it, checks it and restarts hyalo",
        "У вас {0}: «Установить» скачает её, проверит и перезапустит hyalo";
    InstallingHelp => "Downloading and checking it; hyalo restarts once it is in place",
        "Скачивается и проверяется; затем hyalo перезапустится";
    /// Why the update did not install.
    FailedHelp => "Not installed: {0}", "Не установлена: {0}";
    Install => "Install", "Установить";
    CheckUpdates => "Check for updates", "Проверять обновления";
    /// The version that runs.
    CheckHelp => "You have hyalo {0}; GitHub is asked once a day, nothing installs by itself",
        "У вас hyalo {0}; раз в день спрашивает GitHub, сама ничего не ставит";

    // What can be wrong.
    /// Why the file could not be saved, as Windows says it.
    Unsaved => "cannot save config.toml: {0}", "не удалось сохранить config.toml: {0}";
    Unreadable => "config.toml cannot be read", "не удаётся прочитать config.toml";
    NotUtf8 => "this is not UTF-8 text: save the file as UTF-8",
        "это не текст в UTF-8: сохраните файл в кодировке UTF-8";
    Escape => "a backslash in double quotes starts an escape: write paths in single quotes, as 'C:\\Windows'",
        "обратная косая черта в двойных кавычках начинает управляющую последовательность: пишите пути в одинарных кавычках, например 'C:\\Windows'";
    NotFullPath => "`{0}` is not a full path such as `D:\\old`",
        "`{0}` — не полный путь вроде `D:\\old`";
    NotPlace => "`{0}` is neither a folder name nor a full path such as `D:\\old`",
        "`{0}` — не имя папки и не полный путь вроде `D:\\old`";
    NotLanguage => "`{0}` is not a language hyalo speaks: \"en\" or \"ru\"",
        "hyalo не знает языка `{0}`: только \"en\" или \"ru\"";
    NotPrefix => "`{0}` is not a prefix: one word, not starting with >",
        "`{0}` — не префикс: нужно одно слово, и не с >";
    NotAddress => "`{0}` is not a web address with {query} where the words go",
        "`{0}` — не веб-адрес с {query} на месте слов";
    NotEngine => "`{0}` is not a prefix and an address, such as `so https://stackoverflow.com/search?q={query}`",
        "`{0}` — не префикс с адресом вроде `so https://stackoverflow.com/search?q={query}`";
    Twice => "`{0}` twice", "`{0}` указано дважды";
    Unnamed => "hyalo has no name for that key: choose another",
        "у этой клавиши нет имени в hyalo: выберите другую";
    UnknownKey => "`{0}` is not a key hyalo knows", "hyalo не знает клавишу `{0}`";
    TwoKeys => "two keys, `{0}` and `{1}`: a hotkey has one",
        "две клавиши, `{0}` и `{1}`, а в сочетании нужна одна";
    EmptyHotkey => "the hotkey is empty", "сочетание клавиш не задано";
    NoKey => "no key after the modifiers in `{0}`", "в `{0}` нет клавиши после модификаторов";
    Bare => "`{0}` needs Ctrl, Alt or Win: only F1 to F24 do without",
        "к `{0}` нужно добавить Ctrl, Alt или Win: без них обходятся только F1–F24";
    /// Why an update was not installed; the reasons as Windows says them.
    DownloadFailed => "cannot download it: {0}", "не удалось скачать: {0}";
    Mismatch => "the download is not the build its SHA-256 names",
        "скачанный файл не совпадает со своей SHA-256";
    SwapFailed => "cannot put it in place: {0}", "не удалось заменить файл: {0}";
    StartFailed => "cannot start it: {0}", "не удалось запустить: {0}";
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The places a sentence has for words: `{0}`, `{1}`, ..., sorted.
    fn places(sentence: &str) -> Vec<String> {
        let mut places: Vec<String> = sentence
            .match_indices('{')
            .filter_map(|(at, _)| {
                let (number, _) = sentence[at + 1..].split_once('}')?;
                number.parse::<usize>().ok().map(|_| number.to_string())
            })
            .collect();
        places.sort();
        places
    }

    #[test]
    fn every_sentence_is_said_in_both_languages_with_the_same_places() {
        for &text in Text::ALL {
            let (english, russian) = (Language::English.say(text), Language::Russian.say(text));
            assert!(!english.is_empty() && !russian.is_empty(), "{text:?}");
            assert_eq!(places(english), places(russian), "{text:?}");
        }
        assert_eq!(Text::ALL.len(), TABLE.len());
    }

    #[test]
    fn each_sentence_is_found_by_its_name() {
        assert_eq!(Language::English.say(Text::Exit), "Exit");
        assert_eq!(Language::Russian.say(Text::Exit), "Выход");
        assert_eq!(Language::Russian.say(Text::Settings), "Параметры");
        assert_eq!(
            Language::English.say(Text::Placeholder),
            "Search apps, files, settings"
        );
    }

    #[test]
    fn words_go_in_their_places_in_either_order() {
        let said = Language::Russian.fill(Text::TakenKept, &["Ctrl+K", "Alt+Space"]);
        assert_eq!(
            said,
            "Ctrl+K уже занято: hyalo по-прежнему открывается по Alt+Space"
        );
        assert_eq!(fill("{1} and {0}", &["a", "b"]), "b and a");
        assert_eq!(fill("{0}{0}", &["ab"]), "abab");
    }

    #[test]
    fn braces_that_are_no_place_stay_as_they_are() {
        assert_eq!(fill("a {x} {9} {", &["w"]), "a {x} {9} {");
        assert_eq!(fill("{0}", &["{1}"]), "{1}", "a word is never filled in");
        assert_eq!(fill("no places", &[]), "no places");
        assert_eq!(fill("", &["w"]), "");
    }

    #[test]
    fn a_language_goes_by_its_code_in_the_config() {
        for language in Language::ALL {
            assert_eq!(Language::parse(language.code()), Some(language));
        }
        assert_eq!(Language::parse("RU"), Some(Language::Russian));
        assert_eq!(Language::parse(" en "), Some(Language::English));
        assert_eq!(Language::parse("de"), None);
        assert_eq!(Language::parse(""), None);
        assert_eq!(Language::default(), Language::English);
    }

    #[test]
    fn windows_in_russian_means_russian_and_anything_else_english() {
        for russian in [0x0419, 0x0819] {
            assert_eq!(
                Language::of_windows(russian),
                Language::Russian,
                "{russian:#06x}"
            );
        }
        for other in [0x0409, 0x0809, 0x0422, 0x0423, 0x0418, 0x0018, 0] {
            assert_eq!(
                Language::of_windows(other),
                Language::English,
                "{other:#06x}"
            );
        }
    }

    #[test]
    fn each_language_calls_itself_by_its_own_name() {
        assert_eq!(Language::English.name(), "English");
        assert_eq!(Language::Russian.name(), "Русский");
    }
}
