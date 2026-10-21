//! Анализ и очистка диска (v2).
//!
//! - `scan` — обход папки в фоне: дерево размеров, крупные файлы, давно не менявшееся;
//! - `guard` — что удалять нельзя, и само удаление в Корзину;
//! - `apps` — программы и их «хвосты» в ~/Library, как в AppCleaner;
//! - `cleanup` — быстрые чистки: кэши, node_modules, старые версии IDE, Docker.

pub mod apps;
pub mod cleanup;
pub mod guard;
pub mod scan;

use std::fs::Metadata;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

use rayon::prelude::*;
use serde::Serialize;
use tauri::AppHandle;

pub fn home() -> PathBuf {
    PathBuf::from(std::env::var("HOME").unwrap_or_else(|_| "/".into()))
}

pub fn now() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0)
}

/// Сколько файл реально занимает на диске (как «На диске» в Finder), а не его
/// логический размер. Разница важна для разреженных файлов (образ Docker) и
/// для файлов iCloud, которые лежат только в облаке: у них блоков ноль.
pub fn allocated(meta: &Metadata) -> u64 {
    meta.blocks() * 512
}

/// Размер папки и время самого свежего изменения внутри неё.
/// Подпапки обходятся параллельно (rayon), ссылки не раскрываются.
pub fn du(path: &Path) -> (u64, i64) {
    let Ok(meta) = std::fs::symlink_metadata(path) else { return (0, 0) };
    if !meta.is_dir() {
        return (allocated(&meta), meta.mtime());
    }
    let Ok(entries) = std::fs::read_dir(path) else { return (allocated(&meta), meta.mtime()) };
    let entries: Vec<PathBuf> = entries.flatten().map(|e| e.path()).collect();
    entries
        .par_iter()
        .map(|p| du(p))
        .reduce(|| (allocated(&meta), meta.mtime()), |a, b| (a.0 + b.0, a.1.max(b.1)))
}

/// Один пункт для удаления: путь, размер и когда внутри что-то менялось.
#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct Item {
    pub path: String,
    pub label: String,
    pub size: u64,
    pub newest: i64,
    /// Отмечен ли пункт по умолчанию.
    pub selected: bool,
    /// Удалить нельзя (системное, база Docker и т. п.), поясняет почему.
    pub protected: Option<String>,
}

impl Item {
    pub fn measure(path: &Path, label: impl Into<String>, selected: bool) -> Item {
        let (size, newest) = du(path);
        Item {
            path: path.to_string_lossy().into_owned(),
            label: label.into(),
            size,
            newest,
            selected,
            protected: guard::check(path).err(),
        }
    }
}

// ---------- Мелкие команды для интерфейса ----------

/// Показать файл в Finder (выделить его в окне папки).
#[tauri::command]
pub fn reveal_in_finder(path: String) -> Result<(), String> {
    Command::new("open").arg("-R").arg(&path).status().map_err(|e| e.to_string())?;
    Ok(())
}

/// Открыть «Системные настройки → Конфиденциальность → Полный доступ к диску».
#[tauri::command]
pub fn open_full_disk_access() -> Result<(), String> {
    Command::new("open")
        .arg("x-apple.systempreferences:com.apple.preference.security?Privacy_AllFiles")
        .status()
        .map_err(|e| e.to_string())?;
    Ok(())
}

/// Есть ли у приложения «Полный доступ к диску». Папку базы TCC без него
/// прочитать нельзя, а есть она на любом маке.
#[tauri::command]
pub fn has_full_disk_access() -> bool {
    std::fs::read_dir("/Library/Application Support/com.apple.TCC").is_ok()
}

/// Системное окно выбора папки для сканирования.
#[tauri::command]
pub async fn pick_folder(app: AppHandle) -> Option<String> {
    use tauri_plugin_dialog::DialogExt;
    let picked = app
        .dialog()
        .file()
        .set_title("Какую папку проанализировать")
        .set_directory(home())
        .blocking_pick_folder()?;
    picked.into_path().ok().map(|p| p.to_string_lossy().into_owned())
}
