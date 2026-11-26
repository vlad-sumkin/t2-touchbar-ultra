<div align="center">

<img src="assets/icons/hicolor/scalable/apps/org.t2touchbarultra.gtk.svg" width="116" height="116" alt="T2 Touchbar Ultra" />

# ✨ T2 Touchbar Ultra
### Next-Generation Touch Bar Workspace, Telemetry HUD, Smart Input & Force Touch Mechanical Keyboard Studio for Apple T2 MacBooks on Linux

[![Rust](https://img.shields.io/badge/Rust-2024_Edition-orange?style=for-the-badge&logo=rust)](https://www.rust-lang.org/)
[![GTK4 + Libadwaita](https://img.shields.io/badge/Settings_UI-GTK4_%2B_Libadwaita-3584e4?style=for-the-badge&logo=gnome)](https://gtk.org/)
[![Wayland / Niri](https://img.shields.io/badge/Compositor-Wayland_%2F_Niri-22c55e?style=for-the-badge)](#)
[![HID Report 0x53](https://img.shields.io/badge/Taptic_Engine-HID_Report_0x53-ff8c00?style=for-the-badge)](#2-%EF%B8%8F-mechanical-keyboard-clicks-via-force-touch-trackpad-mechpad-engine)
[![License: GPL-3.0](https://img.shields.io/badge/License-GPL--3.0--or--later-blue?style=for-the-badge)](LICENSE)

🌐 **Language / Язык:** **English** | **[Русский](README.ru.md)**

<p align="center">
  <b>Zero-polling event-driven OLED Touch Bar daemon + 7 mechanical switch acoustic profiles + full GTK4 Settings Suite</b><br>
  Engineered specifically for Apple T2 MacBook Pro / Air running Wayland &amp; Niri on Linux.
</p>

</div>

---

## 🎯 Why T2 Touchbar Ultra?

Most Linux Touch Bar daemons stop at static function keys. **T2 Touchbar Ultra** reimagines the Touch Bar and Force Touch trackpad actuator as one cohesive, hardware-native productivity system:

| Layer / Module | Role | Highlight |
| :--- | :--- | :--- |
| **`t2-touchbar-ultra`** | Headless DRM + `libinput` + `uinput` daemon | **0% GTK bloat**, event-driven `poll()` loop, adaptive timeouts |
| **`t2-touchbar-ultra-settings`** | Native **GTK4 + Libadwaita** Control Center | 6 categorized tabs, live switch tester, `RU / EN` toggle, 1-click Save &amp; Restart |
| **`MechPad` (`HID Report 0x53`)** | Trackpad Force Touch acoustic synthesizer | 7 real switch curves, stabilizer physics, sleep/wake auto-rebind, auto-mute |
| **Smart Input Suite** | Text Prediction + Word Lookup + Clipboard | Fuzzy RU/EN suggestions, Russian translation &amp; definition marquee, clipboard strip |

> [!TIP]
> The daemon binary `t2-touchbar-ultra` links **zero GUI toolkits**, runs smoothly both inside Wayland sessions and on the greetd lock/login screen (`t2-touchbar-ultra-greeter`), and never polls hardware while idle.

---

## 🚀 Feature Showcase

### 1. 📊 Idle Telemetry HUD & OLED Longevity
When not showing action buttons, the Touch Bar transforms into an ultra-crisp OLED telemetry dashboard (or sleeps completely dark to conserve battery):
- **Clock, Weekday & Date** rendered cleanly alongside **Niri Workspace Pills** and clickable **Open Window Tabs** (supports custom SVG icons &amp; smooth marquee scrolling for long titles).
- **Interactive Tap-Cycling Widgets:**
  - **System Monitor:** Cycles **CPU %**, **RAM %**, and **CPU Temperature (°C)**.
  - **Power &amp; Battery:** Cycles **Battery %**, **Live Power Consumption in Watts (W)** both on battery and plugged into AC, and **Remaining Time**.
  - **Hardware Strip:** Live **Volume / Mute badge**, **Display Brightness**, and **Keyboard Layout (`RU` / `EN`)**.
- **Interactive Media Capsule (`MPRIS`):** Live progress bar with 1-finger scrubbing needle + animated **"Artist – Title"** popups with instant Prev/Next buttons on track changes.
- **OLED Anti-Burn-In (`Pixel Shift`):** Imperceptibly shifts dashboard elements periodically to protect your Touch Bar OLED panel.

---

### 2. ⌨️ Mechanical Keyboard Clicks via Force Touch Trackpad (`MechPad Engine`)
Turn every keystroke on your laptop keyboard into crisp, responsive mechanical switch clicks emitted physically through the T2 Force Touch trackpad actuator — **no speakers, no Bluetooth latency, pure tactile acoustics**:

| Profile ID | Switch Name | Category | Sound &amp; Feel Signature |
| :--- | :--- | :--- | :--- |
| `cherry-mx-blue` | **Cherry MX Blue** | Clicky | Bright click-jacket snap + sharp 2-stage clack (`5.0 ms` secondary burst) |
| `cherry-mx-brown` | **Cherry MX Brown** | Tactile | Crisp mid-range tactile notch with dry, balanced bottom-out |
| `cherry-mx-red` | **Cherry MX Red** | Linear | Smooth, light linear travel with soft low-mid bottom-out |
| `holy-panda` | **Holy Panda** | Tactile · Thock | Pronounced rounded bump with hollow woody pop |
| `topre-45g` | **Topre 45g** | Electrostatic | Deepest, muted HHKB rubber-dome low-frequency thock |
| `ibm-model-m` | **IBM Model M** | Buckling Spring | Loud sharp snap + sustained vintage metallic spring ring (`6.5 ms` Stage-2) |
| `gateron-oil-king` | **Gateron Oil King** | Linear · Lubed | Velvety, creamy, marbly deep factory-lubed linear |

- **Direct `HID Report 0x53` Multi-Tone Engine:** Combines Gaussian impulse envelopes with up to 3 layered sine carriers (`95 Hz – 4.0 kHz`) and Stage-2 bursts, strictly clamped inside Apple's `AmplitudeMax = 70` actuator safety limit, with seamless `sysfs` fallback.
- **Stabilizer Acoustics:** `Space`, `Enter`, `Backspace`/`Delete`, and modifiers automatically carry deeper pitch, longer decay, and fuller weight than alphanumeric keys.
- **Suspend / Resume Self-Healing:** Listens directly to `org.freedesktop.login1.Manager.PrepareForSleep` via D-Bus and proactively releases and re-acquires newly enumerated `/dev/hidraw*` actuator devices immediately upon wake — zero manual restarts after sleep.
- **Audio Mute Aware:** Automatically silences both mechanical keystrokes and Touch Bar haptics whenever system audio is completely muted (`[MUTED]` or `0%`).

---

### 3. 🧠 Smart Typing, Translator Marquee &amp; Clipboard History
- **RU / EN Predictive Typing:** Real-time local + bigram word predictions directly on the Touch Bar in browsers, editors, and Telegram, with typo tolerance (Damerau–Levenshtein) and yellow matching prefix highlights.
- **Clipboard History Strip:** Focusing an empty text field reveals your recent copied text snippets for 1-tap pasting (configurable auto-hide timeout, default `30 s`).
- **Highlighted Word Translation &amp; Dictionary Marquee:** Select any single word in any app — after a configurable stabilization pause (default `3 s`), the Touch Bar displays its **Russian translation in yellow** followed by its **italicized definition** scrolling smoothly for `60 s`.
- **Emoji Strip &amp; HSV Color Picker:** Expandable popular emoji strip and an interactive HSV color ribbon (`builtin:colorpicker`) that copies hex values straight to your clipboard.

---

### 4. 👆 Adaptive Layers, Swipes &amp; Touch ID
- **Three Switchable Rows:** Media keys, Function keys (`F1..F12` via `Fn` hold or two-finger swipe), and your **Custom 3rd Layer** (`Print`/`Ins`/`Del`/`Home`/`End`/`PgUp`/`PgDn` + personal commands &amp; web apps).
- **Adaptive Timeout Learning:** Learns per-layer visibility durations (`5 s – 30 s`) based on whether you keep re-waking the bar.
- **Dark-Bar Multi-Finger Swipes:** Slide 2 or 3 fingers anywhere on the dark/idle Touch Bar to adjust **Brightness** or **Volume** smoothly with firm stop detents (`0%`/`1%` and `100%`) and Noctalia OSD notifications.
- **Touch ID &amp; Greeter Mode:** Animated Touch ID unlock prompt during authentication (`Changed` D-Bus signal integration) plus `t2-touchbar-ultra-greeter` for login managers (`greetd`).

---

### 5. 🎨 T2 Touchbar Ultra Settings Studio (`GTK4 + Libadwaita`)
Manage every single parameter visually:
- **6 Categorized Tabs:** *General &amp; Service*, *Gestures*, *Haptics &amp; MechPad*, *Smart Features &amp; Typography*, *Theme Color Editor (24+ slots)*, and *Custom Layer Keys*.
- **Live Switch Auditioning:** Test any of the 7 mechanical switches directly from the Haptics tab.
- **Instant `RU / EN` Button:** Flip the entire GUI between Russian and English on the fly without discarding unsaved changes.
- **Save &amp; Restart (`Ctrl+S`):** Validates all fields, writes `config.toml`, `theme.toml`, and `keys.toml`, and restarts `t2-touchbar-ultra.service` asynchronously.

---

## 🛠️ Configuration

All user files live cleanly in `$XDG_CONFIG_HOME/t2-touchbar-ultra/` (system defaults in `/etc/t2-touchbar-ultra/`):
- `config.toml` — full daemon, haptic, gesture, and smart-feature configuration
- `theme.toml` — HEX colors with **instant hot-reload** (zero restart required!)
- `keys.toml` — personal 3rd-layer buttons (`label = "command"` or `[[key]]` blocks with `icon` / `send` / `run`)
- `icons/` — custom `.svg` icons for apps and buttons

State (`state.toml` &amp; `clipboard_history.json`) is stored in `$XDG_STATE_HOME/t2-touchbar-ultra/`.

<details>
<summary><b>📄 Example config.toml &amp; Environment Variables</b></summary>

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

Supported overrides: `T2_TOUCHBAR_ULTRA_CONFIG`, `T2_TOUCHBAR_ULTRA_THEME`, `T2_TOUCHBAR_ULTRA_KEYS`, `T2_TOUCHBAR_ULTRA_STATE`, `T2_TOUCHBAR_ULTRA_DICT`, `T2_TOUCHBAR_ULTRA_ICONS`.

</details>

---

## 📦 Build &amp; Installation

### Prerequisites
- Apple T2 MacBook running Linux (`t2bdrm`/`appletbdrm`, `t2_trackpad_actuator` DKMS modules, `libinput`, `wtype`, `wpctl`, `wl-paste`)
- Build dependencies: Rust (edition 2024), `gtk4`, `libadwaita`, `libinput`, `pkgconf`

### Build from Source
```bash
git clone https://github.com/vlad-sumkin/t2-touchbar-ultra.git
cd t2-touchbar-ultra
RUSTFLAGS="-C target-cpu=native" cargo build --release --workspace
cargo test --workspace
sudo make install
```

### Enable Systemd Integration
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
> Reboot once after first adding your user to the `t2-touchbar-ultra` group so `user@UID.service` receives device permissions for DRM, input, backlight, `uinput`, and `/dev/hidraw*`.

Launch the Settings Studio anytime via app menu (**T2 Touchbar Ultra**) or terminal:
```bash
t2-touchbar-ultra-settings
```

---

## 📜 License &amp; Notices

Licensed under **GNU General Public License v3.0 or later** ([LICENSE](LICENSE)).  
See [THIRD-PARTY-NOTICES.md](THIRD-PARTY-NOTICES.md) for MIT/DRM foundations and switch waveform credits.