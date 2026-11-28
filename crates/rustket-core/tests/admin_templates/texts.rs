//! The admin-facing texts of the templates (docs/policy.md section 14): per
//! key a display name, an explanation, an optional Group Policy sentence and
//! (for keys with a value) a field label, in English and Russian. Plain
//! single-spaced text: ADML shows it raw. Not UI strings: they are not in
//! `locales/` and not in the app.

use rustket_core::policy::Key;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Language {
    En,
    Ru,
}

impl Language {
    pub const ALL: [Language; 2] = [Language::En, Language::Ru];

    /// The ADML directory.
    pub fn locale(self) -> &'static str {
        match self {
            Language::En => "en-US",
            Language::Ru => "ru-RU",
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub struct Text {
    pub name: &'static str,
    /// True on every system, in the Group Policy terms (Enabled, Disabled,
    /// not configured); the JSON schemas translate those to their values.
    pub explain: &'static str,
    /// A fact of the Windows channel only, appended in the ADML.
    pub windows: Option<&'static str>,
    /// The label of the value field; `None` for bool keys, which have none.
    pub label: Option<&'static str>,
}

#[derive(Debug, Clone, Copy)]
pub struct KeyTexts {
    pub key: Key,
    pub en: Text,
    pub ru: Text,
    /// What "not set" means, for the reference (English only).
    pub not_set: &'static str,
}

impl KeyTexts {
    pub fn get(&self, lang: Language) -> &Text {
        match lang {
            Language::En => &self.en,
            Language::Ru => &self.ru,
        }
    }
}

/// The display name of an enum value.
#[derive(Debug, Clone, Copy)]
pub struct EnumText {
    pub key: Key,
    pub value: &'static str,
    pub en: &'static str,
    pub ru: &'static str,
}

impl EnumText {
    pub fn get(&self, lang: Language) -> &'static str {
        match lang {
            Language::En => self.en,
            Language::Ru => self.ru,
        }
    }
}

/// The category's display name.
pub fn category(_lang: Language) -> &'static str {
    "Rustket"
}

/// The display name of the `supportedOn` definition of `version`.
pub fn supported_on(lang: Language, version: &str) -> String {
    match lang {
        Language::En => format!("Rustket {version} and later"),
        Language::Ru => format!("Rustket {version} и новее"),
    }
}

/// The ADML's `displayName` and `description`.
pub fn adml_header(lang: Language) -> (&'static str, &'static str) {
    match lang {
        Language::En => (
            "Rustket",
            "Policy settings of Rustket, the native Rocket.Chat desktop client.",
        ),
        Language::Ru => (
            "Rustket",
            "Параметры политики Rustket, нативного настольного клиента Rocket.Chat.",
        ),
    }
}

pub fn key_texts(key: Key) -> Option<&'static KeyTexts> {
    KEYS.iter().find(|t| t.key == key)
}

pub fn enum_text(key: Key, value: &str) -> Option<&'static EnumText> {
    ENUM_VALUES
        .iter()
        .find(|t| t.key == key && t.value == value)
}

pub static ENUM_VALUES: &[EnumText] = &[
    EnumText {
        key: Key::UpdateMode,
        value: "prompt",
        en: "prompt: check and install when the user agrees",
        ru: "prompt: проверять и устанавливать с согласия пользователя",
    },
    EnumText {
        key: Key::UpdateMode,
        value: "notify",
        en: "notify: only report new versions",
        ru: "notify: только сообщать о новых версиях",
    },
    EnumText {
        key: Key::UpdateMode,
        value: "off",
        en: "off: no update checks",
        ru: "off: не проверять обновления",
    },
];

pub static KEYS: &[KeyTexts] = &[
    KeyTexts {
        key: Key::ManagedServers,
        en: Text {
            name: "Managed servers",
            explain: "Servers added to every user's server list; users cannot remove them. One item per server: its URL with the scheme, optionally followed by a space and a display name (https://chat.example.com Example Chat). \"Allowed servers\" never blocks a managed server, but \"Require HTTPS for servers\" still refuses an http:// one. The certificate of a managed server must be trusted by the system: accepted certificate exceptions are not used for it. Disabled or not configured: no server is added. A server taken off the list stays as an ordinary server. Applies on all systems. A change applies while Rustket runs, within a few minutes.",
            windows: Some(
                "Only the computer setting counts (HKLM): a user-scope value is ignored.",
            ),
            label: Some("Servers (URL, optionally followed by a display name):"),
        },
        ru: Text {
            name: "Управляемые серверы",
            explain: "Серверы в списке каждого пользователя, которые нельзя удалить. По одному элементу на сервер: URL со схемой и, при желании, через пробел отображаемое имя (https://chat.example.com Example Chat). «Разрешённые серверы» управляемый сервер не блокируют, но «Требовать HTTPS для серверов» всё равно отклоняет сервер http://. Сертификату такого сервера должна доверять система: принятые исключения к нему не применяются. Отключено или не задано: серверы не добавляются. Убранный из списка сервер остаётся обычным. Действует во всех системах. Изменение применяется во время работы Rustket в течение нескольких минут.",
            windows: Some(
                "Действует только параметр компьютера (HKLM): значение для пользователя игнорируется.",
            ),
            label: Some("Серверы (URL и, при желании, отображаемое имя):"),
        },
        not_set: "No server is added.",
    },
    KeyTexts {
        key: Key::AllowUserServers,
        en: Text {
            name: "Allow users to add and remove servers",
            explain: "Enabled or not configured: users can add and remove servers. Disabled: the add and remove actions are hidden and rocketchat:// links to servers that are not configured yet are refused; servers already in the list stay usable while they are allowed. When no server is configured and users may not add one, Rustket shows a screen that says so, with the organization name and the support link. Managed servers can never be removed. Applies on all systems. A change applies while Rustket runs, within a few minutes.",
            windows: None,
            label: None,
        },
        ru: Text {
            name: "Разрешить пользователям добавлять и удалять серверы",
            explain: "Включено или не задано: пользователи могут добавлять и удалять серверы. Отключено: действия добавления и удаления скрыты, ссылки rocketchat:// на ещё не настроенные серверы отклоняются; серверы, уже находящиеся в списке, работают, пока они разрешены. Если ни один сервер не настроен, а добавлять их нельзя, Rustket показывает экран с этим сообщением, названием организации и ссылкой на поддержку. Управляемые серверы удалить нельзя никогда. Действует во всех системах. Изменение применяется во время работы Rustket в течение нескольких минут.",
            windows: None,
            label: None,
        },
        not_set: "Users can add and remove servers.",
    },
    KeyTexts {
        key: Key::AllowedServers,
        en: Text {
            name: "Allowed servers",
            explain: "Limits the servers Rustket may use. One pattern per item: [scheme://]host[:port][/path-prefix]. The host ignores case; *. at its start means any subdomain (not the domain itself). No scheme: http and https; no port: the default port; no path: any path. Unreadable patterns are skipped. Managed servers always pass this list, but \"Require HTTPS for servers\" still applies. Other servers are suspended (kept, not connected). Disabled or not configured: every server is allowed. An empty list allows managed servers only. Applies on all systems. A change applies while Rustket runs, within a few minutes.",
            windows: Some(
                "In this template an empty list counts as not configured: list the managed servers instead.",
            ),
            label: Some("Server patterns:"),
        },
        ru: Text {
            name: "Разрешённые серверы",
            explain: "Ограничивает круг серверов Rustket. Элемент — шаблон: [схема://]хост[:порт][/начало-пути]. Регистр хоста не важен; *. в начале — любой поддомен (не сам домен). Без схемы — http и https, без порта — порт по умолчанию, без пути — любой путь. Нечитаемый шаблон пропускается. Управляемые серверы список не ограничивает (но «Требовать HTTPS для серверов» действует). Остальные приостанавливаются (остаются без подключения). Отключено или не задано: разрешены все серверы. Пустой список разрешает только управляемые. Действует во всех системах. Изменение применяется во время работы Rustket в течение нескольких минут.",
            windows: Some(
                "В этом шаблоне пустой список считается незаданным: перечислите управляемые серверы.",
            ),
            label: Some("Шаблоны серверов:"),
        },
        not_set: "Every server is allowed.",
    },
    KeyTexts {
        key: Key::RequireHttps,
        en: Text {
            name: "Require HTTPS for servers",
            explain: "Enabled: servers with an http:// address are refused, managed or not, and configured ones are suspended (kept, never connected). Disabled or not configured: http:// servers may be used. Applies on all systems. A change applies while Rustket runs, within a few minutes.",
            windows: None,
            label: None,
        },
        ru: Text {
            name: "Требовать HTTPS для серверов",
            explain: "Включено: серверы с адресом http:// отклоняются, управляемые или нет, а уже настроенные приостанавливаются (остаются в списке, но не подключаются). Отключено или не задано: серверы http:// можно использовать. Действует во всех системах. Изменение применяется во время работы Rustket в течение нескольких минут.",
            windows: None,
            label: None,
        },
        not_set: "http:// servers may be used.",
    },
    KeyTexts {
        key: Key::AllowCertificateExceptions,
        en: Text {
            name: "Allow accepting untrusted certificates",
            explain: "Enabled or not configured: when a server presents a certificate the system does not trust, the user may accept it and Rustket keeps the exception. Disabled: there is no accept action and stored exceptions are ignored. Once Rustket reads the change (within a few minutes) the accept action disappears; a signed-in server keeps its connection settings until its session next starts (after a sign-in or Retry, at the latest at the next start of Rustket), and Rustket shows one notice about it. Managed servers never use stored exceptions. Applies on all systems.",
            windows: None,
            label: None,
        },
        ru: Text {
            name: "Разрешить принимать недоверенные сертификаты",
            explain: "Включено или не задано: если сервер предъявляет сертификат, которому система не доверяет, пользователь может принять его, и Rustket запомнит исключение. Отключено: принять сертификат нельзя, сохранённые исключения игнорируются. Когда Rustket прочитает изменение (в течение нескольких минут), кнопка принятия сертификата исчезает; сервер, на котором выполнен вход, сохраняет параметры соединения до следующего запуска сеанса (после входа или кнопки «Повторить», самое позднее при следующем запуске Rustket), и Rustket один раз сообщает об этом. Управляемые серверы сохранённые исключения не используют никогда. Действует во всех системах.",
            windows: None,
            label: None,
        },
        not_set: "Users may accept an untrusted certificate; stored exceptions are used.",
    },
    KeyTexts {
        key: Key::UpdateMode,
        en: Text {
            name: "Update mode",
            explain: "How the built-in updater behaves. prompt: check on schedule, install when the user clicks. notify: report new versions with a link to the release page, never download or install. off: no checks and no request to the update feed; the app says that the organization manages updates. Disabled or not configured: prompt, except notify for a Windows installation for all users and for future MSI and macOS installer packages. Only builds with the built-in updater read it; the Mac App Store version and builds without the updater ignore it. A change applies while Rustket runs, within a few minutes; leaving prompt cancels a download in progress.",
            windows: None,
            label: Some("Update mode:"),
        },
        ru: Text {
            name: "Режим обновлений",
            explain: "Поведение встроенного механизма обновления. prompt: проверять по расписанию, устанавливать по нажатию пользователя. notify: сообщать о новых версиях со ссылкой на страницу выпуска, не скачивать и не устанавливать. off: не проверять и не обращаться к источнику обновлений; приложение сообщает, что обновлениями управляет организация. Отключено или не задано: prompt, а для установки Windows для всех пользователей и будущих пакетов MSI и macOS notify. Его читают только сборки со встроенным механизмом обновления; версия из Mac App Store и сборки без него параметр игнорируют. Изменение применяется во время работы Rustket в течение нескольких минут; переход из режима prompt отменяет идущую загрузку.",
            windows: None,
            label: Some("Режим обновлений:"),
        },
        not_set: "prompt, except notify for a Windows installation for all users and for future MSI and macOS installer packages.",
    },
    KeyTexts {
        key: Key::AllowPasswordLogin,
        en: Text {
            name: "Allow sign-in with a password",
            explain: "Enabled or not configured: the sign-in screen offers the user name and password form. Disabled: the form is hidden, also when the server uses it for LDAP sign-in. This shapes the client only: the server still accepts passwords, and signing in with the browser opens the server's own login page. Applies on all systems. A change applies at the next sign-in; signed-in sessions are not affected.",
            windows: None,
            label: None,
        },
        ru: Text {
            name: "Разрешить вход по паролю",
            explain: "Включено или не задано: экран входа предлагает форму с именем пользователя и паролем. Отключено: форма скрыта, в том числе когда сервер использует её для входа через LDAP. Это влияет только на клиент: сервер по-прежнему принимает пароли, а вход через браузер открывает собственную страницу входа сервера. Действует во всех системах. Изменение применяется при следующем входе; сеансы, в которые уже выполнен вход, не затрагиваются.",
            windows: None,
            label: None,
        },
        not_set: "The password form is offered.",
    },
    KeyTexts {
        key: Key::AllowTokenLogin,
        en: Text {
            name: "Allow sign-in with a personal access token",
            explain: "Enabled or not configured: the sign-in screen offers signing in with a personal access token. Disabled: that option is hidden. This shapes the client only: the server still accepts tokens, and a token link pasted or opened during a browser sign-in still signs in, because Rustket cannot tell a personal access token from the token a browser sign-in returns; to forbid personal access tokens, remove the permission create-personal-access-tokens on the Rocket.Chat server (see the deployment guide). Applies on all systems. A change applies at the next sign-in; signed-in sessions are not affected.",
            windows: None,
            label: None,
        },
        ru: Text {
            name: "Разрешить вход по персональному токену доступа",
            explain: "Включено или не задано: экран входа предлагает вход по персональному токену доступа. Отключено: этот вариант скрыт. Это влияет только на клиент: сервер по-прежнему принимает токены, а ссылка с токеном, вставленная или открытая во время входа через браузер, по-прежнему выполняет вход, потому что Rustket не может отличить персональный токен доступа от токена, который возвращает вход через браузер; чтобы запретить персональные токены, уберите на сервере Rocket.Chat разрешение create-personal-access-tokens (см. руководство по развёртыванию). Действует во всех системах. Изменение применяется при следующем входе; сеансы, в которые уже выполнен вход, не затрагиваются.",
            windows: None,
            label: None,
        },
        not_set: "The personal access token sign-in is offered.",
    },
    KeyTexts {
        key: Key::AllowSecretsFile,
        en: Text {
            name: "Allow storing sign-in secrets in a file",
            explain: "Enabled or not configured: when no system keyring is usable, Rustket may keep sign-in secrets in a plain file, and users may choose memory-only storage in Settings. Disabled: secrets are never written to a file; without a keyring they are kept in memory only, and after each start users sign in again and enter their E2E password again. The Mac App Store version ignores it: it never writes such a file. A change applies while Rustket runs, within a few minutes.",
            windows: None,
            label: None,
        },
        ru: Text {
            name: "Разрешить хранить секреты входа в файле",
            explain: "Включено или не задано: если системное хранилище паролей недоступно, Rustket может хранить секреты входа в обычном файле, а пользователь может выбрать хранение только в памяти в настройках. Отключено: секреты никогда не записываются в файл; без хранилища паролей они хранятся только в памяти, и после каждого запуска нужно снова входить и вводить пароль E2E. Версия из Mac App Store параметр игнорирует: она такой файл не пишет. Изменение применяется во время работы Rustket в течение нескольких минут.",
            windows: None,
            label: None,
        },
        not_set: "The user's own setting; without a keyring secrets may go to a file.",
    },
    KeyTexts {
        key: Key::NotificationPreview,
        en: Text {
            name: "Show message text in notifications",
            explain: "Enabled: notifications show the message text. Disabled: notifications do not show the message text. Not configured: the user's own setting decides. Applies on all systems. A change applies while Rustket runs, within a few minutes.",
            windows: None,
            label: None,
        },
        ru: Text {
            name: "Показывать текст сообщений в уведомлениях",
            explain: "Включено: уведомления показывают текст сообщения. Отключено: уведомления не показывают текст сообщения. Не задано: решает собственная настройка пользователя. Действует во всех системах. Изменение применяется во время работы Rustket в течение нескольких минут.",
            windows: None,
            label: None,
        },
        not_set: "The user's own setting.",
    },
    KeyTexts {
        key: Key::HardwareAcceleration,
        en: Text {
            name: "Use hardware acceleration",
            explain: "Enabled: Rustket draws its window with the graphics processor. Disabled: it uses the software (CPU) renderer, which helps with faulty graphics drivers and remote sessions. Not configured: the user's own setting decides. The ICED_BACKEND environment variable still takes priority. The Mac App Store version ignores it. Settings show a change at once; it takes effect at the next start of Rustket.",
            windows: None,
            label: None,
        },
        ru: Text {
            name: "Использовать аппаратное ускорение",
            explain: "Включено: Rustket рисует окно с помощью графического процессора. Отключено: используется программная отрисовка (на процессоре), что помогает при неисправных графических драйверах и в удалённых сеансах. Не задано: решает собственная настройка пользователя. Переменная окружения ICED_BACKEND по-прежнему имеет приоритет. Версия из Mac App Store параметр игнорирует. Настройки показывают изменение сразу; в силу оно вступает при следующем запуске Rustket.",
            windows: None,
            label: None,
        },
        not_set: "The user's own setting.",
    },
    KeyTexts {
        key: Key::LaunchAtLogin,
        en: Text {
            name: "Start Rustket when the user signs in",
            explain: "Enabled: Rustket registers itself to start when the user signs in to the computer. Disabled: it removes that registration. Rustket applies the value when it starts and when this setting changes, once the terms are accepted and no blocking screen is shown, so it cannot act before its first start; a change the user makes outside Rustket (also in Task Manager or GNOME) stays until then. On a Mac, Rustket cannot undo a switch-off under Login Items (\"Allow in the Background\"). Not configured: the user decides; removing the policy leaves the registration as it is. On the Mac App Store version, macOS may ask the user to approve the registration in System Settings. Applies on all systems.",
            windows: None,
            label: None,
        },
        ru: Text {
            name: "Запускать Rustket при входе пользователя",
            explain: "Включено: Rustket регистрирует себя для запуска при входе пользователя в систему. Отключено: снимает эту регистрацию. Rustket применяет значение при запуске и при изменении этого параметра, когда условия приняты и экран блокировки не показан, поэтому до первого запуска оно не действует; изменение, сделанное пользователем вне Rustket (и в диспетчере задач или GNOME), сохраняется до этих моментов. На Mac отключение в «Объектах входа» («Разрешить в фоне») Rustket отменить не может. Не задано: решает пользователь; удаление политики оставляет регистрацию как есть. В версии из Mac App Store macOS может попросить одобрить регистрацию в Системных настройках. Действует во всех системах.",
            windows: None,
            label: None,
        },
        not_set: "The user decides; removing the policy leaves the registration as it is.",
    },
    KeyTexts {
        key: Key::OrganizationName,
        en: Text {
            name: "Organization name",
            explain: "The name shown as \"Managed by ...\" on locked settings, on the blocking screen and for suspended servers. One line of at most 80 characters; a longer value, or one with control, line-break or text-direction characters, is ignored. Disabled or not configured: Rustket says \"your organization\". Applies on all systems. A change applies while Rustket runs, within a few minutes.",
            windows: None,
            label: Some("Organization name:"),
        },
        ru: Text {
            name: "Название организации",
            explain: "Название, которое показывается в пометке об управлении у заблокированных настроек, на экране блокировки и у приостановленных серверов. Одна строка длиной не более 80 символов; более длинное значение или значение с управляющими символами, переводами строки или символами направления текста игнорируется. Отключено или не задано: Rustket пишет «ваша организация». Действует во всех системах. Изменение применяется во время работы Rustket в течение нескольких минут.",
            windows: None,
            label: Some("Название организации:"),
        },
        not_set: "Rustket says \"your organization\".",
    },
    KeyTexts {
        key: Key::SupportUrl,
        en: Text {
            name: "Support link",
            explain: "A help link for users, shown on the blocking screen, for suspended servers and when users may not add a server. It must be an absolute https:// URL; any other value is ignored. Disabled or not configured: no link is shown. Applies on all systems. A change applies while Rustket runs, within a few minutes.",
            windows: Some(
                "Only the computer setting counts (HKLM): a user-scope value is ignored.",
            ),
            label: Some("Support link (https://):"),
        },
        ru: Text {
            name: "Ссылка на поддержку",
            explain: "Ссылка на помощь для пользователей: на экране блокировки, у приостановленных серверов и когда добавлять серверы нельзя. Это должен быть абсолютный URL https://; любое другое значение игнорируется. Отключено или не задано: ссылка не показывается. Действует во всех системах. Изменение применяется во время работы Rustket в течение нескольких минут.",
            windows: Some(
                "Действует только параметр компьютера (HKLM): значение для пользователя игнорируется.",
            ),
            label: Some("Ссылка на поддержку (https://):"),
        },
        not_set: "No support link is shown.",
    },
    KeyTexts {
        key: Key::MinimumVersion,
        en: Text {
            name: "Minimum Rustket version",
            explain: "The oldest Rustket version that may connect, as MAJOR.MINOR.PATCH (for example 0.4.0). An older client connects to no server and shows a screen saying that an update is required, with the updater where users may install updates (update mode prompt). The client version is compared in semver order: a pre-release such as 0.6.0-rc.1 is older than 0.6.0; build metadata is ignored. A value that is not three numbers is ignored, so a typo cannot lock out every client. Disabled or not configured: any version may connect. Versions before 0.4.0 do not know this setting. Applies on all systems. A change applies while Rustket runs, within a few minutes.",
            windows: None,
            label: Some("Minimum version (MAJOR.MINOR.PATCH):"),
        },
        ru: Text {
            name: "Минимальная версия Rustket",
            explain: "Самая старая версия Rustket, которой разрешено подключаться, в виде MAJOR.MINOR.PATCH (например, 0.4.0). Более старый клиент не подключается ни к одному серверу и показывает экран о необходимости обновления, с обновлением, если пользователи могут его установить (режим prompt). Предварительная версия, например 0.6.0-rc.1, считается более старой, чем 0.6.0; метаданные сборки не учитываются. Значение не из трёх чисел игнорируется, чтобы опечатка не заблокировала все клиенты. Отключено или не задано: подключаться может любая версия. Версии до 0.4.0 этот параметр не знают. Действует во всех системах. Изменение применяется во время работы Rustket в течение нескольких минут.",
            windows: None,
            label: Some("Минимальная версия (MAJOR.MINOR.PATCH):"),
        },
        not_set: "Any version may connect.",
    },
];
