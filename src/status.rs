// SPDX-License-Identifier: GPL-3.0-or-later

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::OnceLock;

static PREV_CPU_TOTAL: AtomicU64 = AtomicU64::new(0);
static PREV_CPU_IDLE: AtomicU64 = AtomicU64::new(0);

pub fn read_cpu_percent() -> Option<u32> {
    let content = fs::read_to_string("/proc/stat").ok()?;
    let first_line = content.lines().next()?;
    let rest = first_line.strip_prefix("cpu ")?.trim_start();
    let mut parts = rest.split_whitespace();
    let user: u64 = parts.next()?.parse().ok()?;
    let nice: u64 = parts.next()?.parse().ok()?;
    let system: u64 = parts.next()?.parse().ok()?;
    let idle: u64 = parts.next()?.parse().ok()?;
    let iowait: u64 = parts.next().and_then(|s| s.parse().ok()).unwrap_or(0);
    let irq: u64 = parts.next().and_then(|s| s.parse().ok()).unwrap_or(0);
    let softirq: u64 = parts.next().and_then(|s| s.parse().ok()).unwrap_or(0);
    let steal: u64 = parts.next().and_then(|s| s.parse().ok()).unwrap_or(0);

    let idle_time = idle + iowait;
    let non_idle = user + nice + system + irq + softirq + steal;
    let total = idle_time + non_idle;

    let prev_total = PREV_CPU_TOTAL.swap(total, Ordering::Relaxed);
    let prev_idle = PREV_CPU_IDLE.swap(idle_time, Ordering::Relaxed);

    if prev_total > 0 && total > prev_total {
        let delta_total = total - prev_total;
        let delta_idle = idle_time.saturating_sub(prev_idle);
        let used = delta_total.saturating_sub(delta_idle);
        let percent = ((used as f64 / delta_total as f64) * 100.0).round() as u32;
        Some(percent.min(100))
    } else {
        Some(0)
    }
}

static CPU_TEMP_PATH: OnceLock<Option<PathBuf>> = OnceLock::new();

fn find_cpu_temp_path() -> Option<PathBuf> {
    // 1. Check thermal zones (e.g. x86_pkg_temp or cpu)
    if let Ok(entries) = fs::read_dir("/sys/class/thermal") {
        for entry in entries.flatten() {
            let path = entry.path();
            if let Some(file_name) = path.file_name().and_then(|s| s.to_str())
                && file_name.starts_with("thermal_zone")
            {
                let t_type = fs::read_to_string(path.join("type")).unwrap_or_default();
                let lower = t_type.trim().to_lowercase();
                if lower.contains("x86_pkg_temp") || lower.contains("cpu") || lower.contains("coretemp") {
                    let temp_path = path.join("temp");
                    if temp_path.is_file() {
                        return Some(temp_path);
                    }
                }
            }
        }
    }

    // 2. Check hwmon (coretemp, k10temp, zenpower, cpu)
    if let Ok(entries) = fs::read_dir("/sys/class/hwmon") {
        for entry in entries.flatten() {
            let path = entry.path();
            let name = fs::read_to_string(path.join("name")).unwrap_or_default();
            let name = name.trim();
            if name == "coretemp" || name == "k10temp" || name == "zenpower" || name.contains("cpu") {
                let temp_path = path.join("temp1_input");
                if temp_path.is_file() {
                    return Some(temp_path);
                }
            }
        }
    }

    // 3. Fallback to thermal_zone0 if valid
    let tz0 = PathBuf::from("/sys/class/thermal/thermal_zone0/temp");
    if tz0.is_file() {
        return Some(tz0);
    }

    None
}

pub fn read_cpu_temp() -> Option<u32> {
    let path = CPU_TEMP_PATH.get_or_init(find_cpu_temp_path).as_ref()?;
    let content = fs::read_to_string(path).ok()?;
    let millidegrees = content.trim().parse::<i64>().ok()?;
    if millidegrees > 0 {
        Some((millidegrees / 1000) as u32)
    } else {
        None
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BatteryPowerState {
    /// Running on battery (discharging)
    Discharging,
    /// Actively charging from AC power
    Charging,
    /// Connected to AC power directly (charge threshold, bypass, not charging)
    DirectMains,
    /// Connected to AC power, battery full (100%)
    Full,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BatteryStatus {
    pub percent: u32,
    pub state: BatteryPowerState,
    pub power_mw: Option<u32>,
}

impl BatteryStatus {
    #[allow(dead_code)]
    pub fn is_charging(&self) -> bool {
        self.state == BatteryPowerState::Charging
    }

    pub fn is_ac_online(&self) -> bool {
        self.state != BatteryPowerState::Discharging
    }

    pub fn power_watts(&self) -> Option<f32> {
        self.power_mw.map(|mw| mw as f32 / 1000.0)
    }
}

struct BatteryPaths {
    capacity: PathBuf,
    status: PathBuf,
    ac_online: Option<PathBuf>,
    power_now: Option<PathBuf>,
    voltage_now: Option<PathBuf>,
    current_now: Option<PathBuf>,
    smc_system_power: Option<PathBuf>,
}

static BATTERY_PATHS: OnceLock<Option<BatteryPaths>> = OnceLock::new();

fn find_smc_system_power() -> Option<PathBuf> {
    let hwmon_dir = Path::new("/sys/class/hwmon");
    if let Ok(entries) = fs::read_dir(hwmon_dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            let name = fs::read_to_string(path.join("name")).unwrap_or_default();
            let name = name.trim();
            if name == "t2smc" || name == "applesmc" || name == "macsmc" {
                if let Ok(files) = fs::read_dir(&path) {
                    for f in files.flatten() {
                        let fname = f.file_name().to_string_lossy().to_string();
                        if fname.starts_with("power") && fname.ends_with("_label") {
                            if let Ok(lbl) = fs::read_to_string(f.path()) {
                                if lbl.trim() == "PSTR" {
                                    let input_name = fname.replace("_label", "_input");
                                    let input_path = path.join(input_name);
                                    if input_path.is_file() {
                                        return Some(input_path);
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    }
    None
}

fn find_battery_paths() -> Option<BatteryPaths> {
    let dir = Path::new("/sys/class/power_supply");
    let mut ac_online = None;
    let mut bat_dir = None;

    if let Ok(entries) = fs::read_dir(dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            let supply_type = fs::read_to_string(path.join("type")).unwrap_or_default();
            let supply_type = supply_type.trim();
            if (supply_type == "Mains" || supply_type == "USB" || supply_type == "AC") && ac_online.is_none() {
                let online = path.join("online");
                if online.is_file() {
                    ac_online = Some(online);
                }
            } else if supply_type == "Battery" && bat_dir.is_none() {
                bat_dir = Some(path);
            }
        }
    }

    let bat = bat_dir.or_else(|| {
        let bat0 = PathBuf::from("/sys/class/power_supply/BAT0");
        if bat0.is_dir() { Some(bat0) } else { None }
    })?;

    let power_now = bat.join("power_now");
    let voltage_now = bat.join("voltage_now");
    let current_now = bat.join("current_now");

    Some(BatteryPaths {
        capacity: bat.join("capacity"),
        status: bat.join("status"),
        ac_online,
        power_now: if power_now.is_file() { Some(power_now) } else { None },
        voltage_now: if voltage_now.is_file() { Some(voltage_now) } else { None },
        current_now: if current_now.is_file() { Some(current_now) } else { None },
        smc_system_power: find_smc_system_power(),
    })
}

pub fn read_battery() -> Option<BatteryStatus> {
    let paths = BATTERY_PATHS.get_or_init(find_battery_paths).as_ref()?;
    let capacity = fs::read_to_string(&paths.capacity)
        .ok()
        .and_then(|s| s.trim().parse::<u32>().ok())
        .unwrap_or(0)
        .min(100);
    let status = fs::read_to_string(&paths.status).unwrap_or_default();
    let ac_online_adapter = paths.ac_online.as_ref().is_some_and(|p| {
        fs::read_to_string(p).ok().is_some_and(|s| s.trim() == "1")
    });

    let power_mw = if let Some(smc_p) = &paths.smc_system_power {
        fs::read_to_string(smc_p)
            .ok()
            .and_then(|s| s.trim().parse::<u64>().ok())
            .map(|uw| (uw / 1000) as u32)
    } else {
        None
    }
    .or_else(|| {
        if let Some(p) = &paths.power_now {
            fs::read_to_string(p)
                .ok()
                .and_then(|s| s.trim().parse::<u64>().ok())
                .map(|uw| (uw / 1000) as u32)
        } else if let (Some(v_path), Some(c_path)) = (&paths.voltage_now, &paths.current_now) {
            let v = fs::read_to_string(v_path).ok().and_then(|s| s.trim().parse::<u64>().ok());
            let c = fs::read_to_string(c_path).ok().and_then(|s| s.trim().parse::<u64>().ok());
            match (v, c) {
                (Some(v), Some(c)) if v > 0 && c > 0 => {
                    let mw = (v * c / 1_000_000_000) as u32;
                    Some(mw)
                }
                _ => None,
            }
        } else {
            None
        }
    });

    let state = match status.trim() {
        "Charging" => BatteryPowerState::Charging,
        "Full" => BatteryPowerState::Full,
        "Not charging" => BatteryPowerState::DirectMains,
        "Discharging" => {
            if ac_online_adapter {
                BatteryPowerState::DirectMains
            } else {
                BatteryPowerState::Discharging
            }
        }
        _ => {
            if ac_online_adapter {
                BatteryPowerState::DirectMains
            } else {
                BatteryPowerState::Discharging
            }
        }
    };
    Some(BatteryStatus {
        percent: capacity,
        state,
        power_mw,
    })
}

pub fn is_ac_online() -> bool {
    if let Some(bat) = read_battery() {
        bat.is_ac_online()
    } else {
        true
    }
}

pub fn read_ram_percent() -> Option<u32> {
    let content = fs::read_to_string("/proc/meminfo").ok()?;
    let mut total_kb = 0.0f32;
    let mut avail_kb = 0.0f32;
    for line in content.lines() {
        if let Some(rest) = line.strip_prefix("MemTotal:") {
            total_kb = rest.split_whitespace().next()?.parse().ok()?;
        } else if let Some(rest) = line.strip_prefix("MemAvailable:") {
            avail_kb = rest.split_whitespace().next()?.parse().ok()?;
        }
        if total_kb > 0.0 && avail_kb > 0.0 {
            break;
        }
    }
    if total_kb > 0.0 {
        let percent = ((total_kb - avail_kb) / total_kb * 100.0f32).round() as u32;
        Some(percent)
    } else {
        None
    }
}

#[allow(dead_code)]
pub fn read_ram_usage() -> Option<(f32, f32)> {
    let content = fs::read_to_string("/proc/meminfo").ok()?;
    let mut total_kb = 0.0f32;
    let mut avail_kb = 0.0f32;
    for line in content.lines() {
        if let Some(rest) = line.strip_prefix("MemTotal:") {
            total_kb = rest.split_whitespace().next()?.parse().ok()?;
        } else if let Some(rest) = line.strip_prefix("MemAvailable:") {
            avail_kb = rest.split_whitespace().next()?.parse().ok()?;
        }
        if total_kb > 0.0 && avail_kb > 0.0 {
            break;
        }
    }
    if total_kb > 0.0 {
        let used_gb = (total_kb - avail_kb) / 1024.0 / 1024.0;
        let total_gb = total_kb / 1024.0 / 1024.0;
        Some((used_gb, total_gb))
    } else {
        None
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ClockWidgetData {
    pub time: String,
    pub weekday: String,
    pub date: String,
}

impl Default for ClockWidgetData {
    fn default() -> Self {
        Self {
            time: "08:36".to_string(),
            weekday: "Четверг".to_string(),
            date: "29 октября".to_string(),
        }
    }
}

pub fn read_clock_data() -> ClockWidgetData {
    unsafe {
        let mut now: libc::time_t = 0;
        libc::time(&mut now);
        let mut tm: libc::tm = std::mem::zeroed();
        libc::localtime_r(&now, &mut tm);

        let weekdays = [
            "Воскресенье",
            "Понедельник",
            "Вторник",
            "Среда",
            "Четверг",
            "Пятница",
            "Суббота",
        ];
        let months = [
            "января", "февраля", "марта", "апреля", "мая", "июня",
            "июля", "августа", "сентября", "октября", "ноября", "декабря",
        ];
        let weekday = if (0..7).contains(&tm.tm_wday) {
            weekdays[tm.tm_wday as usize].to_string()
        } else {
            String::new()
        };
        let month = if (0..12).contains(&tm.tm_mon) {
            months[tm.tm_mon as usize]
        } else {
            ""
        };
        let time = format!("{:02}:{:02}", tm.tm_hour, tm.tm_min);
        let date = format!("{} {}", tm.tm_mday, month);

        ClockWidgetData {
            time,
            weekday,
            date,
        }
    }
}

#[allow(dead_code)]
pub fn format_idle_time() -> String {
    let clock = read_clock_data();
    format!("{} {}, {}", clock.time, clock.weekday, clock.date)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn format_idle_time_is_not_empty() {
        let time_str = format_idle_time();
        assert!(time_str.contains(':'));
        assert!(time_str.len() >= 10);
    }

    #[test]
    fn test_read_clock_data() {
        let clock = read_clock_data();
        assert_eq!(clock.time.len(), 5);
        assert!(clock.time.contains(':'));
        assert!(!clock.weekday.is_empty());
        assert!(!clock.date.is_empty());
    }

    #[test]
    fn battery_power_state_classification() {
        let bat = BatteryStatus {
            percent: 80,
            state: BatteryPowerState::DirectMains,
            power_mw: None,
        };
        assert!(bat.is_ac_online());
        assert!(!bat.is_charging());

        let bat_dis = BatteryStatus {
            percent: 50,
            state: BatteryPowerState::Discharging,
            power_mw: Some(15000),
        };
        assert!(!bat_dis.is_ac_online());
        assert_eq!(bat_dis.power_watts(), Some(15.0));
    }

    #[test]
    fn test_live_battery_and_smc_power() {
        if let Some(bat) = read_battery() {
            println!("Live Battery: {}%, state: {:?}, power: {:?} mW ({:?} W)",
                bat.percent, bat.state, bat.power_mw, bat.power_watts());
            assert!(bat.percent <= 100);
            if let Some(w) = bat.power_watts() {
                assert!(w > 0.0 && w < 200.0);
            }
        }
    }

    #[test]
    fn test_cpu_percent_reads() {
        let cpu = read_cpu_percent();
        assert!(cpu.is_some());
    }

    #[test]
    fn test_cpu_temp_reads() {
        let temp = read_cpu_temp();
        // On hardware with thermal zones, this will read a non-zero temperature
        if let Some(t) = temp {
            assert!(t > 0 && t < 120);
        }
    }
}
