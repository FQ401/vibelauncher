# Дизайн-система VibeLauncher

Версия токенов 1.0.0. Машиночитаемые значения: [`tokens.json`](tokens.json). Витрина компонентов в обеих темах: [`design-system.html`](design-system.html) (чистый HTML/CSS, без скриптов и внешних ресурсов).

UI приложения рисуется средствами egui/eframe, поэтому система ограничена тем, что выражается через `egui::Visuals`, `egui::Style` и `TextStyle`: сплошные цвета, скругления, отступы, фиксированные размеры шрифтов, простые тени. Градиентов, blur, произвольной типографики внутри строки и CSS-эффектов нет.

Визуальный язык оригинален: нейтральные сине-серые поверхности и индиго-акцент. Ресурсы, цвета, иконки и текстуры других лаунчеров и игры не используются. Упоминание Minecraft — только текстом по официальным Usage Guidelines; логотипы и графика Mojang не применяются.

## 1. Темы

Две темы: `light` и `dark`. По умолчанию тема следует системной (`eframe` отдаёт `ctx.system_theme()`), пользователь может выбрать «Светлая / Тёмная / Системная». Набор ролей одинаков в обеих темах, меняются только значения.

## 2. Палитра

Цвета заданы ролями, а не оттенками. В компонентах используются только роли.

| Роль | Назначение | Light | Dark |
|---|---|---|---|
| `bg` | фон окна | `#F6F7F9` | `#0F1117` |
| `surface` | панели, карточки, списки | `#FFFFFF` | `#171A22` |
| `surface-raised` | диалоги, всплывающие | `#FFFFFF` | `#1E222C` |
| `surface-sunken` | поля ввода, дорожка прогресса | `#ECEEF2` | `#0B0D12` |
| `surface-hover` | наведение на элемент | `#EEF0F4` | `#222735` |
| `border` | декоративные границы | `#D5D9E0` | `#2B3040` |
| `border-strong` | границы интерактивных элементов | `#7C8596` | `#6B7488` |
| `text` | основной текст | `#14171F` | `#E8EAF0` |
| `text-muted` | вторичный текст | `#4B5565` | `#A3ABBB` |
| `text-disabled` | неактивное | `#8A93A3` | `#6B7488` |
| `accent` | основные действия, выбор, ссылки | `#4F46E5` | `#818CF8` |
| `accent-hover` | наведение | `#4338CA` | `#A5B4FC` |
| `accent-active` | нажатие | `#3730A3` | `#7077F2` |
| `on-accent` | текст на акценте | `#FFFFFF` | `#0F1117` |
| `accent-subtle` | фон выбранного элемента | `#E7E6FB` | `#262B4D` |
| `focus-ring` | кольцо фокуса | `#4F46E5` | `#A5B4FC` |
| `selection` | выделение текста | `#C7C4F5` | `#3A4180` |
| `success` / `warning` / `danger` / `info` | статусы (цвет текста и значков) | `#166534` / `#92400E` / `#B91C1C` / `#1D4ED8` | `#4ADE80` / `#FBBF24` / `#F87171` / `#60A5FA` |
| `*-subtle` | фон статусного блока | см. `tokens.json` | см. `tokens.json` |
| `on-danger` | текст на `danger` | `#FFFFFF` | `#0F1117` |
| `scrim` | затемнение под диалогом | `#14171F99` | `#000000B3` |

Правило: смысл не передаётся только цветом — статус всегда сопровождается значком или текстом.

### Контрасты (WCAG 2.2 AA)

Требования: обычный текст не менее 4.5:1, границы интерактивных элементов и кольцо фокуса не менее 3:1. Значения рассчитаны по формуле относительной яркости WCAG.

Светлая тема:

| Передний план | Фон | Контраст | Результат |
|---|---|---|---|
| `text` | `bg` | 16.71:1 | AA |
| `text` | `surface` | 17.92:1 | AA |
| `text` | `surface-raised` | 17.92:1 | AA |
| `text-muted` | `bg` | 7.03:1 | AA |
| `text-muted` | `surface` | 7.54:1 | AA |
| `text-muted` | `surface-hover` | 6.61:1 | AA |
| `text-disabled` | `surface` | 3.1:1 | не требуется (неактивный) |
| `on-accent` | `accent` | 6.29:1 | AA |
| `on-accent` | `accent-hover` | 7.9:1 | AA |
| `on-accent` | `accent-active` | 9.93:1 | AA |
| `accent` | `surface` | 6.29:1 | AA |
| `accent` | `bg` | 5.87:1 | AA |
| `text` | `accent-subtle` | 14.6:1 | AA |
| `accent` | `accent-subtle` | 5.12:1 | AA |
| `success` | `surface` | 7.13:1 | AA |
| `warning` | `surface` | 7.09:1 | AA |
| `danger` | `surface` | 6.47:1 | AA |
| `info` | `surface` | 6.7:1 | AA |
| `success` | `success-subtle` | 6.12:1 | AA |
| `warning` | `warning-subtle` | 6.04:1 | AA |
| `danger` | `danger-subtle` | 5.19:1 | AA |
| `info` | `info-subtle` | 5.4:1 | AA |
| `on-danger` | `danger` | 6.47:1 | не требуется (неактивный) |
| `border-strong` | `surface` | 3.72:1 | AA |
| `border-strong` | `bg` | 3.47:1 | AA |
| `focus-ring` | `surface` | 6.29:1 | AA |
| `focus-ring` | `bg` | 5.87:1 | AA |
| `accent` | `surface` | 6.29:1 | AA |

Тёмная тема:

| Передний план | Фон | Контраст | Результат |
|---|---|---|---|
| `text` | `bg` | 15.69:1 | AA |
| `text` | `surface` | 14.46:1 | AA |
| `text` | `surface-raised` | 13.22:1 | AA |
| `text-muted` | `bg` | 8.18:1 | AA |
| `text-muted` | `surface` | 7.54:1 | AA |
| `text-muted` | `surface-hover` | 6.46:1 | AA |
| `text-disabled` | `surface` | 3.71:1 | не требуется (неактивный) |
| `on-accent` | `accent` | 6.33:1 | AA |
| `on-accent` | `accent-hover` | 9.47:1 | AA |
| `on-accent` | `accent-active` | 5.04:1 | AA |
| `accent` | `surface` | 5.83:1 | AA |
| `accent` | `bg` | 6.33:1 | AA |
| `text` | `accent-subtle` | 11.37:1 | AA |
| `accent` | `accent-subtle` | 4.59:1 | AA |
| `success` | `surface` | 9.98:1 | AA |
| `warning` | `surface` | 10.42:1 | AA |
| `danger` | `surface` | 6.29:1 | AA |
| `info` | `surface` | 6.84:1 | AA |
| `success` | `success-subtle` | 8.16:1 | AA |
| `warning` | `warning-subtle` | 8.47:1 | AA |
| `danger` | `danger-subtle` | 5.77:1 | AA |
| `info` | `info-subtle` | 5.95:1 | AA |
| `on-danger` | `danger` | 6.82:1 | не требуется (неактивный) |
| `border-strong` | `surface` | 3.71:1 | AA |
| `border-strong` | `bg` | 4.02:1 | AA |
| `focus-ring` | `surface` | 8.73:1 | AA |
| `focus-ring` | `bg` | 9.47:1 | AA |
| `accent` | `surface` | 5.83:1 | AA |

`text-disabled` исключён из требований (неактивные элементы, WCAG 1.4.3). Для него дополнительно действует непрозрачность `0.5` у контролов.

## 3. Типографика

Шрифты (все с открытыми лицензиями, содержат кириллицу):

| Назначение | Шрифт | Лицензия | Источник |
|---|---|---|---|
| Интерфейс | Inter (запасной — Noto Sans) | SIL OFL 1.1 | https://github.com/rsms/inter |
| Моноширинный (логи, пути, JVM-аргументы) | JetBrains Mono (запасной — Noto Sans Mono) | SIL OFL 1.1 | https://github.com/JetBrains/JetBrainsMono |
| Иконки | Lucide | ISC | https://github.com/lucide-icons/lucide |

OFL допускает встраивание в программу при сохранении текста лицензии и копирайта; шрифты по отдельности не продаются, изменённые версии остаются под OFL. Тексты лицензий кладутся рядом со шрифтами в `assets/fonts/` и показываются в окне «О программе». Inter содержит кириллицу и не имеет Reserved Font Name.

Шкала (px, line-height в px, один размер на стиль, начертание задаётся семейством шрифта):

| Стиль | Размер | Интерлиньяж | Начертание | egui `TextStyle` |
|---|---|---|---|---|
| caption | 11 | 16 | Regular | — (Small для подписей) |
| small | 12 | 18 | Regular | `Small` |
| body | 14 | 20 | Regular | `Body`, `Button` |
| body-lg | 16 | 24 | Regular | — (`TextStyle::Name("BodyLg")`) |
| heading-3 | 18 | 24 | SemiBold | `TextStyle::Name("Heading3")` |
| heading-2 | 22 | 28 | SemiBold | `Heading` |
| heading-1 | 28 | 34 | SemiBold | `TextStyle::Name("Heading1")` |
| mono | 13 | 18 | Regular | `Monospace` |

Внутри одной строки разные размеры и начертания не смешиваются. Акцент в тексте — цветом роли или отдельным `RichText` в другом семействе (`Inter SemiBold`, регистрируется как отдельное `FontFamily::Name`), без произвольной вёрстки. Минимальный размер текста — 11.

## 4. Сетка и отступы

Базовая единица — 4 px. Шкала: 0, 4, 8, 12, 16, 24, 32, 48, 64.

| Токен | Значение |
|---|---|
| `item-spacing` | 8 × 8 |
| `button-padding` | 12 × 6 |
| `window-margin`, `card-padding` | 16 |
| `indent` | 16 |
| высота контрола | 32 (крупный 40) |
| минимальная цель нажатия | 32 × 32 |
| ширина боковой панели | 220 |
| значок инстанса | 48 (крупный 64) |

Скругления: `sm` 4 (чипы, индикаторы), `md` 6 (кнопки, поля), `lg` 10 (карточки, списки), `xl` 14 (диалоги), `full` 999 (прогресс, бейджи).

Тени: `sm` (карточка), `md` (выпадающие меню), `lg` (диалог). Параметры: смещение, blur, цвет; spread равен 0. В тёмной теме тени плотнее, поверхность дополнительно отделяется рамкой `border`.

Раскладка окна: боковая панель 220 + контентная область; контент — сетка карточек с колонками минимум 300 и зазором 12; при ширине окна менее 720 боковая панель сворачивается до иконок (56).

## 5. Движение

Длительности: `fast` 100 мс (hover, нажатие), `base` 160 мс (вкладки, появление уведомлений), `slow` 240 мс (диалоги). Кривые: standard `(0.2, 0, 0, 1)`, enter `(0, 0, 0, 1)`, exit `(0.3, 0, 1, 1)`. В egui — `ctx.animate_bool_with_time` и `Style::animation_time = 0.16`. Настройка «Уменьшить анимацию» (по умолчанию берётся из системы) обнуляет длительности; неопределённый прогресс показывается статичным сегментом.

## 6. Компоненты и состояния

Для каждого интерактивного компонента определены состояния: default, hover, pressed, focus, disabled; для полей и карточек — также error/selected.

### Кнопки

| Вариант | Фон | Текст | Граница | Назначение |
|---|---|---|---|---|
| primary | `accent` | `on-accent` | `accent` | главное действие экрана («Играть») — не более одной на область |
| secondary | `surface` | `text` | `border-strong` | прочие действия |
| ghost | прозрачный | `text` | нет | действия в тулбарах |
| danger | `danger` | `on-danger` | `danger` | необратимые действия |

Состояния: hover — `accent-hover` / `surface-hover`; pressed — `accent-active`; focus — кольцо 2 px `focus-ring`, смещение 2 px; disabled — непрозрачность 0.5, курсор «запрещено», не фокусируется. Высота 32, радиус `md`, значок слева от подписи через 8 px. Подпись всегда текстовая; кнопка только со значком имеет подсказку и `accessible label`.

### Поля ввода

Фон `surface-sunken`, граница `border-strong` 1 px, высота 32, радиус `md`. Focus — кольцо 2 px. Error — граница `danger` 2 px, под полем сообщение цветом `danger` с текстом причины. Disabled — непрозрачность 0.5. Подпись над полем (`small`, `text-muted`), подсказка под полем. Placeholder — `text-muted`. Выделение — `selection`.

### Карточки инстансов

Контейнер `surface-raised`, граница `border`, радиус `lg`, отступ 16, тень `sm`. Содержимое: значок инстанса 48 (слева), название (`body` SemiBold), метаданные в `small`/`text-muted` («загрузчик · версия игры · время игры»), справа главное действие. Состояния:

- default — как описано;
- hover — фон `surface-hover`, видны вторичные действия (меню «…»);
- selected — граница `accent` 2 px, фон `accent-subtle`;
- running — значок и бейдж `success`, кнопка «Стоп» (danger);
- error — граница `danger`, строка причины цветом `danger` со значком, кнопка «Детали»;
- disabled/updating — непрозрачность 0.5, тонкий прогресс под метаданными;
- focus — кольцо 2 px по контуру карточки; карточка достижима с клавиатуры, Enter запускает, контекстное меню — клавиша меню или Shift+F10.

Значки инстансов: набор из 20 иконок Lucide (`box`, `sword`, `pickaxe`, `flask-conical`, `leaf`, `flame`, `snowflake`, `zap`, `rocket`, `gem`, `compass`, `map`, `castle`, `tree-pine`, `ghost`, `skull`, `heart`, `star`, `wrench`, `play`) на подложке цвета `accent-subtle`; пользователь может выбрать свой файл изображения. Логотипы и текстуры Mojang и других правообладателей не поставляются.

### Списки

Строка высотой 32 (плотная) или 40, радиус `md`, горизонтальный отступ 12. Hover — `surface-hover`; selected — `accent-subtle` и полоса `accent` 3 px слева; disabled — `text-disabled`; focus — кольцо. Навигация стрелками вверх/вниз, Home/End, Enter — действие, Space — отметка. Разделители — `border`. Для длинных списков — прокрутка (ширина полосы 8) и виртуализация.

### Вкладки

Горизонтальная полоса под заголовком, разделитель `border`. Активная вкладка — текст `accent`, нижняя линия `accent` 2 px, SemiBold; неактивная — `text-muted`; hover — `text`; disabled — `text-disabled`. Фокус: кольцо; стрелки влево/вправо переключают, Home/End — первая/последняя. Роли доступности: tablist / tab / tabpanel.

### Прогресс

Дорожка высотой 8, радиус `full`, цвет `surface-sunken`, граница `border`; заполнение `accent` (успех — `success`, ошибка — `danger`). Рядом всегда числовое значение или текст этапа («Скачивание: 62%»). Неопределённый прогресс — движущийся сегмент 30% (при пониженной анимации — статичный). Спиннер используется только для коротких операций менее 2 с.

### Уведомления

Тосты в правом нижнем углу, ширина до 360, радиус `md`, тень `md`, акцентная полоса слева 4 px цвета статуса, фон `*-subtle`, значок статуса и текст. Типы: info, success, warning, danger. Автоскрытие: info/success — 5 с, warning — 8 с, danger — не скрывается автоматически. Появление 160 мс. Постоянные ошибки дублируются в журнале событий. Объявляются вспомогательным технологиям как live region (polite; danger — assertive). Кнопка закрытия доступна с клавиатуры.

### Диалоги

Модальное окно: затемнение `scrim`, поверхность `surface-raised`, радиус `xl`, отступ 16, тень `lg`, ширина 360–560. Заголовок `heading-3`, текст `body`, кнопки справа: слева вторичная («Отмена»), справа основная; в деструктивных диалогах основная — danger, а начальный фокус на «Отмена». Esc закрывает (эквивалент «Отмена»), Enter — основное действие, Tab зациклен внутри диалога; после закрытия фокус возвращается на вызвавший элемент.

### Пустые и ошибочные состояния

Пустое: значок 48 `text-muted`, заголовок, одна строка пояснения, одно основное действие; по центру области. Ошибочное: значок `danger`, что произошло (одно предложение), что делать, кнопки «Повторить» и «Подробности»; технические детали — в раскрывающемся блоке `mono`. Состояние загрузки — скелетон из блоков `surface-sunken` (без анимации при пониженной анимации).

## 7. Иконки

Набор: Lucide (лицензия ISC, допускает использование, копирование, изменение и распространение при сохранении текста лицензии; отдельные иконки, унаследованные от Feather, — MIT). Проверено 27.10.2026: https://github.com/lucide-icons/lucide/blob/main/LICENSE. Альтернатива с совместимой лицензией — Phosphor (MIT).

Параметры: сетка 24, штрих 1.5 (в приложении фиксированный вес набора), размеры 16 / 20 / 24, цвет — роль `text`, `text-muted`, `accent` или статус; иконка наследует цвет текста.

Доставка в egui — два варианта:

1. Основной: шрифт `lucide.ttf` из пакета lucide-static, регистрируется как отдельное `FontFamily::Name("icons")`; иконка — символ Unicode (кодовые точки берутся из `lucide.css`/JSON пакета), рисуется как обычный текст, поэтому наследует размер и цвет и красится темой без перерисовки. Генерируется Rust-модуль констант `icons::BOX` и т. п.
2. Дополнительный (многоцветные значки пользователя, большие иллюстрации): SVG через `egui_extras` (`install_image_loaders`, `Image::new(include_image!(...))`), перекраска `Image::tint(color)`.

Тексты лицензий ISC/MIT вместе с копирайтом Lucide включаются в «О программе». Значок без подписи обязан иметь подсказку и доступное имя.

## 8. Доступность

- Контраст — раздел 2; границы и кольцо фокуса — не менее 3:1.
- Фокус виден всегда: кольцо 2 px `focus-ring`, смещение 2 px, не скрывается и не обрезается контейнером.
- Все действия доступны с клавиатуры; порядок Tab совпадает с визуальным; ловушек фокуса нет (кроме модальных диалогов с возвратом фокуса).
- Минимальная цель нажатия 32 × 32 (рекомендуется 40 для основных действий).
- Размер интерфейса масштабируется (`ctx.set_pixels_per_point`, 100–200%) без потери функций; текст в 14 px при масштабе 100% — минимум для основного содержимого.
- Статус не передаётся только цветом.
- Уменьшение анимации — раздел 5.
- egui публикует дерево доступности через AccessKit; у кастомных виджетов задаются роль и подпись (`widget_info`).

## 9. Как токены ложатся на egui

Токены загружаются из `tokens.json` (на сборке — генерация Rust-констант или `include_str!` + `serde_json` при старте) и применяются функцией `apply_theme(ctx, Theme)`.

| Токен | egui |
|---|---|
| `bg` | `Visuals::panel_fill`, `window_fill` фона окна |
| `surface` | `Visuals::extreme_bg_color` для списков; `widgets.noninteractive.bg_fill` |
| `surface-raised` | `Visuals::window_fill` для диалогов и меню |
| `surface-sunken` | `Visuals::extreme_bg_color` (текстовые поля), `faint_bg_color` |
| `border` | `Visuals::window_stroke`, `widgets.noninteractive.bg_stroke` |
| `border-strong` | `widgets.inactive.bg_stroke` |
| `text` / `text-muted` | `Visuals::override_text_color`, `widgets.noninteractive.fg_stroke`; muted через `RichText::color` |
| `text-disabled` | `widgets.noninteractive` для неактивных, плюс `ui.disable()` (непрозрачность) |
| `accent` | `Visuals::selection.bg_fill`, `hyperlink_color`, заливка primary-кнопки |
| `accent-hover` / `accent-active` | `widgets.hovered.bg_fill` / `widgets.active.bg_fill` |
| `selection` | `Visuals::selection.bg_fill` для текста (отдельно от акцента кнопок рисуется в виджете) |
| `focus-ring` | `Visuals::selection.stroke`, `widgets.*.expansion` + собственный `Stroke` 2 px |
| `warning` / `danger` | `Visuals::warn_fg_color`, `error_fg_color` |
| `radius.*` | `CornerRadius` в `widgets.*.corner_radius`, `window_corner_radius`, `menu_corner_radius`, `Frame::corner_radius` |
| `shadow.*` | `Visuals::window_shadow`, `popup_shadow`, `Frame::shadow` (`Shadow { offset, blur, spread, color }`) |
| `spacing.*` | `Style::spacing.item_spacing`, `button_padding`, `window_margin`, `indent`, `interact_size`, `scroll.bar_width` |
| `typography.size` | `Style::text_styles` (`FontId::new(size, family)`) |
| темы | `Visuals::dark_mode`, `ctx.set_visuals_of(Theme::Light, ...)` и `Theme::Dark`; `ThemePreference` для выбора «Системная» |
| `typography.family` | `FontDefinitions::font_data` + `families` (`Proportional` → Inter, `Monospace` → JetBrains Mono, `Name("icons")` → Lucide) |

Для компонентов, которых нет в базовом egui (карточка, тост, бейдж, вкладки с линией), создаётся небольшой слой `ui::components` с функциями, читающими роли из активной темы. Составные состояния (selected, error, running) — это комбинации ролей, описанные в разделе 6, а не отдельные токены.

Ограничение: egui рисует текст без CSS-раскладки, поэтому в токенах нет `letter-spacing`, `text-transform` и т. п.; обрезка длинных названий — `Label::truncate`.

## 10. Лицензии и происхождение

Цвета, скругления, отступы и компоненты разработаны для VibeLauncher и не копируют внешние лаунчеры или игру. Используемые чужие материалы: Inter (OFL 1.1), JetBrains Mono (OFL 1.1), Lucide (ISC). Проверка лицензий выполнена 27.10.2026 по файлам лицензий в репозиториях проектов.
