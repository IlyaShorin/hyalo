//! Windows' own settings: the pages of the Settings app (`ms-settings:`)
//! and the classic Control Panel's items, from a table.
//!
//! A table rather than the shell's Control Panel folder: the shell names
//! its items only in the language Windows is shown in, and `device
//! manager` should find Device Manager on a Russian Windows too; some of
//! the most wanted (Environment Variables, Network Connections) are no
//! items of that folder at all. The icons still come from the shell.
//!
//! Each setting has its name as Windows 11 shows it in English and in
//! Russian and a few words people search it by. The query matches all of
//! them whatever language hyalo speaks; the row shows the name in hyalo's.

use crate::item::{Action, Item, ItemId, Kind};
use crate::text::{Language, Text};

/// One of Windows' settings: where it is and what it is called.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Setting {
    /// What Enter opens; also the setting's frecency key.
    pub opens: Opens,
    /// The shell item whose icon its row shows (a parsing name).
    pub icon: &'static str,
    pub panel: Panel,
    /// The name in English and in Russian, in `Language`'s order.
    names: [&'static str; 2],
    /// Words people search it by, in either language.
    keywords: &'static [&'static str],
}

/// How Windows opens a setting.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Opens {
    /// Through the shell: an `ms-settings:` page, a `shell:::{…}` item.
    Shell(&'static str),
    /// A program in the system folder with its arguments.
    Program(&'static str, &'static str),
}

/// Where a setting lives, as its row's second line says.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Panel {
    /// The Settings app.
    Settings,
    ControlPanel,
}

impl Setting {
    /// Its frecency key: what it opens, as one line.
    pub fn id(&self) -> ItemId {
        ItemId(match self.opens {
            Opens::Shell(target) => target.to_string(),
            Opens::Program(program, arguments) => format!("{program} {arguments}"),
        })
    }

    /// Its row in `language`: the name in that language, the other name
    /// and the keywords to find it by, and where it lives.
    pub fn item(&self, language: Language) -> Item {
        let title = self.names[language as usize];
        let other = self.names.iter().filter(|name| **name != title);
        let keywords = other.chain(self.keywords).map(|name| name.to_string());
        Item {
            id: self.id(),
            title: title.to_string(),
            subtitle: language.say(self.panel.text()).to_string(),
            keywords: keywords.collect(),
            kind: Kind::Setting,
            action: match self.opens {
                Opens::Shell(target) => Action::Open(target.to_string()),
                Opens::Program(program, arguments) => Action::Launch {
                    program: program.to_string(),
                    arguments: arguments.to_string(),
                },
            },
        }
    }
}

impl Panel {
    fn text(self) -> Text {
        match self {
            Panel::Settings => Text::WindowsSettings,
            Panel::ControlPanel => Text::ControlPanel,
        }
    }
}

/// Every setting's row, in `language`.
pub fn items(language: Language) -> Vec<Item> {
    SETTINGS
        .iter()
        .map(|setting| setting.item(language))
        .collect()
}

/// The Settings app, whose icon every page of it shows.
pub const SETTINGS_APP: &str = r"shell:AppsFolder\windows.immersivecontrolpanel_cw5n1h2txyewy!microsoft.windows.immersivecontrolpanel";

/// A page of the Settings app.
const fn page(
    uri: &'static str,
    english: &'static str,
    russian: &'static str,
    keywords: &'static [&'static str],
) -> Setting {
    Setting {
        opens: Opens::Shell(uri),
        icon: SETTINGS_APP,
        panel: Panel::Settings,
        names: [english, russian],
        keywords,
    }
}

/// A Control Panel item the shell opens, with its own icon.
const fn item(
    target: &'static str,
    english: &'static str,
    russian: &'static str,
    keywords: &'static [&'static str],
) -> Setting {
    let opened = page(target, english, russian, keywords);
    Setting {
        icon: target,
        panel: Panel::ControlPanel,
        ..opened
    }
}

/// A Control Panel dialog a program opens, with the icon of the item it
/// belongs to.
const fn program(
    (program, arguments): (&'static str, &'static str),
    icon: &'static str,
    english: &'static str,
    russian: &'static str,
    keywords: &'static [&'static str],
) -> Setting {
    let shown = item(icon, english, russian, keywords);
    Setting {
        opens: Opens::Program(program, arguments),
        ..shown
    }
}

/// The Control Panel's System item.
const SYSTEM: &str = "shell:::{BB06C0E4-D293-4f75-8A90-CB05B6477EEE}";

/// Programs and Features.
const PROGRAMS: &str = "shell:::{7b81be6a-ce2b-4676-a29e-eb907a5126c5}";

/// This PC.
const THIS_PC: &str = "shell:::{20D04FE0-3AEA-1069-A2D8-08002B30309D}";

/// Every setting, Settings pages first, then the Control Panel's items;
/// among equal matches the first comes first.
#[rustfmt::skip]
pub const SETTINGS: &[Setting] = &[
    // System.
    page("ms-settings:display", "Display", "Дисплей",
        &["brightness", "resolution", "scale", "monitor", "screen", "hdr",
          "яркость", "разрешение экрана", "масштаб", "монитор", "экран"]),
    page("ms-settings:nightlight", "Night light", "Ночной свет",
        &["blue light", "warm", "ночной режим", "синий свет"]),
    page("ms-settings:sound", "Sound", "Звук",
        &["volume", "speakers", "headphones", "microphone", "audio output",
          "громкость", "динамики", "наушники", "микрофон"]),
    page("ms-settings:apps-volume", "Volume mixer", "Микшер громкости",
        &["mixer", "app volume", "микшер", "громкость приложений"]),
    page("ms-settings:notifications", "Notifications", "Уведомления",
        &["do not disturb", "toasts", "не беспокоить"]),
    page("ms-settings:powersleep", "Power & battery", "Питание и батарея",
        &["power", "battery", "sleep", "screen timeout", "power mode", "energy saver",
          "питание", "батарея", "аккумулятор", "сон", "энергосбережение", "режим питания"]),
    page("ms-settings:storagesense", "Storage", "Память",
        &["disk space", "storage sense", "cleanup", "free up space",
          "хранилище", "место на диске", "контроль памяти", "очистка диска"]),
    page("ms-settings:multitasking", "Multitasking", "Многозадачность",
        &["snap windows", "alt+tab", "virtual desktops", "прикрепление окон",
          "виртуальные рабочие столы"]),
    page("ms-settings:clipboard", "Clipboard", "Буфер обмена",
        &["clipboard history", "журнал буфера обмена"]),
    page("ms-settings:remotedesktop", "Remote Desktop", "Удаленный рабочий стол",
        &["rdp", "remote access", "удаленный доступ"]),
    page("ms-settings:about", "About", "О системе",
        &["system info", "pc name", "rename this pc", "windows version", "specifications",
          "сведения о системе", "имя компьютера", "переименовать компьютер",
          "версия windows", "характеристики"]),
    page("ms-settings:troubleshoot", "Troubleshoot", "Устранение неполадок",
        &["troubleshooter", "fix problems", "исправить неполадки"]),
    page("ms-settings:recovery", "Recovery", "Восстановление",
        &["reset this pc", "advanced startup", "вернуть компьютер в исходное состояние",
          "особые варианты загрузки", "сброс"]),
    page("ms-settings:activation", "Activation", "Активация",
        &["product key", "license", "ключ продукта", "лицензия"]),
    page("ms-settings:optionalfeatures", "Optional features", "Дополнительные компоненты",
        &["features", "компоненты"]),
    page("ms-settings:developers", "For developers", "Для разработчиков",
        &["developer mode", "sudo", "режим разработчика"]),

    // Bluetooth & devices.
    page("ms-settings:bluetooth", "Bluetooth & devices", "Bluetooth и устройства",
        &["bluetooth", "devices", "pair", "add device",
          "блютуз", "устройства", "сопряжение", "добавить устройство"]),
    page("ms-settings:printers", "Printers & scanners", "Принтеры и сканеры",
        &["printer", "scanner", "print", "принтер", "сканер", "печать"]),
    page("ms-settings:mousetouchpad", "Mouse", "Мышь",
        &["pointer speed", "scroll", "primary button", "скорость указателя", "прокрутка"]),
    page("ms-settings:devices-touchpad", "Touchpad", "Сенсорная панель",
        &["trackpad", "gestures", "тачпад", "жесты"]),
    page("ms-settings:typing", "Typing", "Ввод",
        &["autocorrect", "touch keyboard", "spelling", "автозамена", "сенсорная клавиатура",
          "орфография"]),
    page("ms-settings:autoplay", "AutoPlay", "Автозапуск",
        &["autorun", "removable drives", "съемные носители"]),
    page("ms-settings:usb", "USB", "USB", &[]),

    // Network & internet.
    page("ms-settings:network", "Network & internet", "Сеть и Интернет",
        &["internet", "network", "connection", "интернет", "сеть", "подключение"]),
    page("ms-settings:network-wifi", "Wi-Fi", "Wi-Fi",
        &["wifi", "wireless", "wlan", "вайфай", "вай-фай", "беспроводная сеть"]),
    page("ms-settings:network-ethernet", "Ethernet", "Ethernet",
        &["cable", "lan", "wired", "кабель", "проводная сеть"]),
    page("ms-settings:network-vpn", "VPN", "VPN", &[]),
    page("ms-settings:network-mobilehotspot", "Mobile hotspot", "Мобильный хот-спот",
        &["hotspot", "share internet", "точка доступа", "раздать интернет"]),
    page("ms-settings:network-airplanemode", "Airplane mode", "Режим «в самолете»",
        &["flight mode", "режим полета", "в самолете", "авиарежим"]),
    page("ms-settings:network-proxy", "Proxy", "Прокси-сервер",
        &["proxy server", "прокси"]),
    page("ms-settings:network-advancedsettings", "Advanced network settings",
        "Дополнительные сетевые параметры",
        &["network adapters", "network reset", "сетевые адаптеры", "сброс сети"]),

    // Personalization.
    page("ms-settings:personalization", "Personalization", "Персонализация",
        &["customize", "оформление"]),
    page("ms-settings:personalization-background", "Background", "Фон",
        &["wallpaper", "desktop background", "обои", "фон рабочего стола"]),
    page("ms-settings:colors", "Colors", "Цвета",
        &["dark mode", "light mode", "accent color", "transparency",
          "темный режим", "темная тема", "светлая тема", "цвет акцента", "прозрачность"]),
    page("ms-settings:themes", "Themes", "Темы",
        &["theme", "desktop icons", "тема", "значки рабочего стола"]),
    page("ms-settings:lockscreen", "Lock screen", "Экран блокировки",
        &["lock screen picture", "screen saver", "заставка"]),
    page("ms-settings:personalization-start", "Start", "Пуск",
        &["start menu", "меню пуск"]),
    page("ms-settings:taskbar", "Taskbar", "Панель задач",
        &["tray", "system tray", "taskbar alignment", "область уведомлений"]),
    page("ms-settings:fonts", "Fonts", "Шрифты", &["font", "шрифт"]),

    // Apps.
    page("ms-settings:appsfeatures", "Installed apps", "Установленные приложения",
        &["uninstall", "remove app", "programs", "apps & features",
          "удалить программу", "удаление программ", "программы"]),
    page("ms-settings:defaultapps", "Default apps", "Приложения по умолчанию",
        &["default browser", "open with", "file associations",
          "браузер по умолчанию", "открывать с помощью", "сопоставление файлов"]),
    page("ms-settings:startupapps", "Startup", "Автозагрузка",
        &["startup apps", "autostart", "run at startup", "автозапуск приложений",
          "запуск при входе"]),

    // Accounts.
    page("ms-settings:yourinfo", "Your info", "Ваши данные",
        &["account", "profile picture", "учетная запись", "аккаунт", "аватар"]),
    page("ms-settings:signinoptions", "Sign-in options", "Варианты входа",
        &["password", "pin", "windows hello", "fingerprint", "face",
          "пароль", "пин-код", "отпечаток пальца", "распознавание лица"]),
    page("ms-settings:emailandaccounts", "Email & accounts",
        "Электронная почта и учетные записи",
        &["email", "mail accounts", "почта"]),
    page("ms-settings:otherusers", "Other users", "Другие пользователи",
        &["add user", "family", "добавить пользователя", "семья"]),
    page("ms-settings:backup", "Windows Backup", "Архивация Windows",
        &["backup", "onedrive", "резервное копирование"]),

    // Time & language.
    page("ms-settings:dateandtime", "Date & time", "Дата и время",
        &["time zone", "clock", "set time", "часовой пояс", "часы", "время"]),
    page("ms-settings:regionlanguage", "Language & region", "Язык и регион",
        &["display language", "keyboard layout", "input language", "region",
          "язык интерфейса", "раскладка клавиатуры", "язык ввода", "регион"]),
    page("ms-settings:speech", "Speech", "Речь",
        &["voice", "speech recognition", "голос", "распознавание речи"]),

    // Gaming.
    page("ms-settings:gaming-gamemode", "Game Mode", "Игровой режим",
        &["games", "gaming", "игры"]),

    // Accessibility.
    page("ms-settings:easeofaccess", "Accessibility", "Специальные возможности",
        &["ease of access", "a11y"]),
    page("ms-settings:easeofaccess-display", "Text size", "Размер текста",
        &["font size", "bigger text", "размер шрифта", "крупный текст"]),
    page("ms-settings:easeofaccess-mousepointer", "Mouse pointer and touch",
        "Указатель мыши и сенсорный ввод",
        &["cursor size", "pointer size", "размер курсора", "размер указателя"]),
    page("ms-settings:easeofaccess-magnifier", "Magnifier", "Экранная лупа",
        &["zoom", "лупа", "увеличение"]),
    page("ms-settings:easeofaccess-narrator", "Narrator", "Экранный диктор",
        &["screen reader", "диктор"]),
    page("ms-settings:easeofaccess-colorfilter", "Color filters", "Цветовые фильтры",
        &["grayscale", "color blind", "оттенки серого", "дальтонизм"]),

    // Privacy & security.
    page("ms-settings:privacy", "Privacy & security", "Конфиденциальность и защита",
        &["privacy", "permissions", "конфиденциальность", "разрешения"]),
    page("ms-settings:privacy-location", "Location", "Расположение",
        &["gps", "location services", "геолокация", "местоположение"]),
    page("ms-settings:privacy-webcam", "Camera", "Камера",
        &["webcam", "camera access", "веб-камера"]),
    page("ms-settings:privacy-microphone", "Microphone", "Микрофон",
        &["microphone access", "mic", "доступ к микрофону"]),
    page("ms-settings:findmydevice", "Find my device", "Поиск устройства",
        &["lost device", "потерянное устройство"]),

    // Windows Update.
    page("ms-settings:windowsupdate", "Windows Update", "Центр обновления Windows",
        &["updates", "check for updates", "update windows",
          "обновления", "обновление windows", "проверить наличие обновлений"]),
    page("ms-settings:windowsupdate-history", "Update history", "Журнал обновлений",
        &["installed updates", "uninstall updates", "установленные обновления"]),
    page("ms-settings:windowsinsider", "Windows Insider Program",
        "Программа предварительной оценки Windows",
        &["insider", "preview builds", "инсайдер"]),

    // The Control Panel.
    item("shell:::{74246bfc-4c96-11d0-abef-0020af6b0b7a}", "Device Manager",
        "Диспетчер устройств",
        &["devices", "drivers", "hardware", "devmgmt", "драйверы", "оборудование",
          "устройства"]),
    program(("rundll32.exe", "sysdm.cpl,EditEnvironmentVariables"), SYSTEM,
        "Environment Variables", "Переменные среды",
        &["path", "env", "system variables", "переменные окружения",
          "системные переменные"]),
    program(("control.exe", "sysdm.cpl"), SYSTEM, "System Properties", "Свойства системы",
        &["sysdm", "computer name", "advanced system settings", "performance",
          "system protection", "remote settings", "имя компьютера",
          "дополнительные параметры системы", "быстродействие", "защита системы",
          "удаленный доступ"]),
    item(PROGRAMS, "Programs and Features", "Программы и компоненты",
        &["appwiz", "uninstall a program", "удаление программ", "удалить программу"]),
    program(("optionalfeatures.exe", ""), PROGRAMS,
        "Turn Windows features on or off", "Включение или отключение компонентов Windows",
        &["windows features", "hyper-v", "wsl", ".net framework", "компоненты windows"]),
    item("shell:::{025A5937-A6BE-4686-A844-36FE4BEC8B6D}", "Power Options", "Электропитание",
        &["power plan", "powercfg", "lid", "power button", "план электропитания",
          "схема электропитания", "кнопка питания", "крышка"]),
    item("shell:::{F2DDFC82-8F12-4CDD-B7DC-D4FE1425AA4D}", "Sound", "Звук",
        &["mmsys", "playback devices", "recording devices", "sounds",
          "устройства воспроизведения", "записывающие устройства"]),
    item("shell:::{7007ACC7-3202-11D1-AAD2-00805FC1270E}", "Network Connections",
        "Сетевые подключения",
        &["ncpa", "network adapters", "adapter settings", "сетевые адаптеры",
          "параметры адаптера"]),
    item("shell:::{8E908FC9-BECC-40f6-915B-F4CA0E70D03D}", "Network and Sharing Center",
        "Центр управления сетями и общим доступом",
        &["sharing", "network center", "общий доступ"]),
    item("shell:::{1206F5F1-0569-412C-8FEC-3204630DFB70}", "Credential Manager",
        "Диспетчер учетных данных",
        &["passwords", "saved passwords", "credentials", "сохраненные пароли",
          "учетные данные"]),
    item("shell:::{BD84B380-8CA2-1069-AB1D-08000948F534}", "Fonts", "Шрифты",
        &["font", "шрифт"]),
    item("shell:::{6C8EEC18-8D75-41B2-A177-8831D59D2D50}", "Mouse", "Мышь",
        &["main.cpl", "double-click speed", "pointers", "cursor",
          "скорость двойного щелчка", "указатели", "курсор"]),
    item("shell:::{725BE8F7-668E-4C7B-8F90-46BDB0936430}", "Keyboard", "Клавиатура",
        &["repeat rate", "repeat delay", "cursor blink", "скорость повтора",
          "задержка повтора"]),
    item("shell:::{62D8ED13-C9D0-4CE8-A914-47DD628FB1B0}", "Region", "Региональные стандарты",
        &["intl.cpl", "formats", "date format", "decimal separator", "system locale",
          "формат даты", "разделитель", "язык системы", "регион"]),
    item("shell:::{60632754-c523-4b62-b45c-4172da012619}", "User Accounts",
        "Учетные записи пользователей",
        &["users", "uac", "user account control", "пользователи",
          "контроль учетных записей"]),
    item("shell:::{6DFD7C5C-2451-11d3-A299-00C04F8EF6AF}", "File Explorer Options",
        "Параметры Проводника",
        &["folder options", "hidden files", "file extensions", "show hidden",
          "параметры папок", "скрытые файлы", "расширения файлов"]),
    item("shell:::{A3DD4F92-658A-410F-84FD-6FBBBEF2FFFE}", "Internet Options",
        "Свойства браузера",
        &["inetcpl", "internet properties", "свойства интернета"]),
    item("shell:::{4026492F-2F69-46B8-B9BF-5654FC07E423}", "Windows Defender Firewall",
        "Брандмауэр Защитника Windows",
        &["firewall", "брандмауэр", "файрвол", "фаервол"]),
    item("shell:::{A8A91A66-3A7D-4424-8D24-04E180695C7A}", "Devices and Printers",
        "Устройства и принтеры",
        &["printers", "принтеры"]),
    item("shell:::{D20EA4E1-3957-11d2-A40B-0C5020524153}", "Windows Tools",
        "Инструменты Windows",
        &["administrative tools", "администрирование", "средства администрирования"]),
    item("shell:::{87D66A43-7B11-4A28-9811-C86EE395ACF7}", "Indexing Options",
        "Параметры индексирования",
        &["search index", "индекс поиска"]),
    item("shell:::{D9EF8727-CAC2-4e60-809E-86F80A666C91}", "BitLocker Drive Encryption",
        "Шифрование диска BitLocker",
        &["bitlocker", "encryption", "шифрование"]),
    program(("mmc.exe", "diskmgmt.msc"), THIS_PC, "Disk Management", "Управление дисками",
        &["diskmgmt", "partitions", "format disk", "разделы", "форматировать диск"]),
];

#[cfg(test)]
mod tests {
    use super::*;
    use crate::frecency::{Frecency, Timestamp};
    use crate::places::Places;
    use crate::rank::rank;

    /// The id and title of the first setting `query` finds in `language`.
    fn first(query: &str, language: Language) -> Option<(String, String)> {
        let items = items(language);
        let ranked = rank(
            query,
            &items,
            &Frecency::default(),
            &Places::default(),
            Timestamp(0),
        );
        let first = ranked.into_iter().next()?.item;
        Some((first.id.0, first.title))
    }

    fn finds(query: &str, id: &str, english: &str, russian: &str) {
        let found = |language| first(query, language);
        let expected = |title: &str| Some((id.to_string(), title.to_string()));
        assert_eq!(found(Language::English), expected(english), "{query}");
        assert_eq!(found(Language::Russian), expected(russian), "{query}");
    }

    #[test]
    fn settings_pages_are_found_in_either_language_and_named_in_hyalos() {
        let bluetooth = ("Bluetooth & devices", "Bluetooth и устройства");
        finds(
            "bluetooth",
            "ms-settings:bluetooth",
            bluetooth.0,
            bluetooth.1,
        );
        finds("display", "ms-settings:display", "Display", "Дисплей");
        finds("дисплей", "ms-settings:display", "Display", "Дисплей");
        finds("звук", "ms-settings:sound", "Sound", "Звук");
        let update = ("Windows Update", "Центр обновления Windows");
        finds("обновлени", "ms-settings:windowsupdate", update.0, update.1);
        let default = ("Default apps", "Приложения по умолчанию");
        finds(
            "default apps",
            "ms-settings:defaultapps",
            default.0,
            default.1,
        );
    }

    #[test]
    fn keywords_find_a_page_by_what_people_call_it() {
        finds("wifi", "ms-settings:network-wifi", "Wi-Fi", "Wi-Fi");
        finds("яркость", "ms-settings:display", "Display", "Дисплей");
        finds("brightness", "ms-settings:display", "Display", "Дисплей");
        let installed = ("Installed apps", "Установленные приложения");
        finds(
            "uninstall",
            "ms-settings:appsfeatures",
            installed.0,
            installed.1,
        );
    }

    #[test]
    fn control_panel_items_are_found_in_either_language() {
        let devices = "shell:::{74246bfc-4c96-11d0-abef-0020af6b0b7a}";
        let manager = ("Device Manager", "Диспетчер устройств");
        finds("device manager", devices, manager.0, manager.1);
        finds("диспетчер устр", devices, manager.0, manager.1);
        let variables = "rundll32.exe sysdm.cpl,EditEnvironmentVariables";
        let names = ("Environment Variables", "Переменные среды");
        finds("переменные среды", variables, names.0, names.1);
        finds("path", variables, names.0, names.1);
    }

    fn setting(id: &str) -> Item {
        let items = items(Language::Russian);
        let found = items.into_iter().find(|item| item.id.0 == id);
        found.expect("in the table")
    }

    #[test]
    fn a_page_opens_through_the_shell_and_says_it_is_a_setting() {
        let display = setting("ms-settings:display");
        assert_eq!(
            display.action,
            Action::Open("ms-settings:display".to_string())
        );
        assert_eq!(display.subtitle, "Параметры Windows");
        assert_eq!(display.kind, Kind::Setting);
        assert!(display.keywords.contains(&"Display".to_string()));
        assert!(
            !display.keywords.contains(&"Дисплей".to_string()),
            "the title"
        );
        let english = SETTINGS[0].item(Language::English);
        assert_eq!(english.subtitle, "Settings");
    }

    #[test]
    fn a_control_panel_item_opens_its_program_and_says_where_it_lives() {
        let variables = setting("rundll32.exe sysdm.cpl,EditEnvironmentVariables");
        let program = Action::Launch {
            program: "rundll32.exe".to_string(),
            arguments: "sysdm.cpl,EditEnvironmentVariables".to_string(),
        };
        assert_eq!(variables.action, program);
        assert_eq!(variables.subtitle, "Панель управления");
        let devices = setting("shell:::{74246bfc-4c96-11d0-abef-0020af6b0b7a}");
        let opens = "shell:::{74246bfc-4c96-11d0-abef-0020af6b0b7a}".to_string();
        assert_eq!(devices.action, Action::Open(opens));
        let english = devices.id.0.as_str();
        let devices = SETTINGS.iter().find(|setting| setting.id().0 == english);
        let devices = devices.expect("Device Manager").item(Language::English);
        assert_eq!(devices.subtitle, "Control Panel");
    }

    #[test]
    fn every_setting_is_listed_once_with_a_name_in_each_language() {
        let mut ids: Vec<ItemId> = SETTINGS.iter().map(Setting::id).collect();
        ids.sort();
        ids.dedup();
        assert_eq!(ids.len(), SETTINGS.len());
        for setting in SETTINGS {
            assert!(setting.names.iter().all(|name| !name.is_empty()));
            assert!(!setting.icon.is_empty(), "{:?}", setting.names);
        }
    }

    #[test]
    fn a_page_shows_the_settings_apps_icon_and_an_item_its_own() {
        let pages = SETTINGS
            .iter()
            .filter(|setting| setting.panel == Panel::Settings);
        assert!(pages.clone().count() > 40);
        assert!(pages.clone().all(|page| page.icon == SETTINGS_APP));
        let items = SETTINGS
            .iter()
            .filter(|setting| setting.panel == Panel::ControlPanel);
        assert!(items.clone().count() > 15);
        assert!(items.clone().all(|item| item.icon.starts_with("shell:::{")));
    }
}
