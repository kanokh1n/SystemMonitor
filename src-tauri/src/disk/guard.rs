//! Защита от опасного удаления и само удаление в Корзину.
//!
//! Любое удаление из интерфейса проходит через `check`: системные папки,
//! корни домашней папки и данные Docker (там тома с базами) удалить нельзя.
//! Удаляем только в Корзину: из неё всё можно вернуть («Вернуть» в Finder).

use std::path::{Component, Path, PathBuf};

use serde::Serialize;
use sysinfo::{ProcessesToUpdate, System};

use super::home;

/// Эти папки нельзя удалить целиком, но внутри них удалять можно.
fn protected_exact(home: &Path) -> Vec<PathBuf> {
    let mut list: Vec<PathBuf> = [
        "/", "/Applications", "/Library", "/System", "/Users", "/Volumes", "/bin", "/cores", "/dev",
        "/etc", "/opt", "/opt/homebrew", "/private", "/sbin", "/tmp", "/usr", "/var",
    ]
    .iter()
    .map(PathBuf::from)
    .collect();
    for dir in [
        "", "Applications", "Desktop", "Documents", "Downloads", "Library", "Movies", "Music",
        "Pictures", "Public", "go", ".cargo", ".npm", ".Trash",
    ] {
        list.push(home.join(dir));
    }
    for dir in [
        "Application Support", "Caches", "CloudStorage", "Containers", "Group Containers", "Logs",
        "Mobile Documents", "Preferences", "Developer",
    ] {
        list.push(home.join("Library").join(dir));
    }
    list
}

/// Эти папки нельзя удалить ни целиком, ни по частям, ни вместе с родителем.
fn protected_trees(home: &Path) -> Vec<(PathBuf, &'static str)> {
    const DOCKER: &str = "данные Docker (тома с базами) удаляются только из самого Docker";
    const SYSTEM: &str = "системная папка macOS";
    const SECRETS: &str = "ключи и пароли";
    let h = |p: &str| home.join(p);
    vec![
        (PathBuf::from("/System"), SYSTEM),
        (PathBuf::from("/usr"), SYSTEM),
        (PathBuf::from("/bin"), SYSTEM),
        (PathBuf::from("/sbin"), SYSTEM),
        (PathBuf::from("/private"), SYSTEM),
        (PathBuf::from("/Library"), "общая папка всех пользователей, нужны права администратора"),
        (h(".ssh"), SECRETS),
        (h(".gnupg"), SECRETS),
        (h("Library/Keychains"), SECRETS),
        (h("Library/Containers/com.docker.docker"), DOCKER),
        (h("Library/Group Containers/group.com.docker"), DOCKER),
        (h(".docker"), DOCKER),
        (h(".orbstack"), DOCKER),
        (h("OrbStack"), DOCKER),
        (h("Library/Group Containers/HUAQ24HBR6.dev.orbstack"), DOCKER),
        (h(".colima"), DOCKER),
        (h(".lima"), DOCKER),
        (h(".rd"), DOCKER),
        (h("Library/Application Support/rancher-desktop"), DOCKER),
    ]
}

/// Можно ли удалить этот путь. `Err` объясняет, почему нельзя.
pub fn check(path: &Path) -> Result<(), String> {
    if !path.is_absolute() || path.components().any(|c| c == Component::ParentDir) {
        return Err("неполный путь".into());
    }
    let home = home();
    if protected_exact(&home).iter().any(|p| p == path) {
        return Err("основная папка, удалять можно только то, что внутри".into());
    }
    for (tree, why) in protected_trees(&home) {
        // Сам путь внутри защищённого, или защищённое лежит внутри пути.
        if path.starts_with(&tree) || tree.starts_with(path) {
            return Err(why.into());
        }
    }
    if path.extension().is_some_and(|e| e == "app") {
        if let Some(info) = super::apps::read_info(path) {
            if info.bundle_id.starts_with("com.apple.") {
                return Err("программа Apple, входит в macOS".into());
            }
        }
    }
    if std::env::current_exe().is_ok_and(|exe| exe.starts_with(path)) {
        return Err("это сам System Monitor".into());
    }
    Ok(())
}

/// Запущена ли программа из этой папки (.app) прямо сейчас.
fn running_inside(path: &Path) -> Option<String> {
    if path.extension().is_none_or(|e| e != "app") {
        return None;
    }
    let mut sys = System::new();
    sys.refresh_processes(ProcessesToUpdate::All, true);
    sys.processes()
        .values()
        .find(|p| p.exe().is_some_and(|exe| exe.starts_with(path)))
        .map(|p| p.name().to_string_lossy().into_owned())
}

#[derive(Serialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct TrashReport {
    pub trashed: Vec<String>,
    pub failed: Vec<(String, String)>,
}

/// Перемещает пути в Корзину.
///
/// Сначала через Finder: так у файлов в Корзине работает «Вернуть». Если Finder
/// не дал (например, не выдано разрешение управлять Finder), по одному через
/// системный NSFileManager: вернуть тогда можно, перетащив файл из Корзины.
pub fn to_trash(paths: &[String]) -> TrashReport {
    let mut report = TrashReport::default();
    let mut ok: Vec<PathBuf> = vec![];
    for p in paths {
        let path = PathBuf::from(p);
        if let Err(why) = check(&path) {
            report.failed.push((p.clone(), format!("Нельзя удалить: {why}")));
        } else if std::fs::symlink_metadata(&path).is_err() {
            report.failed.push((p.clone(), "Уже нет на диске".into()));
        } else if let Some(name) = running_inside(&path) {
            report.failed.push((p.clone(), format!("Программа запущена ({name}), сначала закрой её")));
        } else {
            ok.push(path);
        }
    }
    if ok.is_empty() {
        return report;
    }

    if trash::delete_all(&ok).is_err() {
        #[cfg(target_os = "macos")]
        {
            use trash::macos::{DeleteMethod, TrashContextExtMacos};
            let mut ctx = trash::TrashContext::default();
            ctx.set_delete_method(DeleteMethod::NsFileManager);
            for path in &ok {
                // Часть могла уйти в Корзину до ошибки: такие уже не существуют.
                if path.symlink_metadata().is_ok() {
                    if let Err(e) = ctx.delete(path) {
                        report.failed.push((path.to_string_lossy().into_owned(), friendly(&e.to_string())));
                    }
                }
            }
        }
    }
    for path in ok {
        let s = path.to_string_lossy().into_owned();
        if path.symlink_metadata().is_err() {
            report.trashed.push(s);
        } else if !report.failed.iter().any(|(f, _)| *f == s) {
            report.failed.push((s, "Не удалось переместить в Корзину".into()));
        }
    }
    report
}

#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
fn friendly(err: &str) -> String {
    if err.contains("permission") || err.contains("Operation not permitted") || err.contains("-10000") {
        "Нет прав. Для программ в «Программах» macOS может спросить пароль администратора".into()
    } else {
        err.into()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn protects_system_and_docker() {
        let h = home();
        assert!(check(Path::new("/")).is_err());
        assert!(check(Path::new("/System/Library")).is_err());
        assert!(check(&h).is_err());
        assert!(check(&h.join("Library")).is_err());
        assert!(check(&h.join("Library/Caches")).is_err());
        assert!(check(&h.join("Library/Containers/com.docker.docker/Data/vms")).is_err());
        assert!(check(&h.join(".ssh/id_ed25519")).is_err());
        assert!(check(Path::new("relative/path")).is_err());
        assert!(check(&h.join("Downloads/../Library")).is_err());
    }

    #[test]
    fn allows_ordinary_files() {
        let h = home();
        assert!(check(&h.join("Downloads/big.iso")).is_ok());
        assert!(check(&h.join("Library/Caches/com.spotify.client")).is_ok());
        assert!(check(&h.join("projects/app/node_modules")).is_ok());
    }
}
