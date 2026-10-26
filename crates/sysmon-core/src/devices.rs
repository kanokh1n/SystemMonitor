//! Bluetooth‑устройства и их заряд: наушники, мышь, клавиатура, трекпад.
//!
//! Список и заряд берём из `system_profiler SPBluetoothDataType -json` (там есть
//! AirPods с левым/правым/кейсом) и дополняем из `ioreg`: Magic Mouse, Keyboard,
//! Trackpad и некоторые сторонние мыши отдают заряд только туда.
//! `system_profiler` работает около секунды, поэтому опрашиваем раз в 30 секунд.
//!
//! Последний известный заряд каждого устройства сохраняем в `devices.json`
//! в папке данных приложения: отключённые наушники показываем с зарядом,
//! который был, когда они были подключены.
//!
//! Как часто опрашивать, решает тот, кто держит [`Store`]: окно — раз в [`POLL`],
//! панель — только пока открыта вкладка «Устройства».

use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;
use std::process::Command;
use std::sync::Mutex;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use serde_json::Value;

pub const POLL: Duration = Duration::from_secs(30);

/// Заряд одной части устройства. У AirPods их три: левый, правый и кейс.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Level {
    /// `main`, `left`, `right` или `case`.
    pub part: String,
    pub percent: u8,
}

#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct Device {
    /// MAC‑адрес, а если его нет — имя.
    pub id: String,
    pub name: String,
    /// `headphones`, `mouse`, `keyboard`, `trackpad`, `gamepad`, `speaker`, `phone`, `watch` или `other`.
    pub kind: String,
    pub connected: bool,
    pub levels: Vec<Level>,
    /// Заряд получен только что, а не взят из сохранённого.
    pub live: bool,
    /// Когда заряд был измерен (секунды Unix).
    pub levels_at: Option<u64>,
    /// Когда устройство последний раз было подключено (секунды Unix).
    pub last_seen: Option<u64>,
}

/// То, что помним об устройстве между запусками.
#[derive(Serialize, Deserialize, Clone, Default)]
#[serde(rename_all = "camelCase")]
struct Remembered {
    levels: Vec<Level>,
    levels_at: Option<u64>,
    last_seen: Option<u64>,
}

/// Устройство, как его видит система прямо сейчас.
#[derive(Debug, PartialEq)]
struct Found {
    id: String,
    name: String,
    kind: String,
    connected: bool,
    levels: Vec<Level>,
}

pub struct Store {
    path: Option<PathBuf>,
    known: Mutex<HashMap<String, Remembered>>,
}

impl Store {
    pub fn load(path: Option<PathBuf>) -> Self {
        let known = path
            .as_ref()
            .and_then(|p| fs::read(p).ok())
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default();
        Self { path, known: Mutex::new(known) }
    }

    /// Обновляет сохранённый заряд подключённых устройств и собирает итоговый список.
    fn merge(&self, found: Vec<Found>, now: u64) -> Vec<Device> {
        let mut known = self.known.lock().unwrap();
        let mut changed = false;
        let mut devices: Vec<Device> = found
            .into_iter()
            .map(|f| {
                let mem = known.entry(f.id.clone()).or_default();
                let live = f.connected && !f.levels.is_empty();
                if f.connected {
                    mem.last_seen = Some(now);
                    changed = true;
                }
                if live {
                    mem.levels = f.levels;
                    mem.levels_at = Some(now);
                }
                Device {
                    id: f.id,
                    name: f.name,
                    kind: f.kind,
                    connected: f.connected,
                    levels: mem.levels.clone(),
                    live,
                    levels_at: mem.levels_at,
                    last_seen: mem.last_seen,
                }
            })
            .collect();
        if changed {
            self.save(&known);
        }
        // Сначала подключённые, потом те, что недавно были рядом.
        devices.sort_by(|a, b| {
            (!a.connected, std::cmp::Reverse(a.last_seen), &a.name).cmp(&(!b.connected, std::cmp::Reverse(b.last_seen), &b.name))
        });
        devices
    }

    fn save(&self, known: &HashMap<String, Remembered>) {
        let Some(path) = &self.path else { return };
        if let Some(dir) = path.parent() {
            let _ = fs::create_dir_all(dir);
        }
        if let Ok(json) = serde_json::to_vec_pretty(known) {
            let _ = fs::write(path, json);
        }
    }
}

fn run(cmd: &str, args: &[&str]) -> Option<String> {
    let out = Command::new(cmd).args(args).output().ok()?;
    out.status.success().then(|| String::from_utf8_lossy(&out.stdout).into_owned())
}

fn now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

/// `AA-BB-cc-...` и `aa:bb:...` приводим к одному виду `AA:BB:CC:...`.
fn normalize_address(addr: &str) -> String {
    addr.trim().replace('-', ":").to_uppercase()
}

/// `"85%"` или `85` → 85.
fn parse_percent(v: &Value) -> Option<u8> {
    let n = match v {
        Value::Number(n) => n.as_f64()?,
        Value::String(s) => s.trim().trim_end_matches('%').trim().parse().ok()?,
        _ => return None,
    };
    (0.0..=100.0).contains(&n).then_some(n.round() as u8)
}

fn kind_of(minor_type: &str, name: &str) -> String {
    let t = format!("{} {}", minor_type, name).to_lowercase();
    let kind = [
        ("headphone", "headphones"),
        ("headset", "headphones"),
        ("airpods", "headphones"),
        ("beats", "headphones"),
        ("earbud", "headphones"),
        ("mouse", "mouse"),
        ("keyboard", "keyboard"),
        ("trackpad", "trackpad"),
        ("gamepad", "gamepad"),
        ("joystick", "gamepad"),
        ("controller", "gamepad"),
        ("speaker", "speaker"),
        ("phone", "phone"),
        ("watch", "watch"),
    ]
    .iter()
    .find(|(needle, _)| t.contains(needle))
    .map_or("other", |(_, kind)| kind);
    kind.to_string()
}

/// Разбирает JSON из `system_profiler SPBluetoothDataType -json`.
///
/// Новые macOS кладут устройства в `device_connected` / `device_not_connected`,
/// старые (до Ventura) — в `device_title` с полем `device_isconnected`.
fn parse_system_profiler(json: &str) -> Vec<Found> {
    let Ok(root) = serde_json::from_str::<Value>(json) else { return vec![] };
    let mut found = Vec::new();
    let controllers = root["SPBluetoothDataType"].as_array().cloned().unwrap_or_default();
    for controller in &controllers {
        for (key, connected) in [("device_connected", Some(true)), ("device_not_connected", Some(false)), ("device_title", None)] {
            let Some(list) = controller[key].as_array() else { continue };
            // Каждый элемент — объект из одного ключа: `{ "AirPods Pro": { ...свойства } }`.
            for (name, props) in list.iter().filter_map(Value::as_object).flatten() {
                let connected = connected.unwrap_or_else(|| props["device_isconnected"].as_str() == Some("attrib_Yes"));
                let levels = [
                    ("device_batteryLevelMain", "main"),
                    ("device_batteryLevel", "main"),
                    ("device_batteryLevelLeft", "left"),
                    ("device_batteryLevelRight", "right"),
                    ("device_batteryLevelCase", "case"),
                ]
                .iter()
                .filter_map(|(field, part)| Some(Level { part: part.to_string(), percent: parse_percent(&props[*field])? }))
                .fold(Vec::<Level>::new(), |mut acc, l| {
                    if !acc.iter().any(|a| a.part == l.part) {
                        acc.push(l);
                    }
                    acc
                });
                let id = props["device_address"].as_str().map(normalize_address).unwrap_or_else(|| name.clone());
                let minor = props["device_minorType"].as_str().unwrap_or_default();
                found.push(Found { id, name: name.clone(), kind: kind_of(minor, name), connected, levels });
            }
        }
    }
    found
}

/// Заряд из реестра устройств: `ioreg -r -d 1 -l -k BatteryPercent`.
/// Возвращает пары (адрес или имя, название, процент).
fn parse_ioreg(text: &str) -> Vec<(Option<String>, String, u8)> {
    text.split("+-o ")
        .skip(1)
        .filter_map(|block| {
            let field = |key: &str| {
                let prefix = format!("\"{key}\" = ");
                block.lines().find_map(|l| l.trim().trim_start_matches('|').trim().strip_prefix(&prefix).map(str::to_string))
            };
            let percent: u8 = field("BatteryPercent")?.trim().parse().ok().filter(|p| *p <= 100)?;
            let name = field("Product")?.trim_matches('"').to_string();
            let address = field("DeviceAddress").map(|a| normalize_address(a.trim_matches('"')));
            Some((address, name, percent))
        })
        .collect()
}

/// Добавляет заряд из ioreg к устройствам, у которых его нет. Ищем по адресу, потом по имени.
fn merge_ioreg(found: &mut Vec<Found>, extra: Vec<(Option<String>, String, u8)>) {
    for (address, name, percent) in extra {
        let index = found
            .iter()
            .position(|d| address.as_ref() == Some(&d.id))
            .or_else(|| found.iter().position(|d| d.name == name));
        let level = Level { part: "main".into(), percent };
        match index {
            Some(i) => {
                let d = &mut found[i];
                d.connected = true;
                if d.levels.is_empty() {
                    d.levels.push(level);
                }
            }
            // Устройство не по Bluetooth (например, по кабелю), но с батареей.
            None => found.push(Found {
                id: address.unwrap_or_else(|| name.clone()),
                kind: kind_of("", &name),
                name,
                connected: true,
                levels: vec![level],
            }),
        }
    }
}

fn scan() -> Vec<Found> {
    let mut found = run("system_profiler", &["SPBluetoothDataType", "-json"])
        .map(|j| parse_system_profiler(&j))
        .unwrap_or_default();
    let extra = run("ioreg", &["-r", "-d", "1", "-l", "-k", "BatteryPercent"])
        .map(|t| parse_ioreg(&t))
        .unwrap_or_default();
    merge_ioreg(&mut found, extra);
    found
}

/// Опрашивает устройства (около секунды) и обновляет сохранённый заряд.
pub fn refresh(store: &Store) -> Vec<Device> {
    store.merge(scan(), now())
}

#[cfg(test)]
mod tests {
    use super::*;

    const PROFILER: &str = r#"{
      "SPBluetoothDataType": [{
        "controller_properties": { "controller_address": "11:22:33:44:55:66" },
        "device_connected": [
          { "AirPods Pro": {
              "device_address": "aa:bb:cc:dd:ee:01",
              "device_batteryLevelCase": "45%",
              "device_batteryLevelLeft": "100%",
              "device_batteryLevelRight": "98%",
              "device_minorType": "Headphones" } },
          { "Magic Mouse": { "device_address": "AA:BB:CC:DD:EE:02", "device_minorType": "Mouse" } }
        ],
        "device_not_connected": [
          { "Sony WH-1000XM4": { "device_address": "AA:BB:CC:DD:EE:03", "device_minorType": "Headset" } }
        ]
      }]
    }"#;

    const IOREG: &str = r#"+-o AppleDeviceManagementHIDEventService  <class AppleDeviceManagementHIDEventService, id 0x100000a1b>
    {
      "LowBatteryNotificationPercentage" = 2
      "BatteryPercent" = 64
      "Product" = "Magic Mouse"
      "DeviceAddress" = "aa-bb-cc-dd-ee-02"
    }
+-o AppleDeviceManagementHIDEventService  <class AppleDeviceManagementHIDEventService, id 0x100000a2c>
    {
      "BatteryPercent" = 81
      "Product" = "MX Master 3"
    }
"#;

    #[test]
    fn parses_system_profiler() {
        let found = parse_system_profiler(PROFILER);
        assert_eq!(found.len(), 3);
        let pods = &found[0];
        assert_eq!(pods.id, "AA:BB:CC:DD:EE:01");
        assert_eq!(pods.kind, "headphones");
        assert!(pods.connected);
        let parts: Vec<_> = pods.levels.iter().map(|l| (l.part.as_str(), l.percent)).collect();
        assert_eq!(parts, vec![("left", 100), ("right", 98), ("case", 45)]);
        assert_eq!(found[1].kind, "mouse");
        assert!(found[1].levels.is_empty());
        assert_eq!(found[2].kind, "headphones");
        assert!(!found[2].connected);
    }

    #[test]
    fn parses_old_system_profiler() {
        let json = r#"{"SPBluetoothDataType":[{"device_title":[
            {"Magic Keyboard":{"device_address":"AA-BB-CC-DD-EE-04","device_isconnected":"attrib_Yes","device_batteryLevel":"70%","device_minorType":"Keyboard"}}
        ]}]}"#;
        let found = parse_system_profiler(json);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].id, "AA:BB:CC:DD:EE:04");
        assert!(found[0].connected);
        assert_eq!(found[0].levels, vec![Level { part: "main".into(), percent: 70 }]);
    }

    #[test]
    fn adds_ioreg_battery() {
        let mut found = parse_system_profiler(PROFILER);
        merge_ioreg(&mut found, parse_ioreg(IOREG));
        assert_eq!(found[1].levels, vec![Level { part: "main".into(), percent: 64 }]);
        let mx = found.iter().find(|d| d.name == "MX Master 3").unwrap();
        assert_eq!(mx.kind, "other");
        assert_eq!(mx.levels[0].percent, 81);
    }

    #[test]
    fn remembers_last_charge() {
        let store = Store::load(None);
        let pods = |connected: bool, levels: Vec<Level>| Found {
            id: "A".into(),
            name: "AirPods".into(),
            kind: "headphones".into(),
            connected,
            levels,
        };
        store.merge(vec![pods(true, vec![Level { part: "left".into(), percent: 90 }])], 100);
        let later = store.merge(vec![pods(false, vec![])], 500);
        assert!(!later[0].live);
        assert_eq!(later[0].levels[0].percent, 90);
        assert_eq!(later[0].levels_at, Some(100));
        assert_eq!(later[0].last_seen, Some(100));
    }
}
