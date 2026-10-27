//! User-facing texts. The interface vocabulary is Russian; this module is the single place to
//! swap for a localization layer.

pub const NAV_HOME: &str = "Главная";
pub const NAV_INSTANCES: &str = "Сборки";
pub const NAV_CATALOG: &str = "Каталог";
pub const NAV_ACCOUNT: &str = "Аккаунт";
pub const NAV_SETTINGS: &str = "Настройки";
pub const NAV_TASKS: &str = "Задачи";

pub const PLAY: &str = "Играть";
pub const STOP: &str = "Остановить";
pub const CONFIGURE: &str = "Настроить";
pub const CANCEL: &str = "Отмена";
pub const PREPARING: &str = "Подготовка";
pub const PLAYING: &str = "Играет";
pub const OTHER_INSTANCES: &str = "Другие сборки";
pub const NO_INSTANCES: &str = "Сборок пока нет";
pub const NO_TASKS: &str = "Активных задач нет";
pub const PROBLEM: &str = "Проблема";
pub const MOCK_DATA: &str = "Тестовые данные";
pub const THEME_DARK: &str = "Тёмная тема";
pub const THEME_LIGHT: &str = "Светлая тема";
pub const BACKEND_LOST: &str = "Нет связи с ядром";

pub const COL_NAME: &str = "Название";
pub const COL_VERSION: &str = "Версия";
pub const COL_LOADER: &str = "Загрузчик";
pub const COL_PLAYED: &str = "Время игры";

pub const SOON: &str = "Этот раздел появится позже";
pub const STUB_CATALOG: &str = "Поиск модов, модпаков, ресурс-паков и шейдеров.";
pub const STUB_ACCOUNT: &str = "Аккаунты Microsoft и оффлайн-ники.";
pub const STUB_SETTINGS: &str = "Параметры лаунчера: Java, память, внешний вид.";

/// "45 мин" or "12 ч 5 мин".
pub fn hours_minutes(secs: u64) -> String {
    let minutes = secs / 60;
    if minutes < 60 {
        format!("{minutes} мин")
    } else {
        format!("{} ч {} мин", minutes / 60, minutes % 60)
    }
}
