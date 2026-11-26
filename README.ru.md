<div align="center">

<img src="assets/icons/hicolor/scalable/apps/org.t2touchbarultra.gtk.svg" width="116" height="116" alt="T2 Touchbar Ultra" />

# ✨ T2 Touchbar Ultra
### Интеллектуальный Touch Bar, HUD-телеметрия, предиктивный ввод, словарь выделенных слов и симулятор механической клавиатуры на приводе Force Touch для MacBook T2 на Linux

[![Rust](https://img.shields.io/badge/Rust-2024_Edition-orange?style=for-the-badge&logo=rust)](https://www.rust-lang.org/)
[![GTK4 + Libadwaita](https://img.shields.io/badge/Настройки_GUI-GTK4_%2B_Libadwaita-3584e4?style=for-the-badge&logo=gnome)](https://gtk.org/)
[![Wayland / Niri](https://img.shields.io/badge/Композитор-Wayland_%2F_Niri-22c55e?style=for-the-badge)](#)
[![HID Report 0x53](https://img.shields.io/badge/Taptic_Engine-HID_Report_0x53-ff8c00?style=for-the-badge)](#2-%EF%B8%8F-механические-щелчки-клавиатуры-через-трекпад-mechpad-engine)
[![Лицензия: GPL-3.0](https://img.shields.io/badge/Лицензия-GPL--3.0--or--later-blue?style=for-the-badge)](LICENSE)

🌐 **Язык / Language:** **[English](README.md)** | **Русский**

<p align="center">
  <b>Событийный OLED-демон без фонового опроса + 7 профилей механических свитчей + графический центр настроек на GTK4/Libadwaita</b><br>
  Создано специально для MacBook Pro / Air с чипом Apple T2 под управлением Wayland и Niri.
</p>

</div>

---

## 🎯 Чем выделяется T2 Touchbar Ultra?

Обычные демоны Touch Bar под Linux ограничиваются статичным рядом клавиш `F1..F12`. **T2 Touchbar Ultra** объединяет OLED-дисплей Touch Bar, привод Force Touch тачпада и рабочий стол в единую быструю систему на **Rust**:

| Компонент | Назначение | Главная особенность |
| :--- | :--- | :--- |
| **`t2-touchbar-ultra`** | Фоновый демон DRM + `libinput` + `uinput` | **0% GTK-зависимостей**, чистый `poll()`, адаптивные таймеры слоёв |
| **`t2-touchbar-ultra-settings`** | Графический конфигуратор **GTK4 + Libadwaita** | 6 вкладок, тестер свитчей, мгновенный тумблер **RU / EN**, «Сохранить» = применить и перезапустить |
| **`MechPad` (`HID Report 0x53`)** | Синтез щелчков клавиш через Taptic Engine | 7 профилей свитчей, акустика стабилизаторов, автовосстановление после сна и защита по Mute |
| **Умные панели ввода** | Подсказки слов + Словарь + Буфер обмена | Нечёткий RU/EN-поиск, бегущая строка перевода и толкования слов, быстрый доступ к буферу |

> [!TIP]
> Основной демон `t2-touchbar-ultra` не тянет графических библиотек — он стабильно работает как в пользовательской сессии Wayland, так и на экране входа `greetd` (`t2-touchbar-ultra-greeter`), не создавая фоновой нагрузки в простое.

---

## 🚀 Подробный обзор возможностей

### 1. 📊 Живой дашборд ожидания и защита OLED
В режиме ожидания панель показывает контрастный информационный статус-бар (или полностью гаснет для экономии заряда):
- **Часы, день недели и дата**, **переключатель воркспейсов Niri** и кликабельная **панель открытых окон** (с поддержкой пользовательских SVG-иконок и плавной бегущей строкой для длинных заголовков).
- **Циклические виджеты по касанию:**
  - **Мониторинг системы:** переключение **CPU %**, **RAM %** и **Температуры CPU (°C)**.
  - **Питание и батарея:** переключение **Заряда (%)**, **мгновенного потребления энергии в Ваттах (W — как от батареи, так и от сети)** и **оставшегося времени**.
  - **Аппаратный блок:** живые индикаторы **Громкости / Mute**, **Яркости экрана** и **Раскладки (`RU` / `EN`)**.
- **Интерактивный медиа-плеер (`MPRIS`):** капсула с полосой прогресса, перемоткой пальцем и анимированными тостами «Исполнитель — Трек» с кнопками переключения при старте новой композиции.
- **Защита OLED от выгорания (`Pixel Shift`):** незаметное периодическое смещение виджетов для продления срока службы матрицы.

---

### 2. ⌨️ Механические щелчки клавиатуры через трекпад (`MechPad Engine`)
Каждое нажатие и отпускание клавиши на клавиатуре MacBook озвучивается физическим приводом Force Touch под трекпадом — **без динамиков, без задержек Bluetooth и с аутентичным резонансом**:

| ID профиля | Модель свитча | Тип | Характер звучания и тактильности |
| :--- | :--- | :--- | :--- |
| `cherry-mx-blue` | **Cherry MX Blue** | Clicky | Яркий щелчок click-jacket + резкий двухфазный клак (`5.0 ms`) |
| `cherry-mx-brown` | **Cherry MX Brown** | Tactile | Сухой, чёткий тактильный порог со сбалансированным bottom-out |
| `cherry-mx-red` | **Cherry MX Red** | Linear | Мягкий, плавный линейный ход с тихим донным ударом |
| `holy-panda` | **Holy Panda** | Tactile · Thock | Выраженный округлый тактильный бугор с деревянным хлопком («pop») |
| `topre-45g` | **Topre 45g** | Electrostatic | Самый глубокий и приглушённый купольный HHKB-thock |
| `ibm-model-m` | **IBM Model M** | Buckling Spring | Мощный щелчок прогибающейся пружины + металлический отзвук (`6.5 ms`) |
| `gateron-oil-king` | **Gateron Oil King** | Linear · Lubed | Густой, кремовый и бархатный глубокий смазанный линейный отклик |

- **Прямой синтез волн по `HID Report 0x53`:** Гауссова огибающая + до 3 синусоидальных несущих (`95 Гц – 4.0 кГц`) + отложенная вторая фаза, жёстко ограниченные безопасным пределом мотора `AmplitudeMax = 70` (плюс автоматический резервный режим через `sysfs`).
- **Акустика клавиш и стабилизаторов:** Для `Space`, `Enter`, `Backspace`/`Delete` и модификаторов используются собственные коэффициенты частоты, длительности и громкости.
- **Самовосстановление после сна:** Слушает сигнал `PrepareForSleep` от `systemd-logind` по D-Bus и автоматически переоткрывает пересозданный узел `/dev/hidraw*` после выхода из режима сна — перезапуск службы больше не нужен.
- **Привязка к системному Mute:** При полном выключении звука в системе (`[MUTED]` или `0%`) автоматически отключает отклик и клавиш, и тачбара.

---

### 3. 🧠 Умный ввод, словарь выделенных слов и буфер обмена
- **Предиктивный ввод (`RU / EN`):** Локальный словарь + биграммы следующих слов в браузерах, редакторах и **Telegram**, прощение опечаток и жёлтая подсветка совпавшего корня.
- **Панель истории буфера обмена:** При фокусе на пустое текстовое поле отображает последние скопированные фрагменты для мгновенной вставки по тапу (тайм-аут скрытия настраивается, по умолчанию `30 с`).
- **Перевод и толкование выделенного слова:** Выделите любое слово (включая иностранные и иероглифы) — после настраиваемой паузы (`3 с`) на Touch Bar появится плавная бегущая строка с **переводом на русский (жёлтым)** и **толкованием значения (курсивом)** на `60 с`.
- **Лента эмодзи и Палитра цветов:** Прокручиваемая полоса популярных эмодзи и встроенная HSV-палитра (`builtin:colorpicker`) с мгновенным копированием HEX-цвета в буфер.

---

### 4. 👆 Адаптивные слои, свайпы и Touch ID
- **Три слоя кнопок:** Медиа, ряд `F1..F12` (удержание `Fn` или свайп) и **3-й пользовательский слой** (Print/Ins/Del/Home/End/PgUp/PgDn + ваши скрипты, хоткеи и веб-приложения).
- **Адаптивное обучение тайм-аутов:** Запоминает оптимальное время активности каждого слоя (`5 – 30 с`).
- **Свайпы по тёмному тачбару:** Регулировка яркости и громкости 2–3 пальцами вслепую с тактильными упорами на минимуме/максимуме и интеграцией OSD Noctalia.
- **Touch ID и экран входа (`greetd`):** Анимированное приглашение к сканеру при запросах отпечатка пальца + обёртка `t2-touchbar-ultra-greeter` для работы клавиш до входа в систему.

---

### 5. 🎨 Графический интерфейс настроек (`t2-touchbar-ultra-settings`)
Удобный центр управления в стиле системных GTK4/Libadwaita приложений:
- **6 вкладок по группам настроек:** *Общие*, *Жесты*, *Отклик (MechPad + Mute)*, *Умные функции и Шрифты*, *Редактор темы (24+ цветов)*, *Пользовательские кнопки 3-го слоя*.
- **Кнопка «Тест» на профиле свитчей:** позволяет мгновенно оценить любой механический свитч прямо на вашем трекпаде.
- **Кнопка `RU / EN` в заголовке окна:** моментально переключает язык всего интерфейса без потери введённых изменений.
- **Кнопка «Сохранить» (`Ctrl+S`):** проверяет, сохраняет `config.toml`, `theme.toml`, `keys.toml` и асинхронно перезапускает `t2-touchbar-ultra.service`.

---

## 🛠️ Конфигурация

Все личные файлы настроек хранятся в `$XDG_CONFIG_HOME/t2-touchbar-ultra/` (системные умолчания в `/etc/t2-touchbar-ultra/`):
- `config.toml` — основные настройки демона, жестов, отклика и умных функций
- `theme.toml` — HEX-цвета всех виджетов с **мгновенным применением на лету (hot-reload)**
- `keys.toml` — кнопки 3-го слоя (`метка = "команда"` или секции `[[key]]` с `icon` / `send` / `run`)
- `icons/` — пользовательские `.svg` иконки

Обученное состояние (`state.toml`) и кэш буфера (`clipboard_history.json`) находятся в `$XDG_STATE_HOME/t2-touchbar-ultra/`.

<details>
<summary><b>📄 Пример config.toml и переменные окружения</b></summary>

```toml
idle_status_bar = true
pixel_shift = true
haptic_feedback = true
haptic_mute_with_audio = true

haptic_keyboard = true
haptic_keyboard_profile = "cherry-mx-blue" # cherry-mx-blue | cherry-mx-brown | cherry-mx-red | holy-panda | topre-45g | ibm-model-m | gateron-oil-king
haptic_keyboard_intensity = 0.85
haptic_keyboard_key_up = true
haptic_keyboard_acoustics = true

predictive_typing = true
clipboard_history = true
clipboard_timeout_sec = 30
word_lookup = true
word_lookup_delay_ms = 3000
word_lookup_timeout_sec = 60
```

Доступные переменные переопределения: `T2_TOUCHBAR_ULTRA_CONFIG`, `T2_TOUCHBAR_ULTRA_THEME`, `T2_TOUCHBAR_ULTRA_KEYS`, `T2_TOUCHBAR_ULTRA_STATE`, `T2_TOUCHBAR_ULTRA_DICT`, `T2_TOUCHBAR_ULTRA_ICONS`.

</details>

---

## 📦 Сборка и установка

### Требования
- MacBook на чипе Apple T2 под Linux (модули `t2bdrm`/`appletbdrm`, `t2_trackpad_actuator`, `libinput`, `wtype`, `wpctl`, `wl-paste`)
- Для сборки: Rust (2024 edition), `gtk4`, `libadwaita`, `libinput`, `pkgconf`

### Сборка из исходников
```bash
git clone https://github.com/vlad-sumkin/t2-touchbar-ultra.git
cd t2-touchbar-ultra
RUSTFLAGS="-C target-cpu=native" cargo build --release --workspace
cargo test --workspace
sudo make install
```

### Активация служб (Arch Linux / systemd)
```bash
sudo systemd-sysusers
sudo systemd-tmpfiles --create
sudo usermod -aG t2-touchbar-ultra "$USER"
sudo udevadm control --reload && sudo udevadm trigger --action=change
sudo systemctl enable --now t2-touchbar-ultra-attach.service
sudo systemctl --global enable t2-touchbar-ultra.service
systemctl --user daemon-reload && systemctl --user enable --now t2-touchbar-ultra.service
```

> [!IMPORTANT]
> После первого добавления в группу `t2-touchbar-ultra` перезагрузите ноутбук один раз, чтобы user-сессия `systemd` получила права на DRM, input, backlight, `uinput` и `/dev/hidraw*`.

Запустить настройки можно из меню приложений (**T2 Touchbar Ultra**) или командой:
```bash
t2-touchbar-ultra-settings
```

---

## 📜 Лицензия и уведомления

Распространяется по лицензии **GNU General Public License v3.0 или выше** ([LICENSE](LICENSE)).  
Уведомления о стороннем ПО и источниках приведены в [THIRD-PARTY-NOTICES.md](THIRD-PARTY-NOTICES.md).