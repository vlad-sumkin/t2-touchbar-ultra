// SPDX-License-Identifier: GPL-3.0-or-later
//! GTK4 + Libadwaita configuration suite for T2 Touchbar Ultra.

mod i18n;

use std::{
    cell::RefCell,
    process::Command,
    rc::Rc,
    time::Duration,
};

use adw::prelude::*;
use gtk4::{self as gtk, gdk, gio, glib};

use i18n::{I18n, Lang, tr};
use t2_touchbar_ultra::{
    config::Config,
    haptic::{Haptic, KeyboardHapticConfig, actuator_hidraw_available},
    keys::{self, KeySpec, KeySpecAction},
    levels,
    mechpad::SwitchProfile,
    theme::{self, Rgb, Theme},
};

const APP_ID: &str = "org.t2touchbarultra.gtk";
const SERVICE: &str = "t2-touchbar-ultra.service";

fn systemctl_user(args: &[&str]) -> bool {
    Command::new("systemctl")
        .arg("--user")
        .args(args)
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

fn service_active() -> bool {
    Command::new("systemctl")
        .args(["--user", "is-active", "--quiet", SERVICE])
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

fn rgba_from_rgb(rgb: Rgb) -> gdk::RGBA {
    gdk::RGBA::new(
        rgb.0 as f32 / 255.0,
        rgb.1 as f32 / 255.0,
        rgb.2 as f32 / 255.0,
        1.0,
    )
}

fn rgb_from_rgba(rgba: &gdk::RGBA) -> Rgb {
    Rgb(
        (rgba.red() * 255.0).round().clamp(0.0, 255.0) as u8,
        (rgba.green() * 255.0).round().clamp(0.0, 255.0) as u8,
        (rgba.blue() * 255.0).round().clamp(0.0, 255.0) as u8,
    )
}

#[derive(Clone)]
struct DirtyState {
    dirty: Rc<RefCell<bool>>,
    save_btn: gtk::Button,
    i18n: I18n,
}

impl DirtyState {
    fn mark_dirty(&self) {
        *self.dirty.borrow_mut() = true;
        self.refresh_button();
    }

    fn clear_dirty(&self) {
        *self.dirty.borrow_mut() = false;
        self.refresh_button();
    }

    fn refresh_button(&self) {
        let l = self.i18n.lang();
        if *self.dirty.borrow() {
            self.save_btn
                .set_label(tr(l, "Сохранить *", "Save *"));
        } else {
            self.save_btn
                .set_label(tr(l, "Сохранить", "Save"));
        }
    }
}

fn make_switch_row(
    i18n: &I18n,
    ru_title: &'static str,
    en_title: &'static str,
    ru_sub: &'static str,
    en_sub: &'static str,
    initial: bool,
    on_change: impl Fn(bool) + 'static,
) -> adw::SwitchRow {
    let row = adw::SwitchRow::builder().active(initial).build();
    i18n.bind({
        let row = row.clone();
        move |l| {
            row.set_title(tr(l, ru_title, en_title));
            row.set_subtitle(tr(l, ru_sub, en_sub));
        }
    });
    row.connect_active_notify(move |s| on_change(s.is_active()));
    row
}

#[allow(clippy::too_many_arguments)]
fn make_spin_row(
    i18n: &I18n,
    ru_title: &'static str,
    en_title: &'static str,
    ru_sub: &'static str,
    en_sub: &'static str,
    min: f64,
    max: f64,
    step: f64,
    digits: u32,
    initial: f64,
    on_change: impl Fn(f64) + 'static,
) -> adw::SpinRow {
    let adj = gtk::Adjustment::new(initial, min, max, step, step * 5.0, 0.0);
    let row = adw::SpinRow::builder()
        .adjustment(&adj)
        .digits(digits)
        .build();
    i18n.bind({
        let row = row.clone();
        move |l| {
            row.set_title(tr(l, ru_title, en_title));
            row.set_subtitle(tr(l, ru_sub, en_sub));
        }
    });
    row.connect_value_notify(move |r| on_change(r.value()));
    row
}

#[allow(clippy::too_many_arguments)]
fn make_scale_row(
    i18n: &I18n,
    ru_title: &'static str,
    en_title: &'static str,
    ru_sub: &'static str,
    en_sub: &'static str,
    min: f64,
    max: f64,
    step: f64,
    initial: f64,
    unit: &'static str,
    on_change: impl Fn(f64) + 'static,
) -> adw::ActionRow {
    let row = adw::ActionRow::new();
    i18n.bind({
        let row = row.clone();
        move |l| {
            row.set_title(tr(l, ru_title, en_title));
            row.set_subtitle(tr(l, ru_sub, en_sub));
        }
    });
    let scale = gtk::Scale::with_range(gtk::Orientation::Horizontal, min, max, step);
    scale.set_value(initial);
    scale.set_digits(0);
    scale.set_draw_value(true);
    scale.set_format_value_func(move |_, v| {
        if unit.is_empty() {
            format!("{v:.0}")
        } else {
            format!("{v:.0} {unit}")
        }
    });
    scale.set_width_request(210);
    scale.set_valign(gtk::Align::Center);
    scale.connect_value_changed(move |s| on_change(s.value()));
    row.add_suffix(&scale);
    row
}

fn wrap_scroll(page: &adw::PreferencesPage) -> gtk::ScrolledWindow {
    gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .vscrollbar_policy(gtk::PolicyType::Automatic)
        .vexpand(true)
        .child(page)
        .build()
}

fn make_group(i18n: &I18n, ru: &'static str, en: &'static str) -> adw::PreferencesGroup {
    let grp = adw::PreferencesGroup::new();
    i18n.bind({
        let grp = grp.clone();
        move |l| grp.set_title(tr(l, ru, en))
    });
    grp
}

fn switch_profile_labels(lang: Lang) -> gtk::StringList {
    let items: [&str; 7] = match lang {
        Lang::Ru => [
            "Cherry MX Blue — Кликающий (щелчок + резкий клак)",
            "Cherry MX Brown — Тактильный (сухой мягкий порог)",
            "Cherry MX Red — Линейный (плавный тихий ход)",
            "Holy Panda — Тактильный · Thock (глубокий деревянный хлопок)",
            "Topre 45g — Электростатический (глубокий глухой HHKB thock)",
            "IBM Model M — Пружинный (громкий щелчок + металлический звон)",
            "Gateron Oil King — Линейный смазанный (бархатный глубокий звук)",
        ],
        Lang::En => [
            "Cherry MX Blue — Clicky (sharp click-jacket snap + clack)",
            "Cherry MX Brown — Tactile (crisp dry tactile notch)",
            "Cherry MX Red — Linear (smooth light bottom-out)",
            "Holy Panda — Tactile · Thock (pronounced woody pop)",
            "Topre 45g — Electrostatic (deepest muted HHKB dome thock)",
            "IBM Model M — Buckling Spring (loud snap + metallic ring)",
            "Gateron Oil King — Linear · Lubed (creamy marbly deep linear)",
        ],
    };
    gtk::StringList::new(&items)
}

struct ThemeColorSpec {
    ru_title: &'static str,
    en_title: &'static str,
    default_hex: Rgb,
    get: fn(&Theme) -> Option<Rgb>,
    set: fn(&mut Theme, Option<Rgb>),
}

fn add_theme_color_row(
    group: &adw::PreferencesGroup,
    i18n: &I18n,
    theme: &Rc<RefCell<Theme>>,
    dirty: &DirtyState,
    spec: ThemeColorSpec,
) {
    let row = adw::ActionRow::new();
    let initial = (spec.get)(&theme.borrow());
    let display_rgb = initial.unwrap_or(spec.default_hex);

    let dialog = gtk::ColorDialog::builder().with_alpha(false).build();
    let color_btn = gtk::ColorDialogButton::new(Some(dialog));
    color_btn.set_rgba(&rgba_from_rgb(display_rgb));
    color_btn.set_valign(gtk::Align::Center);

    let reset_btn = gtk::Button::builder()
        .valign(gtk::Align::Center)
        .css_classes(["flat"])
        .sensitive(initial.is_some())
        .build();

    let updating = Rc::new(RefCell::new(false));
    let sync_subtitle = {
        let row = row.clone();
        let theme = theme.clone();
        let reset_btn = reset_btn.clone();
        let spec_get = spec.get;
        let def = spec.default_hex;
        move |l: Lang| match spec_get(&theme.borrow()) {
            Some(c) => {
                row.set_subtitle(&format!("{} {}", tr(l, "Свой:", "Custom:"), c.to_hex()));
                reset_btn.set_sensitive(true);
            }
            None => {
                row.set_subtitle(&format!("{} ({})", tr(l, "По умолчанию", "Default"), def.to_hex()));
                reset_btn.set_sensitive(false);
            }
        }
    };

    i18n.bind({
        let row = row.clone();
        let reset_btn = reset_btn.clone();
        let sync_subtitle = sync_subtitle.clone();
        move |l| {
            row.set_title(tr(l, spec.ru_title, spec.en_title));
            reset_btn.set_label(tr(l, "Сброс", "Reset"));
            sync_subtitle(l);
        }
    });

    color_btn.connect_rgba_notify({
        let theme = theme.clone();
        let dirty = dirty.clone();
        let updating = updating.clone();
        let sync_subtitle = sync_subtitle.clone();
        let i18n = i18n.clone();
        let spec_set = spec.set;
        move |btn| {
            if *updating.borrow() {
                return;
            }
            let picked = rgb_from_rgba(&btn.rgba());
            spec_set(&mut theme.borrow_mut(), Some(picked));
            sync_subtitle(i18n.lang());
            dirty.mark_dirty();
        }
    });

    reset_btn.connect_clicked({
        let theme = theme.clone();
        let dirty = dirty.clone();
        let updating = updating.clone();
        let color_btn = color_btn.clone();
        let sync_subtitle = sync_subtitle.clone();
        let i18n = i18n.clone();
        let spec_set = spec.set;
        let def = spec.default_hex;
        move |_| {
            *updating.borrow_mut() = true;
            spec_set(&mut theme.borrow_mut(), None);
            color_btn.set_rgba(&rgba_from_rgb(def));
            *updating.borrow_mut() = false;
            sync_subtitle(i18n.lang());
            dirty.mark_dirty();
        }
    });

    row.add_suffix(&reset_btn);
    row.add_suffix(&color_btn);
    group.add(&row);
}

fn build_keys_list(
    container: &gtk::Box,
    specs: &Rc<RefCell<Vec<KeySpec>>>,
    i18n: &I18n,
    dirty: &DirtyState,
) {
    while let Some(child) = container.first_child() {
        container.remove(&child);
    }

    let len = specs.borrow().len();
    for idx in 0..len {
        let item = specs.borrow()[idx].clone();
        let card = adw::PreferencesGroup::new();
        let l = i18n.lang();
        card.set_title(&format!("{} #{}", tr(l, "Кнопка", "Button"), idx + 1));

        let header_box = gtk::Box::new(gtk::Orientation::Horizontal, 4);
        let up_btn = gtk::Button::from_icon_name("go-up-symbolic");
        up_btn.add_css_class("flat");
        up_btn.set_sensitive(idx > 0);
        let down_btn = gtk::Button::from_icon_name("go-down-symbolic");
        down_btn.add_css_class("flat");
        down_btn.set_sensitive(idx + 1 < len);
        let del_btn = gtk::Button::from_icon_name("user-trash-symbolic");
        del_btn.add_css_class("flat");
        del_btn.add_css_class("destructive-action");

        header_box.append(&up_btn);
        header_box.append(&down_btn);
        header_box.append(&del_btn);
        card.set_header_suffix(Some(&header_box));

        let label_row = adw::EntryRow::builder()
            .title(tr(l, "Название / Метка", "Label"))
            .text(&item.label)
            .build();
        let icon_row = adw::EntryRow::builder()
            .title(tr(l, "Иконка SVG (необязательно)", "Icon SVG name (optional)"))
            .text(item.icon.as_deref().unwrap_or(""))
            .build();

        let is_send = matches!(item.action, KeySpecAction::Send(_));
        let val_text = match &item.action {
            KeySpecAction::Run(cmd) | KeySpecAction::Send(cmd) => cmd.clone(),
        };

        let kind_model = gtk::StringList::new(&[
            tr(l, "Запуск команды (Shell)", "Run Command (Shell)"),
            tr(l, "Комбинация клавиш (Send)", "Key Combination (Send)"),
        ]);
        let kind_row = adw::ComboRow::builder()
            .title(tr(l, "Тип действия", "Action Type"))
            .model(&kind_model)
            .selected(if is_send { 1 } else { 0 })
            .build();

        let action_row = adw::EntryRow::builder()
            .title(tr(
                l,
                if is_send { "Клавиши (напр. ctrl+c)" } else { "Команда" },
                if is_send { "Keys (e.g. ctrl+c)" } else { "Command" },
            ))
            .text(&val_text)
            .build();

        label_row.connect_changed({
            let specs = specs.clone();
            let dirty = dirty.clone();
            move |e| {
                if let Some(s) = specs.borrow_mut().get_mut(idx) {
                    s.label = e.text().to_string();
                    dirty.mark_dirty();
                }
            }
        });
        icon_row.connect_changed({
            let specs = specs.clone();
            let dirty = dirty.clone();
            move |e| {
                if let Some(s) = specs.borrow_mut().get_mut(idx) {
                    let t = e.text().trim().to_string();
                    s.icon = (!t.is_empty()).then_some(t);
                    dirty.mark_dirty();
                }
            }
        });
        action_row.connect_changed({
            let specs = specs.clone();
            let dirty = dirty.clone();
            let kind_row = kind_row.clone();
            move |e| {
                let text = e.text().to_string();
                let is_send_now = kind_row.selected() == 1;
                if let Some(s) = specs.borrow_mut().get_mut(idx) {
                    s.action = if is_send_now {
                        KeySpecAction::Send(text)
                    } else {
                        KeySpecAction::Run(text)
                    };
                    dirty.mark_dirty();
                }
            }
        });
        kind_row.connect_selected_notify({
            let specs = specs.clone();
            let dirty = dirty.clone();
            let action_row = action_row.clone();
            let i18n = i18n.clone();
            move |kr| {
                let is_send_now = kr.selected() == 1;
                let cur = action_row.text().to_string();
                let l_now = i18n.lang();
                action_row.set_title(tr(
                    l_now,
                    if is_send_now { "Клавиши (напр. ctrl+c)" } else { "Команда" },
                    if is_send_now { "Keys (e.g. ctrl+c)" } else { "Command" },
                ));
                if let Some(s) = specs.borrow_mut().get_mut(idx) {
                    s.action = if is_send_now {
                        KeySpecAction::Send(cur)
                    } else {
                        KeySpecAction::Run(cur)
                    };
                    dirty.mark_dirty();
                }
            }
        });

        up_btn.connect_clicked({
            let specs = specs.clone();
            let dirty = dirty.clone();
            let container = container.clone();
            let i18n = i18n.clone();
            move |_| {
                {
                    let mut list = specs.borrow_mut();
                    if idx > 0 && idx < list.len() {
                        list.swap(idx, idx - 1);
                    }
                }
                dirty.mark_dirty();
                build_keys_list(&container, &specs, &i18n, &dirty);
            }
        });
        down_btn.connect_clicked({
            let specs = specs.clone();
            let dirty = dirty.clone();
            let container = container.clone();
            let i18n = i18n.clone();
            move |_| {
                {
                    let mut list = specs.borrow_mut();
                    if idx + 1 < list.len() {
                        list.swap(idx, idx + 1);
                    }
                }
                dirty.mark_dirty();
                build_keys_list(&container, &specs, &i18n, &dirty);
            }
        });
        del_btn.connect_clicked({
            let specs = specs.clone();
            let dirty = dirty.clone();
            let container = container.clone();
            let i18n = i18n.clone();
            move |_| {
                {
                    let mut list = specs.borrow_mut();
                    if idx < list.len() {
                        list.remove(idx);
                    }
                }
                dirty.mark_dirty();
                build_keys_list(&container, &specs, &i18n, &dirty);
            }
        });

        card.add(&label_row);
        card.add(&icon_row);
        card.add(&kind_row);
        card.add(&action_row);
        container.append(&card);
    }
}

fn build_ui(app: &adw::Application) {
    let i18n = I18n::new(Lang::detect_default());

    let (loaded_cfg, cfg_err) = match Config::load() {
        Ok(c) => (c, None),
        Err(e) => (Config::default(), Some(format!("{e:#}"))),
    };
    let cfg = Rc::new(RefCell::new(loaded_cfg));
    let theme = Rc::new(RefCell::new(Theme::load()));
    let key_specs = Rc::new(RefCell::new(keys::load_specs().unwrap_or_default()));

    // On-demand actuator previewer for the Switch Test button
    let preview_haptic = Rc::new(RefCell::new(Haptic::open_with_keyboard(
        false,
        false,
        140,
        KeyboardHapticConfig {
            enabled: true,
            profile: SwitchProfile::parse(&cfg.borrow().haptic_keyboard_profile)
                .unwrap_or(SwitchProfile::CherryMxBlue),
            intensity: cfg.borrow().haptic_keyboard_intensity,
            key_up: cfg.borrow().haptic_keyboard_key_up,
            acoustics: cfg.borrow().haptic_keyboard_acoustics,
            mute_with_audio: cfg.borrow().haptic_mute_with_audio,
        },
    )));

    let toast_overlay = adw::ToastOverlay::new();

    let header = adw::HeaderBar::new();
    let lang_btn = gtk::Button::new();
    lang_btn.add_css_class("flat");
    header.pack_start(&lang_btn);

    let save_btn = gtk::Button::new();
    save_btn.add_css_class("suggested-action");
    header.pack_end(&save_btn);

    let dirty = DirtyState {
        dirty: Rc::new(RefCell::new(false)),
        save_btn: save_btn.clone(),
        i18n: i18n.clone(),
    };
    dirty.refresh_button();

    i18n.bind({
        let lang_btn = lang_btn.clone();
        let dirty = dirty.clone();
        move |l| {
            lang_btn.set_label(match l {
                Lang::Ru => "RU / EN",
                Lang::En => "EN / RU",
            });
            lang_btn.set_tooltip_text(Some(tr(
                l,
                "Переключить язык интерфейса (Русский / English)",
                "Switch UI language (English / Русский)",
            )));
            dirty.refresh_button();
        }
    });

    lang_btn.connect_clicked({
        let i18n = i18n.clone();
        move |_| {
            i18n.toggle();
        }
    });

    let stack = adw::ViewStack::new();
    let switcher = adw::ViewSwitcher::builder()
        .stack(&stack)
        .policy(adw::ViewSwitcherPolicy::Wide)
        .build();
    header.set_title_widget(Some(&switcher));

    // =========================================================================
    // TAB 1: Общие и Экран / General & Display
    // =========================================================================
    let page_general = adw::PreferencesPage::new();

    let grp_service = make_group(&i18n, "Состояние службы и оборудования", "Service & Hardware Status");
    let row_service = adw::ActionRow::new();
    let btn_srv_toggle = gtk::Button::builder().valign(gtk::Align::Center).build();
    let btn_srv_restart = gtk::Button::builder()
        .valign(gtk::Align::Center)
        .css_classes(["flat"])
        .build();
    row_service.add_suffix(&btn_srv_restart);
    row_service.add_suffix(&btn_srv_toggle);

    let row_actuator = adw::ActionRow::new();
    let row_reset_learn = adw::ActionRow::new();
    let btn_reset_learn = gtk::Button::builder()
        .valign(gtk::Align::Center)
        .css_classes(["destructive-action"])
        .build();
    row_reset_learn.add_suffix(&btn_reset_learn);

    i18n.bind({
        let row_service = row_service.clone();
        let row_actuator = row_actuator.clone();
        let row_reset_learn = row_reset_learn.clone();
        let btn_srv_restart = btn_srv_restart.clone();
        let btn_reset_learn = btn_reset_learn.clone();
        move |l| {
            row_service.set_title(tr(l, "Демон t2-touchbar-ultra", "t2-touchbar-ultra Service"));
            row_actuator.set_title(tr(l, "Привод трекпада и звук", "Trackpad Actuator & Audio"));
            row_reset_learn.set_title(tr(l, "Обучение тайм-аутов", "Adaptive Timeout Learning"));
            row_reset_learn.set_subtitle(tr(
                l,
                "Сбросить накопленные длительности показа слоёв к значениям по умолчанию",
                "Reset learned visibility durations back to defaults",
            ));
            btn_srv_restart.set_label(tr(l, "Перезапустить", "Restart"));
            btn_reset_learn.set_label(tr(l, "Сбросить", "Reset"));
        }
    });

    btn_srv_toggle.connect_clicked(|_| {
        if service_active() {
            systemctl_user(&["disable", "--now", SERVICE]);
        } else {
            systemctl_user(&["enable", "--now", SERVICE]);
        }
    });
    btn_srv_restart.connect_clicked({
        let toast_overlay = toast_overlay.clone();
        let i18n = i18n.clone();
        move |_| {
            let l = i18n.lang();
            let toast_overlay = toast_overlay.clone();
            glib::MainContext::default().spawn_local(async move {
                let ok = gio::spawn_blocking(|| systemctl_user(&["restart", SERVICE]))
                    .await
                    .unwrap_or(false);
                let msg = if ok {
                    tr(l, "Служба перезапущена", "Service restarted")
                } else {
                    tr(l, "Не удалось перезапустить службу", "Failed to restart service")
                };
                toast_overlay.add_toast(adw::Toast::new(msg));
            });
        }
    });
    btn_reset_learn.connect_clicked({
        let toast_overlay = toast_overlay.clone();
        let i18n = i18n.clone();
        move |_| {
            let ok = Command::new("t2-touchbar-ultra")
                .arg("--reset-learning")
                .status()
                .map(|s| s.success())
                .unwrap_or(false);
            let l = i18n.lang();
            let msg = if ok {
                tr(l, "Обученные тайм-ауты сброшены", "Learned timeouts reset")
            } else {
                tr(
                    l,
                    "Не найден бинарник t2-touchbar-ultra (выполните после установки пакета)",
                    "t2-touchbar-ultra binary not in PATH yet",
                )
            };
            toast_overlay.add_toast(adw::Toast::new(msg));
        }
    });

    let refresh_status = {
        let row_service = row_service.clone();
        let btn_srv_toggle = btn_srv_toggle.clone();
        let row_actuator = row_actuator.clone();
        let i18n = i18n.clone();
        move || {
            let l = i18n.lang();
            let active = service_active();
            row_service.set_subtitle(if active {
                tr(l, "Активна (работает в фоне)", "Active (running)")
            } else {
                tr(l, "Остановлена", "Stopped")
            });
            btn_srv_toggle.set_label(if active {
                tr(l, "Остановить", "Stop")
            } else {
                tr(l, "Запустить", "Start")
            });
            let hidraw_ok = actuator_hidraw_available();
            let vol = levels::volume();
            let vol_str = match (vol.muted, vol.percent) {
                (true, _) | (_, Some(0)) => tr(l, "Звук: Выкл (Mute) — отклик приглушён", "Audio: Muted — haptics silenced").to_string(),
                (false, Some(p)) => format!("{}: {p}%", tr(l, "Громкость", "Audio")),
                (false, None) => tr(l, "Звук: нет данных", "Audio: n/a").to_string(),
            };
            let act_str = if hidraw_ok {
                tr(l, "HID Report 0x53 (hidraw готов)", "HID Report 0x53 (hidraw ready)")
            } else {
                tr(l, "Ожидает прав udev (резерв: sysfs play)", "Waiting for udev rule (sysfs play fallback)")
            };
            row_actuator.set_subtitle(&format!("{act_str} • {vol_str}"));
        }
    };
    refresh_status();
    glib::timeout_add_local(Duration::from_secs(2), {
        let refresh_status = refresh_status.clone();
        move || {
            refresh_status();
            glib::ControlFlow::Continue
        }
    });
    i18n.bind({
        let refresh_status = refresh_status.clone();
        move |_| refresh_status()
    });

    grp_service.add(&row_service);
    grp_service.add(&row_actuator);
    grp_service.add(&row_reset_learn);
    page_general.add(&grp_service);

    let grp_display = make_group(&i18n, "Панель Touch Bar и таймеры", "Panel & Timeouts");
    grp_display.add(&make_switch_row(
        &i18n,
        "Статус-бар в режиме ожидания",
        "Idle Status Bar",
        "Показывать часы, окно, батарею, CPU/RAM и погоду вместо чёрного экрана",
        "Show clock, active window, battery, CPU/RAM & weather instead of dark bar",
        cfg.borrow().idle_status_bar,
        {
            let cfg = cfg.clone();
            let dirty = dirty.clone();
            move |v| {
                cfg.borrow_mut().idle_status_bar = v;
                dirty.mark_dirty();
            }
        },
    ));
    grp_display.add(&make_switch_row(
        &i18n,
        "Защита от выгорания (Pixel Shift)",
        "OLED Anti-Burn-In (Pixel Shift)",
        "Плавный незаметный сдвиг элементов статус-бара каждые несколько минут",
        "Subtle periodic shifting of idle widgets to prevent OLED burn-in",
        cfg.borrow().pixel_shift,
        {
            let cfg = cfg.clone();
            let dirty = dirty.clone();
            move |v| {
                cfg.borrow_mut().pixel_shift = v;
                dirty.mark_dirty();
            }
        },
    ));
    let esc_row = adw::ComboRow::builder().build();
    let initial_esc_idx = match cfg.borrow().physical_escape {
        None => 0,
        Some(true) => 1,
        Some(false) => 2,
    };
    let esc_rebinding = Rc::new(std::cell::Cell::new(false));
    i18n.bind({
        let esc_row = esc_row.clone();
        let esc_rebinding = esc_rebinding.clone();
        let init_once = Rc::new(std::cell::Cell::new(true));
        move |l| {
            esc_rebinding.set(true);
            let cur = if init_once.replace(false) {
                initial_esc_idx
            } else {
                esc_row.selected()
            };
            esc_row.set_title(tr(l, "Физическая клавиша Esc", "Physical Esc Key"));
            esc_row.set_subtitle(tr(
                l,
                "Скрывать виртуальную кнопку Esc, если она есть на клавиатуре (16\" и 2020+)",
                "Hide virtual Esc slot when laptop has a physical Esc key (16\" & 2020+)",
            ));
            let m = gtk::StringList::new(&[
                tr(l, "Автоопределение", "Auto-detect"),
                tr(l, "Есть физическая Esc", "Physical Esc present"),
                tr(l, "Показывать Esc на панели", "Show virtual Esc"),
            ]);
            esc_row.set_model(Some(&m));
            esc_row.set_selected(cur);
            esc_rebinding.set(false);
        }
    });
    esc_row.connect_selected_notify({
        let cfg = cfg.clone();
        let dirty = dirty.clone();
        let esc_rebinding = esc_rebinding.clone();
        move |r| {
            if esc_rebinding.get() {
                return;
            }
            cfg.borrow_mut().physical_escape = match r.selected() {
                1 => Some(true),
                2 => Some(false),
                _ => None,
            };
            dirty.mark_dirty();
        }
    });
    grp_display.add(&esc_row);

    grp_display.add(&make_scale_row(
        &i18n,
        "Яркость Touch Bar",
        "Panel Active Brightness",
        "Аппаратный уровень подсветки OLED-панели (0..255)",
        "Hardware OLED backlight brightness level (0..255)",
        1.0,
        255.0,
        1.0,
        cfg.borrow().active_brightness as f64,
        "",
        {
            let cfg = cfg.clone();
            let dirty = dirty.clone();
            move |v| {
                cfg.borrow_mut().active_brightness = v.round() as u32;
                dirty.mark_dirty();
            }
        },
    ));
    grp_display.add(&make_spin_row(
        &i18n,
        "Минимальное время показа слоя (сек)",
        "Minimum Layer Timeout (sec)",
        "Базовая длительность отображения кнопок после пробуждения (от 5 сек)",
        "Base illumination duration after waking a button row (min 5 s)",
        5.0,
        30.0,
        1.0,
        0,
        (cfg.borrow().minimum_timeout_ms / 1000) as f64,
        {
            let cfg = cfg.clone();
            let dirty = dirty.clone();
            move |v| {
                cfg.borrow_mut().minimum_timeout_ms = (v.round() as u64) * 1000;
                dirty.mark_dirty();
            }
        },
    ));
    grp_display.add(&make_spin_row(
        &i18n,
        "Максимальное время обучения (сек)",
        "Maximum Learned Timeout (sec)",
        "Верхний предел автоадаптации времени показа слоя",
        "Ceiling for adaptive extension learning",
        5.0,
        60.0,
        1.0,
        0,
        (cfg.borrow().maximum_timeout_ms / 1000) as f64,
        {
            let cfg = cfg.clone();
            let dirty = dirty.clone();
            move |v| {
                cfg.borrow_mut().maximum_timeout_ms = (v.round() as u64) * 1000;
                dirty.mark_dirty();
            }
        },
    ));
    grp_display.add(&make_spin_row(
        &i18n,
        "Окно продления (сек)",
        "Continuation Window (sec)",
        "В течение этого времени повторное пробуждение продлевает тайм-аут",
        "Window in which re-waking counts as needing longer visibility",
        1.0,
        60.0,
        1.0,
        0,
        (cfg.borrow().continuation_window_ms / 1000) as f64,
        {
            let cfg = cfg.clone();
            let dirty = dirty.clone();
            move |v| {
                cfg.borrow_mut().continuation_window_ms = (v.round() as u64) * 1000;
                dirty.mark_dirty();
            }
        },
    ));
    grp_display.add(&make_spin_row(
        &i18n,
        "Удержание Fn для смены ряда (мс)",
        "Fn Long-Press Duration (ms)",
        "Длительность удержания клавиши Fn для переключения в ряд F1..F12",
        "Duration to hold Fn to switch into the Function row",
        300.0,
        2000.0,
        50.0,
        0,
        cfg.borrow().fn_long_press_ms as f64,
        {
            let cfg = cfg.clone();
            let dirty = dirty.clone();
            move |v| {
                cfg.borrow_mut().fn_long_press_ms = v.round() as u64;
                dirty.mark_dirty();
            }
        },
    ));
    page_general.add(&grp_display);

    let grp_kbd_bl = make_group(&i18n, "Подсветка клавиатуры", "Keyboard Backlight Sync");
    grp_kbd_bl.add(&make_switch_row(
        &i18n,
        "Синхронизация яркости иконок",
        "Follow Keyboard Backlight",
        "Приглушать яркость текста/иконок панели вслед за подсветкой клавиш",
        "Scale Touch Bar glyph brightness together with keyboard backlight",
        cfg.borrow().follow_keyboard_backlight,
        {
            let cfg = cfg.clone();
            let dirty = dirty.clone();
            move |v| {
                cfg.borrow_mut().follow_keyboard_backlight = v;
                dirty.mark_dirty();
            }
        },
    ));
    grp_kbd_bl.add(&make_scale_row(
        &i18n,
        "Минимальная яркость символов",
        "Minimum Glyph Brightness",
        "Читаемый минимум яркости панели, когда подсветка клавиш выключена",
        "Readable brightness floor while keyboard backlight is off",
        0.0,
        100.0,
        1.0,
        cfg.borrow().minimum_brightness as f64,
        "%",
        {
            let cfg = cfg.clone();
            let dirty = dirty.clone();
            move |v| {
                cfg.borrow_mut().minimum_brightness = v.round() as u32;
                dirty.mark_dirty();
            }
        },
    ));
    grp_kbd_bl.add(&make_switch_row(
        &i18n,
        "Автоотключение подсветки при простое",
        "Activity Backlight Management",
        "Гасить подсветку клавиатуры при бездействии и восстанавливать при вводе",
        "Dim keyboard backlight after inactivity and restore on key/trackpad activity",
        cfg.borrow().activity_backlight,
        {
            let cfg = cfg.clone();
            let dirty = dirty.clone();
            move |v| {
                cfg.borrow_mut().activity_backlight = v;
                dirty.mark_dirty();
            }
        },
    ));
    grp_kbd_bl.add(&make_spin_row(
        &i18n,
        "Тайм-аут подсветки клавиатуры (сек)",
        "Keyboard Inactivity Timeout (sec)",
        "Через сколько секунд бездействия гасить клавиатуру (5..300)",
        "Inactivity duration before dimming keyboard backlight (5..300 s)",
        5.0,
        300.0,
        5.0,
        0,
        (cfg.borrow().activity_timeout_ms / 1000) as f64,
        {
            let cfg = cfg.clone();
            let dirty = dirty.clone();
            move |v| {
                cfg.borrow_mut().activity_timeout_ms = (v.round() as u64) * 1000;
                dirty.mark_dirty();
            }
        },
    ));
    page_general.add(&grp_kbd_bl);

    let tab_general = stack.add_titled_with_icon(
        &wrap_scroll(&page_general),
        Some("general"),
        "Общие",
        "preferences-system-symbolic",
    );

    // =========================================================================
    // TAB 2: Жесты / Gestures
    // =========================================================================
    let page_gestures = adw::PreferencesPage::new();
    let grp_gestures = make_group(&i18n, "Многопальцевые жесты и окна касания", "Multi-Finger Gestures & Timing");

    grp_gestures.add(&make_switch_row(
        &i18n,
        "Яркость двумя пальцами, громкость тремя",
        "Two-Finger Brightness (Else Three-Finger)",
        "Вкл: 2 пальца — яркость, 3 — громкость. Выкл: 2 — громкость, 3 — яркость",
        "On: 2 fingers = brightness, 3 = volume. Off: 2 = volume, 3 = brightness",
        cfg.borrow().two_finger_brightness,
        {
            let cfg = cfg.clone();
            let dirty = dirty.clone();
            move |v| {
                cfg.borrow_mut().two_finger_brightness = v;
                dirty.mark_dirty();
            }
        },
    ));
    grp_gestures.add(&make_spin_row(
        &i18n,
        "Окно второго пальца (мс)",
        "Two-Finger Window (ms)",
        "Ожидание второго пальца перед нажатием кнопки при касании (30..300 мс)",
        "Wait window for second finger before acting on single touch (30..300 ms)",
        30.0,
        300.0,
        10.0,
        0,
        cfg.borrow().two_finger_window_ms as f64,
        {
            let cfg = cfg.clone();
            let dirty = dirty.clone();
            move |v| {
                cfg.borrow_mut().two_finger_window_ms = v.round() as u64;
                dirty.mark_dirty();
            }
        },
    ));
    grp_gestures.add(&make_spin_row(
        &i18n,
        "Окно жеста на статус-баре (мс)",
        "Idle Gesture Window (ms)",
        "Ожидание свайпа на статус-баре перед пробуждением ряда клавиш",
        "Multi-finger gesture wait window on the idle status bar",
        100.0,
        5000.0,
        100.0,
        0,
        cfg.borrow().idle_gesture_window_ms as f64,
        {
            let cfg = cfg.clone();
            let dirty = dirty.clone();
            move |v| {
                cfg.borrow_mut().idle_gesture_window_ms = v.round() as u64;
                dirty.mark_dirty();
            }
        },
    ));
    grp_gestures.add(&make_spin_row(
        &i18n,
        "Шаг свайпа громкости (px)",
        "Volume Swipe Step (px)",
        "Расстояние сдвига в пикселях на 1 шаг изменения громкости",
        "Horizontal finger travel per volume step",
        10.0,
        200.0,
        1.0,
        0,
        cfg.borrow().volume_swipe_step_px as f64,
        {
            let cfg = cfg.clone();
            let dirty = dirty.clone();
            move |v| {
                cfg.borrow_mut().volume_swipe_step_px = v.round() as u32;
                dirty.mark_dirty();
            }
        },
    ));
    grp_gestures.add(&make_spin_row(
        &i18n,
        "Шаг свайпа яркости (px)",
        "Brightness Swipe Step (px)",
        "Расстояние сдвига в пикселях на 1 шаг изменения яркости дисплея",
        "Horizontal finger travel per brightness step",
        10.0,
        200.0,
        1.0,
        0,
        cfg.borrow().brightness_swipe_step_px as f64,
        {
            let cfg = cfg.clone();
            let dirty = dirty.clone();
            move |v| {
                cfg.borrow_mut().brightness_swipe_step_px = v.round() as u32;
                dirty.mark_dirty();
            }
        },
    ));
    grp_gestures.add(&make_spin_row(
        &i18n,
        "Порог свайпа смены слоя (px)",
        "Mode Switch Swipe Distance (px)",
        "Длина свайпа для переключения между Media / Fn / Custom рядами",
        "Drag distance required to switch between Media / Fn / Custom rows",
        40.0,
        2000.0,
        20.0,
        0,
        cfg.borrow().mode_swipe_px as f64,
        {
            let cfg = cfg.clone();
            let dirty = dirty.clone();
            move |v| {
                cfg.borrow_mut().mode_swipe_px = v.round() as u32;
                dirty.mark_dirty();
            }
        },
    ));
    page_gestures.add(&grp_gestures);

    let tab_gestures = stack.add_titled_with_icon(
        &wrap_scroll(&page_gestures),
        Some("gestures"),
        "Жесты",
        "input-touchpad-symbolic",
    );

    // =========================================================================
    // TAB 3: Клавиатура и Отклик / Haptics & MechPad
    // =========================================================================
    let page_haptics = adw::PreferencesPage::new();
    let grp_mechpad = make_group(
        &i18n,
        "Механические переключатели клавиатуры (MechPad)",
        "Mechanical Keyboard Simulation (MechPad)",
    );

    grp_mechpad.add(&make_switch_row(
        &i18n,
        "Щелчки клавиатуры через тачпад",
        "Keyboard Typing Haptics",
        "Озвучивать нажатия физической клавиатуры приводом Force Touch",
        "Simulate mechanical switch clicks via trackpad Taptic actuator",
        cfg.borrow().haptic_keyboard,
        {
            let cfg = cfg.clone();
            let dirty = dirty.clone();
            move |v| {
                cfg.borrow_mut().haptic_keyboard = v;
                dirty.mark_dirty();
            }
        },
    ));

    let profile_row = adw::ComboRow::new();
    let test_btn = gtk::Button::builder().valign(gtk::Align::Center).build();
    let initial_prof_idx = SwitchProfile::ALL_IDS
        .iter()
        .position(|&id| id == cfg.borrow().haptic_keyboard_profile)
        .unwrap_or(0) as u32;
    let prof_rebinding = Rc::new(std::cell::Cell::new(false));
    profile_row.add_suffix(&test_btn);

    i18n.bind({
        let profile_row = profile_row.clone();
        let test_btn = test_btn.clone();
        let prof_rebinding = prof_rebinding.clone();
        let init_once = Rc::new(std::cell::Cell::new(true));
        move |l| {
            prof_rebinding.set(true);
            let cur = if init_once.replace(false) {
                initial_prof_idx
            } else {
                profile_row.selected()
            };
            profile_row.set_title(tr(l, "Профиль свитчей", "Switch Profile"));
            profile_row.set_subtitle(tr(
                l,
                "Волновой синтез под АЧХ трекпада MacBook",
                "Frequency-calibrated waveform synthesis for MacBook trackpad",
            ));
            profile_row.set_model(Some(&switch_profile_labels(l)));
            profile_row.set_selected(cur);
            prof_rebinding.set(false);
            test_btn.set_label(tr(l, "Тест", "Test"));
        }
    });
    profile_row.connect_selected_notify({
        let cfg = cfg.clone();
        let dirty = dirty.clone();
        let prof_rebinding = prof_rebinding.clone();
        move |r| {
            if prof_rebinding.get() {
                return;
            }
            let idx = r.selected() as usize;
            if let Some(&id) = SwitchProfile::ALL_IDS.get(idx) {
                cfg.borrow_mut().haptic_keyboard_profile = id.to_owned();
                dirty.mark_dirty();
            }
        }
    });
    test_btn.connect_clicked({
        let cfg = cfg.clone();
        let preview_haptic = preview_haptic.clone();
        let toast_overlay = toast_overlay.clone();
        let i18n = i18n.clone();
        move |_| {
            let c = cfg.borrow();
            let prof = SwitchProfile::parse(&c.haptic_keyboard_profile)
                .unwrap_or(SwitchProfile::CherryMxBlue);
            let vol = levels::volume();
            let muted = vol.muted || vol.percent == Some(0);
            let mut hp = preview_haptic.borrow_mut();
            hp.set_system_muted(muted);
            if c.haptic_mute_with_audio && muted {
                toast_overlay.add_toast(adw::Toast::new(tr(
                    i18n.lang(),
                    "Звук в системе выключен (Mute) — тест приглушён",
                    "System audio is muted — haptic preview silenced",
                )));
                return;
            }
            hp.preview_switch(
                prof,
                c.haptic_keyboard_intensity,
                c.haptic_keyboard_key_up,
                c.haptic_keyboard_acoustics,
            );
        }
    });
    grp_mechpad.add(&profile_row);

    grp_mechpad.add(&make_scale_row(
        &i18n,
        "Сила / громкость щелчков",
        "Click Intensity",
        "Мастер-громкость привода в безопасном диапазоне трекпада (0..100%)",
        "Master actuator level within hardware safety ceiling (0..100%)",
        0.0,
        100.0,
        1.0,
        (cfg.borrow().haptic_keyboard_intensity * 100.0).round() as f64,
        "%",
        {
            let cfg = cfg.clone();
            let dirty = dirty.clone();
            move |v| {
                cfg.borrow_mut().haptic_keyboard_intensity = (v / 100.0) as f32;
                dirty.mark_dirty();
            }
        },
    ));
    grp_mechpad.add(&make_switch_row(
        &i18n,
        "Звук отпускания клавиши (Key-Up)",
        "Key-Up Release Click",
        "Лёгкий обратный щелчок при отпускании клавиши",
        "Subtle upstroke acoustic pulse when releasing keys",
        cfg.borrow().haptic_keyboard_key_up,
        {
            let cfg = cfg.clone();
            let dirty = dirty.clone();
            move |v| {
                cfg.borrow_mut().haptic_keyboard_key_up = v;
                dirty.mark_dirty();
            }
        },
    ));
    grp_mechpad.add(&make_switch_row(
        &i18n,
        "Акустика стабилизаторов (Space / Enter)",
        "Per-Key Stabilizer Acoustics",
        "Глубокий резонанс для пробела, Enter, Backspace и модификаторов",
        "Shaped keycap & stabilizer resonance for Space, Enter, Backspace & modifiers",
        cfg.borrow().haptic_keyboard_acoustics,
        {
            let cfg = cfg.clone();
            let dirty = dirty.clone();
            move |v| {
                cfg.borrow_mut().haptic_keyboard_acoustics = v;
                dirty.mark_dirty();
            }
        },
    ));
    page_haptics.add(&grp_mechpad);

    let grp_tb_haptic = make_group(
        &i18n,
        "Тактильный отклик Touch Bar и привязка к звуку",
        "Touch Bar Haptics & System Mute",
    );
    grp_tb_haptic.add(&make_switch_row(
        &i18n,
        "Отклик при касании Touch Bar",
        "Touch Bar Haptic Feedback",
        "Щелчки при нажатии кнопок, скролле эмодзи, подсказках и упорах свайпа",
        "Haptic clicks on Touch Bar buttons, emoji strip, suggestions & level stops",
        cfg.borrow().haptic_feedback,
        {
            let cfg = cfg.clone();
            let dirty = dirty.clone();
            move |v| {
                cfg.borrow_mut().haptic_feedback = v;
                dirty.mark_dirty();
            }
        },
    ));
    grp_tb_haptic.add(&make_switch_row(
        &i18n,
        "Отклик при отпускании кнопки (Touch-Up)",
        "Touch Bar Release Click",
        "Второй щелчок после отпускания пальца с кнопки Touch Bar",
        "Secondary click emitted when lifting finger off a Touch Bar key",
        cfg.borrow().haptic_touch_up,
        {
            let cfg = cfg.clone();
            let dirty = dirty.clone();
            move |v| {
                cfg.borrow_mut().haptic_touch_up = v;
                dirty.mark_dirty();
            }
        },
    ));
    grp_tb_haptic.add(&make_spin_row(
        &i18n,
        "Пауза между press и release (мс)",
        "Release Gap (ms)",
        "Минимальная пауза перед touch-up щелчком на быстрых тапах (40..1000)",
        "Minimum gap before touch-up actuation on quick taps (40..1000 ms)",
        40.0,
        1000.0,
        10.0,
        0,
        cfg.borrow().haptic_release_gap_ms as f64,
        {
            let cfg = cfg.clone();
            let dirty = dirty.clone();
            move |v| {
                cfg.borrow_mut().haptic_release_gap_ms = v.round() as u64;
                dirty.mark_dirty();
            }
        },
    ));
    grp_tb_haptic.add(&make_switch_row(
        &i18n,
        "Отключать весь отклик при Mute звука",
        "Silence Haptics When Audio Muted",
        "Автоматически отключать щелчки клавиатуры и Touch Bar при полном мьюте (или 0%)",
        "Mute both keyboard and Touch Bar haptics when system audio is muted or at 0%",
        cfg.borrow().haptic_mute_with_audio,
        {
            let cfg = cfg.clone();
            let dirty = dirty.clone();
            move |v| {
                cfg.borrow_mut().haptic_mute_with_audio = v;
                dirty.mark_dirty();
            }
        },
    ));
    page_haptics.add(&grp_tb_haptic);

    let tab_haptics = stack.add_titled_with_icon(
        &wrap_scroll(&page_haptics),
        Some("haptics"),
        "Клавиатура и Отклик",
        "input-keyboard-symbolic",
    );

    // =========================================================================
    // TAB 4: Умные функции и Шрифт / Smart & Fonts
    // =========================================================================
    let page_smart = adw::PreferencesPage::new();
    let grp_smart = make_group(&i18n, "Интерактивные функции", "Smart Interactive Features");

    grp_smart.add(&make_switch_row(
        &i18n,
        "Всплывающий трек при смене музыки (MPRIS)",
        "Now-Playing Track Changes",
        "Показывать «Исполнитель — Трек» и кнопки переключения при старте песни",
        "Show 'Artist – Title' toast with prev/next buttons when media changes",
        cfg.borrow().show_track_changes,
        {
            let cfg = cfg.clone();
            let dirty = dirty.clone();
            move |v| {
                cfg.borrow_mut().show_track_changes = v;
                dirty.mark_dirty();
            }
        },
    ));
    grp_smart.add(&make_switch_row(
        &i18n,
        "Предиктивный ввод и подсказки слов",
        "Predictive Word Suggestions",
        "Подсказки слов (RU/EN), автозавершение и панель эмодзи при наборе текста",
        "Live RU/EN word predictions, auto-completion & emoji bar while typing",
        cfg.borrow().predictive_typing,
        {
            let cfg = cfg.clone();
            let dirty = dirty.clone();
            move |v| {
                cfg.borrow_mut().predictive_typing = v;
                dirty.mark_dirty();
            }
        },
    ));
    grp_smart.add(&make_switch_row(
        &i18n,
        "История буфера обмена в пустых полях",
        "Clipboard History Strip",
        "Показывать последние записи буфера обмена при фокусе на пустое поле",
        "Display recent clipboard entries when focusing an empty text field",
        cfg.borrow().clipboard_history,
        {
            let cfg = cfg.clone();
            let dirty = dirty.clone();
            move |v| {
                cfg.borrow_mut().clipboard_history = v;
                dirty.mark_dirty();
            }
        },
    ));
    grp_smart.add(&make_spin_row(
        &i18n,
        "Время показа буфера обмена (сек)",
        "Clipboard Strip Timeout (sec)",
        "Через сколько секунд скрывать панель буфера обмена (5..300)",
        "Auto-hide duration for the clipboard history bar (5..300 s)",
        5.0,
        300.0,
        5.0,
        0,
        cfg.borrow().clipboard_timeout_sec as f64,
        {
            let cfg = cfg.clone();
            let dirty = dirty.clone();
            move |v| {
                cfg.borrow_mut().clipboard_timeout_sec = v.round() as u64;
                dirty.mark_dirty();
            }
        },
    ));
    grp_smart.add(&make_switch_row(
        &i18n,
        "Перевод и значение выделенного слова",
        "Selected Word Lookup Marquee",
        "При выделении одного слова показывать перевод на русский и его толкование",
        "Show translation and dictionary definition marquee when highlighting a word",
        cfg.borrow().word_lookup,
        {
            let cfg = cfg.clone();
            let dirty = dirty.clone();
            move |v| {
                cfg.borrow_mut().word_lookup = v;
                dirty.mark_dirty();
            }
        },
    ));
    grp_smart.add(&make_spin_row(
        &i18n,
        "Задержка перед поиском слова (мс)",
        "Word Lookup Selection Delay (ms)",
        "Пауза, чтобы слово успело выделиться полностью перед запросом",
        "Wait time after selection change before querying dictionary/translation",
        300.0,
        15000.0,
        100.0,
        0,
        cfg.borrow().word_lookup_delay_ms as f64,
        {
            let cfg = cfg.clone();
            let dirty = dirty.clone();
            move |v| {
                cfg.borrow_mut().word_lookup_delay_ms = v.round() as u64;
                dirty.mark_dirty();
            }
        },
    ));
    grp_smart.add(&make_spin_row(
        &i18n,
        "Длительность показа значения слова (сек)",
        "Word Marquee Timeout (sec)",
        "Сколько секунд отображать бегущую строку с переводом/значением (5..300)",
        "Display duration for the word definition marquee (5..300 s)",
        5.0,
        300.0,
        5.0,
        0,
        cfg.borrow().word_lookup_timeout_sec as f64,
        {
            let cfg = cfg.clone();
            let dirty = dirty.clone();
            move |v| {
                cfg.borrow_mut().word_lookup_timeout_sec = v.round() as u64;
                dirty.mark_dirty();
            }
        },
    ));
    page_smart.add(&grp_smart);

    let grp_typo = make_group(&i18n, "Типографика и основной цвет", "Typography & Key Color");
    let key_color_row = adw::ActionRow::new();
    let kc_dlg = gtk::ColorDialog::builder().with_alpha(false).build();
    let kc_btn = gtk::ColorDialogButton::new(Some(kc_dlg));
    kc_btn.set_valign(gtk::Align::Center);
    let init_kc = theme::parse_color(&cfg.borrow().key_color).unwrap_or(Rgb(0xdc, 0xe6, 0xff));
    kc_btn.set_rgba(&rgba_from_rgb(init_kc));
    i18n.bind({
        let key_color_row = key_color_row.clone();
        let cfg = cfg.clone();
        move |l| {
            key_color_row.set_title(tr(l, "Основной цвет клавиш (key_color)", "Primary Key Color (key_color)"));
            key_color_row.set_subtitle(&cfg.borrow().key_color);
        }
    });
    kc_btn.connect_rgba_notify({
        let cfg = cfg.clone();
        let dirty = dirty.clone();
        let key_color_row = key_color_row.clone();
        move |b| {
            let hex = rgb_from_rgba(&b.rgba()).to_hex();
            cfg.borrow_mut().key_color = hex.clone();
            key_color_row.set_subtitle(&hex);
            dirty.mark_dirty();
        }
    });
    key_color_row.add_suffix(&kc_btn);
    grp_typo.add(&key_color_row);

    let font_entry = adw::EntryRow::builder()
        .text(cfg.borrow().font_family.as_deref().unwrap_or(""))
        .build();
    i18n.bind({
        let font_entry = font_entry.clone();
        move |l| {
            font_entry.set_title(tr(
                l,
                "Шрифт (имя или путь к .ttf/.otf; пусто = стандартный)",
                "Font family or path to .ttf/.otf (empty = default)",
            ));
        }
    });
    font_entry.connect_changed({
        let cfg = cfg.clone();
        let dirty = dirty.clone();
        move |e| {
            let t = e.text().trim().to_string();
            cfg.borrow_mut().font_family = (!t.is_empty()).then_some(t);
            dirty.mark_dirty();
        }
    });
    grp_typo.add(&font_entry);

    grp_typo.add(&make_spin_row(
        &i18n,
        "Размер базового шрифта (px)",
        "Base Font Size (px)",
        "Размер шрифта для кнопок, подсказок, буфера и толкования слов",
        "Font size for keys, predictions, clipboard and word marquee",
        14.0,
        56.0,
        1.0,
        1,
        f64::from(cfg.borrow().font_size.unwrap_or(32.0)),
        {
            let cfg = cfg.clone();
            let dirty = dirty.clone();
            move |v| {
                cfg.borrow_mut().font_size = Some(v as f32);
                dirty.mark_dirty();
            }
        },
    ));
    grp_typo.add(&make_spin_row(
        &i18n,
        "Размер шрифта статус-бара (px)",
        "Status Bar Font Size (px)",
        "Размер шрифта виджетов режима ожидания (часы, CPU, батарея)",
        "Font size for idle status bar widgets (clock, CPU, battery)",
        14.0,
        56.0,
        1.0,
        1,
        f64::from(cfg.borrow().status_font_size.unwrap_or(36.0)),
        {
            let cfg = cfg.clone();
            let dirty = dirty.clone();
            move |v| {
                cfg.borrow_mut().status_font_size = Some(v as f32);
                dirty.mark_dirty();
            }
        },
    ));
    page_smart.add(&grp_typo);

    let tab_smart = stack.add_titled_with_icon(
        &wrap_scroll(&page_smart),
        Some("smart"),
        "Функции и Шрифт",
        "preferences-desktop-font-symbolic",
    );

    // =========================================================================
    // TAB 5: Тема / Theme (theme.toml)
    // =========================================================================
    let page_theme = adw::PreferencesPage::new();
    let grp_t_keys = make_group(&i18n, "Клавиши и подсветка", "Keys & Highlights");
    for spec in [
        ThemeColorSpec { ru_title: "Цвет символов и иконок", en_title: "Key Glyphs & Icons", default_hex: Rgb(0xdc, 0xe6, 0xff), get: |t| t.keys.color, set: |t, v| t.keys.color = v },
        ThemeColorSpec { ru_title: "Цвет кнопки Esc", en_title: "Esc Button", default_hex: Rgb(0xff, 0x8c, 0x00), get: |t| t.keys.esc_color, set: |t, v| t.keys.esc_color = v },
        ThemeColorSpec { ru_title: "Подсветка нажатия", en_title: "Active / Pressed Key", default_hex: Rgb(0xff, 0x8c, 0x00), get: |t| t.keys.active_color, set: |t, v| t.keys.active_color = v },
        ThemeColorSpec { ru_title: "Совпадение в подсказках", en_title: "Suggestion Match Prefix", default_hex: Rgb(0xff, 0xd6, 0x0a), get: |t| t.keys.suggestion_match_color, set: |t, v| t.keys.suggestion_match_color = v },
    ] {
        add_theme_color_row(&grp_t_keys, &i18n, &theme, &dirty, spec);
    }
    page_theme.add(&grp_t_keys);

    let grp_t_center = make_group(&i18n, "Часы, Центр и Медиа-полоса", "Clock, Center & Media Bar");
    for spec in [
        ThemeColorSpec { ru_title: "Часы (время)", en_title: "Clock Time", default_hex: Rgb(0xdc, 0xe6, 0xff), get: |t| t.status.clock.time, set: |t, v| t.status.clock.time = v },
        ThemeColorSpec { ru_title: "Дата", en_title: "Clock Date", default_hex: Rgb(0xdc, 0xe6, 0xff), get: |t| t.status.clock.date, set: |t, v| t.status.clock.date = v },
        ThemeColorSpec { ru_title: "Заголовок активного окна", en_title: "Window Title", default_hex: Rgb(0xdc, 0xe6, 0xff), get: |t| t.status.center.window_title, set: |t, v| t.status.center.window_title = v },
        ThemeColorSpec { ru_title: "Индикатор воркспейса", en_title: "Workspace Pill", default_hex: Rgb(0xdc, 0xe6, 0xff), get: |t| t.status.center.workspace, set: |t, v| t.status.center.workspace = v },
        ThemeColorSpec { ru_title: "Название медиа-трека", en_title: "Media Track Title", default_hex: Rgb(0xdc, 0xe6, 0xff), get: |t| t.status.center.media_title, set: |t, v| t.status.center.media_title = v },
        ThemeColorSpec { ru_title: "Разделитель бегущей строки", en_title: "Marquee Separator", default_hex: Rgb(0xdc, 0xe6, 0xff), get: |t| t.status.center.marquee_separator, set: |t, v| t.status.center.marquee_separator = v },
        ThemeColorSpec { ru_title: "Фон медиа-капсулы", en_title: "Media Bar Background", default_hex: Rgb(0x00, 0x00, 0x00), get: |t| t.status.media_bar.capsule_bg, set: |t, v| t.status.media_bar.capsule_bg = v },
        ThemeColorSpec { ru_title: "Заполнение медиа-прогресса", en_title: "Media Bar Progress Fill", default_hex: Rgb(0xdc, 0xe6, 0xff), get: |t| t.status.media_bar.progress_fill, set: |t, v| t.status.media_bar.progress_fill = v },
        ThemeColorSpec { ru_title: "Бегунок медиа-скруббера", en_title: "Media Bar Needle", default_hex: Rgb(0xff, 0x8c, 0x00), get: |t| t.status.media_bar.needle, set: |t, v| t.status.media_bar.needle = v },
        ThemeColorSpec { ru_title: "Всплывающий трек (Тост)", en_title: "Track Change Popup", default_hex: Rgb(0xdc, 0xe6, 0xff), get: |t| t.track_popup.color, set: |t, v| t.track_popup.color = v },
    ] {
        add_theme_color_row(&grp_t_center, &i18n, &theme, &dirty, spec);
    }
    page_theme.add(&grp_t_center);

    let grp_t_batt = make_group(&i18n, "Батарея и телеметрия", "Battery & System Telemetry");
    for spec in [
        ThemeColorSpec { ru_title: "От сети / заряжена", en_title: "Battery Plugged / AC", default_hex: Rgb(0x00, 0x9f, 0xff), get: |t| t.status.battery.plugged, set: |t, v| t.status.battery.plugged = v },
        ThemeColorSpec { ru_title: "Идёт зарядка", en_title: "Battery Charging", default_hex: Rgb(0x00, 0xe6, 0x76), get: |t| t.status.battery.charging, set: |t, v| t.status.battery.charging = v },
        ThemeColorSpec { ru_title: "Обычный заряд", en_title: "Battery Normal", default_hex: Rgb(0xdc, 0xe6, 0xff), get: |t| t.status.battery.normal, set: |t, v| t.status.battery.normal = v },
        ThemeColorSpec { ru_title: "Низкий заряд", en_title: "Battery Warning", default_hex: Rgb(0xff, 0x8c, 0x00), get: |t| t.status.battery.warning, set: |t, v| t.status.battery.warning = v },
        ThemeColorSpec { ru_title: "Критический заряд", en_title: "Battery Critical", default_hex: Rgb(0xff, 0x3b, 0x30), get: |t| t.status.battery.critical, set: |t, v| t.status.battery.critical = v },
        ThemeColorSpec { ru_title: "Загрузка CPU", en_title: "CPU Usage", default_hex: Rgb(0xdc, 0xe6, 0xff), get: |t| t.status.sysmon.cpu, set: |t, v| t.status.sysmon.cpu = v },
        ThemeColorSpec { ru_title: "Оперативная память (RAM)", en_title: "RAM Usage", default_hex: Rgb(0xdc, 0xe6, 0xff), get: |t| t.status.sysmon.ram, set: |t, v| t.status.sysmon.ram = v },
        ThemeColorSpec { ru_title: "Температура (TEMP)", en_title: "CPU Temperature", default_hex: Rgb(0xdc, 0xe6, 0xff), get: |t| t.status.sysmon.temp, set: |t, v| t.status.sysmon.temp = v },
        ThemeColorSpec { ru_title: "Громкость", en_title: "Volume Widget", default_hex: Rgb(0xdc, 0xe6, 0xff), get: |t| t.status.hardware.volume, set: |t, v| t.status.hardware.volume = v },
        ThemeColorSpec { ru_title: "Звук выключен (Mute)", en_title: "Volume Muted", default_hex: Rgb(0xff, 0x3b, 0x30), get: |t| t.status.hardware.volume_muted, set: |t, v| t.status.hardware.volume_muted = v },
        ThemeColorSpec { ru_title: "Яркость дисплея", en_title: "Brightness Widget", default_hex: Rgb(0xdc, 0xe6, 0xff), get: |t| t.status.hardware.brightness, set: |t, v| t.status.hardware.brightness = v },
        ThemeColorSpec { ru_title: "Раскладка клавиатуры", en_title: "Keyboard Layout", default_hex: Rgb(0xdc, 0xe6, 0xff), get: |t| t.status.hardware.keyboard_layout, set: |t, v| t.status.hardware.keyboard_layout = v },
        ThemeColorSpec { ru_title: "Полоса уровней свайпа", en_title: "Swipe Level Meter", default_hex: Rgb(0xdc, 0xe6, 0xff), get: |t| t.levels.level_meter, set: |t, v| t.levels.level_meter = v },
        ThemeColorSpec { ru_title: "Текст уровней свайпа", en_title: "Swipe Level Text", default_hex: Rgb(0xdc, 0xe6, 0xff), get: |t| t.levels.level_text, set: |t, v| t.levels.level_text = v },
    ] {
        add_theme_color_row(&grp_t_batt, &i18n, &theme, &dirty, spec);
    }
    page_theme.add(&grp_t_batt);

    let tab_theme = stack.add_titled_with_icon(
        &wrap_scroll(&page_theme),
        Some("theme"),
        "Тема",
        "applications-graphics-symbolic",
    );

    // =========================================================================
    // TAB 6: Кнопки (3-й слой) / Custom Keys (keys.toml)
    // =========================================================================
    let keys_outer = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(12)
        .margin_top(16)
        .margin_bottom(24)
        .margin_start(24)
        .margin_end(24)
        .build();

    let keys_toolbar = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .spacing(12)
        .build();
    let keys_desc = gtk::Label::builder()
        .xalign(0.0)
        .wrap(true)
        .hexpand(true)
        .css_classes(["dim-label"])
        .build();
    let add_key_btn = gtk::Button::builder()
        .css_classes(["suggested-action"])
        .valign(gtk::Align::Center)
        .build();
    keys_toolbar.append(&keys_desc);
    keys_toolbar.append(&add_key_btn);
    keys_outer.append(&keys_toolbar);

    let keys_list = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(14)
        .build();
    keys_outer.append(&keys_list);

    let clamp_keys = adw::Clamp::builder()
        .maximum_size(640)
        .child(&keys_outer)
        .build();
    let scroll_keys = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .vscrollbar_policy(gtk::PolicyType::Automatic)
        .vexpand(true)
        .child(&clamp_keys)
        .build();

    i18n.bind({
        let keys_desc = keys_desc.clone();
        let add_key_btn = add_key_btn.clone();
        let keys_list = keys_list.clone();
        let key_specs = key_specs.clone();
        let i18n_clone = i18n.clone();
        let dirty = dirty.clone();
        move |l| {
            keys_desc.set_label(tr(
                l,
                "Пользовательские кнопки на третьем слое Touch Bar (веб-приложения, скрипты, хоткеи).",
                "Custom keys for the third Touch Bar layer (web apps, shell commands, keystrokes).",
            ));
            add_key_btn.set_label(tr(l, "+ Добавить кнопку", "+ Add Button"));
            build_keys_list(&keys_list, &key_specs, &i18n_clone, &dirty);
        }
    });
    add_key_btn.connect_clicked({
        let key_specs = key_specs.clone();
        let keys_list = keys_list.clone();
        let i18n = i18n.clone();
        let dirty = dirty.clone();
        move |_| {
            key_specs.borrow_mut().push(KeySpec {
                label: "App".into(),
                icon: None,
                action: KeySpecAction::Run("xdg-open https://example.com".into()),
            });
            dirty.mark_dirty();
            build_keys_list(&keys_list, &key_specs, &i18n, &dirty);
        }
    });

    let tab_keys = stack.add_titled_with_icon(
        &scroll_keys,
        Some("keys"),
        "Кнопки",
        "view-list-symbolic",
    );

    // Update tab titles dynamically when switching RU <-> EN
    i18n.bind({
        let tab_general = tab_general.clone();
        let tab_gestures = tab_gestures.clone();
        let tab_haptics = tab_haptics.clone();
        let tab_smart = tab_smart.clone();
        let tab_theme = tab_theme.clone();
        let tab_keys = tab_keys.clone();
        move |l| {
            tab_general.set_title(Some(tr(l, "Общие", "General")));
            tab_gestures.set_title(Some(tr(l, "Жесты", "Gestures")));
            tab_haptics.set_title(Some(tr(l, "Отклик", "Haptics")));
            tab_smart.set_title(Some(tr(l, "Умные", "Smart")));
            tab_theme.set_title(Some(tr(l, "Тема", "Theme")));
            tab_keys.set_title(Some(tr(l, "Кнопки", "Keys")));
        }
    });

    // Save & Apply & Restart action
    let do_save = Rc::new({
        let cfg = cfg.clone();
        let theme = theme.clone();
        let key_specs = key_specs.clone();
        let toast_overlay = toast_overlay.clone();
        let dirty = dirty.clone();
        let i18n = i18n.clone();
        move || {
            let l = i18n.lang();
            if let Err(err) = cfg.borrow_mut().save() {
                toast_overlay.add_toast(adw::Toast::new(&format!(
                    "{}: {err:#}",
                    tr(l, "Ошибка конфигурации", "Configuration error")
                )));
                return;
            }
            if let Err(err) = theme.borrow().save() {
                toast_overlay.add_toast(adw::Toast::new(&format!(
                    "{}: {err:#}",
                    tr(l, "Ошибка сохранения темы", "Failed to save theme")
                )));
                return;
            }
            if let Err(err) = keys::save_specs(&key_specs.borrow()) {
                toast_overlay.add_toast(adw::Toast::new(&format!(
                    "{}: {err:#}",
                    tr(l, "Ошибка кнопок", "Custom key error")
                )));
                return;
            }
            dirty.clear_dirty();

            let toast_overlay = toast_overlay.clone();
            glib::MainContext::default().spawn_local(async move {
                let restarted = gio::spawn_blocking(|| systemctl_user(&["restart", SERVICE]))
                    .await
                    .unwrap_or(false);
                let msg = if restarted {
                    tr(
                        l,
                        "Настройки сохранены, служба перезапущена",
                        "Settings saved and service restarted",
                    )
                } else {
                    tr(
                        l,
                        "Файлы сохранены (перезапуск службы не удался или служба не установлена)",
                        "Files saved (service restart skipped or inactive)",
                    )
                };
                toast_overlay.add_toast(adw::Toast::new(msg));
            });
        }
    });

    save_btn.connect_clicked({
        let do_save = do_save.clone();
        move |_| do_save()
    });

    let root_box = gtk::Box::new(gtk::Orientation::Vertical, 0);
    if let Some(err) = cfg_err {
        let banner = adw::Banner::builder()
            .title(format!("Load error (showing defaults): {err}"))
            .revealed(true)
            .build();
        root_box.append(&banner);
    }
    root_box.append(&stack);

    let toolbar = adw::ToolbarView::new();
    toolbar.add_top_bar(&header);
    toolbar.set_content(Some(&root_box));
    toast_overlay.set_child(Some(&toolbar));

    let win = adw::ApplicationWindow::builder()
        .application(app)
        .title("T2 Touchbar Ultra")
        .default_width(820)
        .default_height(780)
        .content(&toast_overlay)
        .build();

    // Ctrl+S shortcut for fast Save & Restart
    let sc = gtk::ShortcutController::new();
    let save_action = gio::SimpleAction::new("save", None);
    save_action.connect_activate({
        let do_save = do_save.clone();
        move |_, _| do_save()
    });
    win.add_action(&save_action);
    if let Some(trigger) = gtk::ShortcutTrigger::parse_string("<Control>s") {
        sc.add_shortcut(gtk::Shortcut::new(
            Some(trigger),
            Some(gtk::NamedAction::new("win.save")),
        ));
    }
    win.add_controller(sc);

    win.present();
}

fn main() -> glib::ExitCode {
    let app = adw::Application::builder().application_id(APP_ID).build();
    app.connect_activate(build_ui);
    app.run()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rgba_rgb_roundtrip() {
        let orig = Rgb(0x1e, 0x90, 0xff);
        let round = rgb_from_rgba(&rgba_from_rgb(orig));
        assert_eq!(orig, round);
    }

    #[test]
    fn i18n_language_toggle_roundtrip() {
        assert_eq!(Lang::Ru.toggle(), Lang::En);
        assert_eq!(Lang::En.toggle(), Lang::Ru);
        assert_eq!(tr(Lang::Ru, "Сохранить", "Save"), "Сохранить");
        assert_eq!(tr(Lang::En, "Сохранить", "Save"), "Save");
    }
}
