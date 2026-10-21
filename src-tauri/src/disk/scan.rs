//! Сканер папок: обходит папку в фоне и строит дерево размеров.
//!
//! Обход параллельный (rayon): каждая подпапка — отдельная задача в пуле потоков,
//! поэтому SSD читается в несколько потоков, а не по одному файлу. Пока идёт скан,
//! раз в 150 мс во фронтенд уходит событие `disk-scan` с прогрессом.
//!
//! Чтобы дерево домашней папки (это миллионы файлов) не съело память, храним:
//! все папки от 1 МБ, все файлы от 1 МБ, а мелочь внутри папки сворачиваем
//! в одну строку «остальное».

use std::collections::HashSet;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use rayon::prelude::*;
use serde::Serialize;
use tauri::{AppHandle, Emitter};

use super::{allocated, guard, now};

/// Файлы и папки меньше этого не храним по отдельности.
const KEEP_MIN: u64 = 1024 * 1024;
const PROGRESS_EVERY: Duration = Duration::from_millis(150);

pub struct Node {
    name: Box<str>,
    size: u64,
    /// Сколько файлов внутри (для файла — 1).
    files: u64,
    /// Самое свежее изменение внутри, unix‑время в секундах.
    newest: i64,
    is_dir: bool,
    /// Папку не удалось прочитать (нет прав).
    denied: bool,
    children: Vec<Node>,
    /// Мелкие файлы и папки, свёрнутые в одну строку.
    rest_size: u64,
    rest_count: u64,
}

/// Общее для всех потоков одного скана.
struct Ctx {
    dev: u64,
    cancel: Arc<AtomicBool>,
    files: AtomicU64,
    bytes: AtomicU64,
    denied: Mutex<Vec<String>>,
    current: Mutex<String>,
    /// Жёсткие ссылки (один файл под разными именами) считаем один раз.
    hardlinks: Mutex<HashSet<(u64, u64)>>,
}

fn scan_dir(path: &Path, name: Box<str>, meta: &std::fs::Metadata, ctx: &Ctx) -> Node {
    let mut node = Node {
        name,
        size: allocated(meta),
        files: 0,
        newest: meta.mtime(),
        is_dir: true,
        denied: false,
        children: vec![],
        rest_size: 0,
        rest_count: 0,
    };
    if ctx.cancel.load(Ordering::Relaxed) {
        return node;
    }
    if let Ok(mut current) = ctx.current.try_lock() {
        *current = path.to_string_lossy().into_owned();
    }
    let entries = match std::fs::read_dir(path) {
        Ok(e) => e,
        Err(_) => {
            node.denied = true;
            let mut denied = ctx.denied.lock().unwrap();
            if denied.len() < 50 {
                denied.push(path.to_string_lossy().into_owned());
            }
            return node;
        }
    };

    let mut subdirs = vec![];
    for entry in entries.flatten() {
        // DirEntry::metadata не идёт по символьным ссылкам — это то, что нужно.
        let Ok(meta) = entry.metadata() else { continue };
        let name: Box<str> = entry.file_name().to_string_lossy().into();
        if meta.is_dir() {
            // Другие диски (флешки, образы) внутри папки не считаем.
            if meta.dev() == ctx.dev {
                subdirs.push((entry.path(), name, meta));
            }
            continue;
        }
        let mut size = allocated(&meta);
        if meta.nlink() > 1 && meta.is_file() && !ctx.hardlinks.lock().unwrap().insert((meta.dev(), meta.ino())) {
            size = 0;
        }
        ctx.files.fetch_add(1, Ordering::Relaxed);
        ctx.bytes.fetch_add(size, Ordering::Relaxed);
        node.files += 1;
        node.newest = node.newest.max(meta.mtime());
        node.size += size;
        if size >= KEEP_MIN {
            node.children.push(Node {
                name,
                size,
                files: 1,
                newest: meta.mtime(),
                is_dir: false,
                denied: false,
                children: vec![],
                rest_size: 0,
                rest_count: 0,
            });
        } else {
            node.rest_size += size;
            node.rest_count += 1;
        }
    }

    let dirs: Vec<Node> = subdirs
        .into_par_iter()
        .map(|(p, name, meta)| scan_dir(&p, name, &meta, ctx))
        .collect();
    for dir in dirs {
        node.size += dir.size;
        node.files += dir.files;
        node.newest = node.newest.max(dir.newest);
        if dir.size >= KEEP_MIN || dir.denied {
            node.children.push(dir);
        } else {
            node.rest_size += dir.size;
            node.rest_count += 1;
        }
    }
    node.children.sort_unstable_by(|a, b| b.size.cmp(&a.size));
    node
}

// ---------- Состояние: один скан на всё приложение ----------

struct Scan {
    root: PathBuf,
    tree: Option<Node>,
    denied: Vec<String>,
    cancel: Arc<AtomicBool>,
}

static SCAN: Mutex<Option<Scan>> = Mutex::new(None);

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
struct Progress {
    root: String,
    running: bool,
    cancelled: bool,
    files: u64,
    bytes: u64,
    current: String,
    elapsed_ms: u64,
    /// Сколько папок не удалось прочитать (подсказка про полный доступ к диску).
    denied: usize,
}

/// Запускает скан папки (по умолчанию домашней). Предыдущий скан отменяется.
#[tauri::command]
pub fn disk_scan_start(app: AppHandle, path: Option<String>) -> Result<(), String> {
    let root = path.map(PathBuf::from).unwrap_or_else(super::home);
    let meta = std::fs::metadata(&root).map_err(|e| format!("{}: {e}", root.display()))?;
    if !meta.is_dir() {
        return Err("Это не папка".into());
    }
    let cancel = Arc::new(AtomicBool::new(false));
    {
        let mut scan = SCAN.lock().unwrap();
        if let Some(old) = scan.as_ref() {
            old.cancel.store(true, Ordering::Relaxed);
        }
        *scan = Some(Scan { root: root.clone(), tree: None, denied: vec![], cancel: cancel.clone() });
    }

    std::thread::spawn(move || {
        let ctx = Arc::new(Ctx {
            dev: meta.dev(),
            cancel: cancel.clone(),
            files: AtomicU64::new(0),
            bytes: AtomicU64::new(0),
            denied: Mutex::new(vec![]),
            current: Mutex::new(String::new()),
            hardlinks: Mutex::new(HashSet::new()),
        });
        let started = Instant::now();
        let done = Arc::new(AtomicBool::new(false));
        let progress = |running: bool| Progress {
            root: root.to_string_lossy().into_owned(),
            running,
            cancelled: cancel.load(Ordering::Relaxed),
            files: ctx.files.load(Ordering::Relaxed),
            bytes: ctx.bytes.load(Ordering::Relaxed),
            current: ctx.current.lock().unwrap().clone(),
            elapsed_ms: started.elapsed().as_millis() as u64,
            denied: ctx.denied.lock().unwrap().len(),
        };

        std::thread::scope(|s| {
            s.spawn(|| {
                while !done.load(Ordering::Relaxed) {
                    let _ = app.emit("disk-scan", progress(true));
                    std::thread::sleep(PROGRESS_EVERY);
                }
            });
            // Отдельный пул побольше: потоки в основном ждут диск, а не считают.
            let pool = rayon::ThreadPoolBuilder::new().num_threads(16).build().unwrap();
            let name: Box<str> = root.to_string_lossy().into();
            let tree = pool.install(|| scan_dir(&root, name, &meta, &ctx));
            let mut scan = SCAN.lock().unwrap();
            // Пока шли, могли запустить новый скан: тогда этот результат не нужен.
            if let Some(scan) = scan.as_mut().filter(|s| Arc::ptr_eq(&s.cancel, &cancel)) {
                if !cancel.load(Ordering::Relaxed) {
                    scan.tree = Some(tree);
                    scan.denied = ctx.denied.lock().unwrap().clone();
                }
            }
            done.store(true, Ordering::Relaxed);
        });
        let _ = app.emit("disk-scan", progress(false));
    });
    Ok(())
}

#[tauri::command]
pub fn disk_scan_cancel() {
    if let Some(scan) = SCAN.lock().unwrap().as_ref() {
        scan.cancel.store(true, Ordering::Relaxed);
    }
}

// ---------- Чтение результатов ----------

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Entry {
    name: String,
    path: String,
    size: u64,
    files: u64,
    newest: i64,
    is_dir: bool,
    denied: bool,
    /// Есть ли смысл заходить внутрь (есть крупные подпапки или файлы).
    has_children: bool,
    protected: Option<String>,
}

fn entry(node: &Node, path: &Path) -> Entry {
    Entry {
        name: node.name.to_string(),
        path: path.to_string_lossy().into_owned(),
        size: node.size,
        files: node.files,
        newest: node.newest,
        is_dir: node.is_dir,
        denied: node.denied,
        has_children: !node.children.is_empty(),
        protected: guard::check(path).err(),
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DirView {
    root: String,
    dir: Entry,
    children: Vec<Entry>,
    /// Свёрнутая мелочь: суммарный размер и сколько файлов и папок.
    rest_size: u64,
    rest_count: u64,
    /// Папки, которые не удалось прочитать во всём скане.
    denied: Vec<String>,
}

/// Находит узел по пути внутри дерева скана.
fn find<'a>(tree: &'a Node, root: &Path, path: &Path) -> Option<&'a Node> {
    let rel = path.strip_prefix(root).ok()?;
    rel.components().try_fold(tree, |node, c| {
        let name = c.as_os_str().to_string_lossy();
        node.children.iter().find(|n| *n.name == *name)
    })
}

fn with_tree<T>(f: impl FnOnce(&Scan, &Node) -> T) -> Result<T, String> {
    let scan = SCAN.lock().unwrap();
    let scan = scan.as_ref().ok_or("Скан ещё не запускался")?;
    let tree = scan.tree.as_ref().ok_or("Скан ещё идёт")?;
    Ok(f(scan, tree))
}

/// Содержимое папки из результатов скана: подпапки и файлы по убыванию размера.
#[tauri::command]
pub fn disk_scan_dir(path: Option<String>) -> Result<DirView, String> {
    with_tree(|scan, tree| {
        let path = path.map(PathBuf::from).unwrap_or_else(|| scan.root.clone());
        let node = find(tree, &scan.root, &path).ok_or("Папки нет в результатах скана")?;
        Ok(DirView {
            root: scan.root.to_string_lossy().into_owned(),
            dir: entry(node, &path),
            children: node.children.iter().take(300).map(|n| entry(n, &path.join(&*n.name))).collect(),
            rest_size: node.rest_size,
            rest_count: node.rest_count,
            denied: scan.denied.clone(),
        })
    })?
}

/// Самые большие файлы во всём скане.
#[tauri::command]
pub fn disk_top_files(limit: usize) -> Result<Vec<Entry>, String> {
    with_tree(|scan, tree| {
        let mut files: Vec<(&Node, PathBuf)> = vec![];
        fn walk<'a>(node: &'a Node, path: PathBuf, out: &mut Vec<(&'a Node, PathBuf)>) {
            for child in &node.children {
                let p = path.join(&*child.name);
                if child.is_dir {
                    walk(child, p, out);
                } else {
                    out.push((child, p));
                }
            }
        }
        walk(tree, scan.root.clone(), &mut files);
        files.sort_unstable_by(|a, b| b.0.size.cmp(&a.0.size));
        files.iter().take(limit).map(|(n, p)| entry(n, p)).collect()
    })
}

/// Крупное, в чём давно ничего не менялось. Берём самый верхний такой
/// уровень: если вся папка старая, внутрь неё уже не смотрим.
#[tauri::command]
pub fn disk_stale(days: i64, min_size: u64) -> Result<Vec<Entry>, String> {
    with_tree(|scan, tree| {
        let cutoff = now() - days * 86_400;
        let mut out = vec![];
        fn walk(node: &Node, path: &Path, cutoff: i64, min: u64, out: &mut Vec<Entry>) {
            for child in node.children.iter().filter(|c| c.size >= min) {
                let p = path.join(&*child.name);
                if child.newest < cutoff && !child.denied && guard::check(&p).is_ok() {
                    out.push(entry(child, &p));
                } else if child.is_dir {
                    walk(child, &p, cutoff, min, out);
                }
            }
        }
        walk(tree, &scan.root, cutoff, min_size, &mut out);
        out.sort_unstable_by(|a, b| b.size.cmp(&a.size));
        out.truncate(200);
        out
    })
}

/// Убирает удалённые пути из дерева и вычитает их размер из всех родителей,
/// чтобы после удаления не пересканировать всю папку.
pub fn forget(paths: &[String]) {
    let mut scan = SCAN.lock().unwrap();
    let Some(scan) = scan.as_mut() else { return };
    let Some(tree) = scan.tree.as_mut() else { return };
    for p in paths {
        let Ok(rel) = Path::new(p).strip_prefix(&scan.root) else { continue };
        let names: Vec<String> = rel.components().map(|c| c.as_os_str().to_string_lossy().into_owned()).collect();
        remove(tree, &names);
    }
}

/// Возвращает (размер, число файлов) удалённого узла.
fn remove(node: &mut Node, names: &[String]) -> Option<(u64, u64)> {
    let (first, rest) = names.split_first()?;
    let i = node.children.iter().position(|n| *n.name == **first)?;
    let removed = if rest.is_empty() {
        let n = node.children.remove(i);
        (n.size, n.files)
    } else {
        remove(&mut node.children[i], rest)?
    };
    node.size = node.size.saturating_sub(removed.0);
    node.files = node.files.saturating_sub(removed.1);
    Some(removed)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx(dev: u64) -> Ctx {
        Ctx {
            dev,
            cancel: Arc::new(AtomicBool::new(false)),
            files: AtomicU64::new(0),
            bytes: AtomicU64::new(0),
            denied: Mutex::new(vec![]),
            current: Mutex::new(String::new()),
            hardlinks: Mutex::new(HashSet::new()),
        }
    }

    #[test]
    fn scans_tree_and_forgets() {
        let root = std::env::temp_dir().join(format!("sm-scan-{}", std::process::id()));
        let big = root.join("a/b");
        std::fs::create_dir_all(&big).unwrap();
        std::fs::write(big.join("big.bin"), vec![1u8; 3 * 1024 * 1024]).unwrap();
        std::fs::write(root.join("small.txt"), "hi").unwrap();
        std::fs::write(root.join("c.bin"), vec![1u8; 2 * 1024 * 1024]).unwrap();
        std::fs::create_dir(root.join("d")).unwrap();
        std::fs::hard_link(root.join("c.bin"), root.join("d/same.bin")).unwrap();

        let meta = std::fs::metadata(&root).unwrap();
        let c = ctx(meta.dev());
        let mut tree = scan_dir(&root, "root".into(), &meta, &c);
        assert_eq!(tree.files, 4);
        // Жёсткая ссылка не удваивает размер.
        assert!(tree.size >= 5 * 1024 * 1024 && tree.size < 6 * 1024 * 1024, "{}", tree.size);
        let a = find(&tree, &root, &root.join("a/b/big.bin")).unwrap();
        assert!(!a.is_dir);

        let before = tree.size;
        let removed = remove(&mut tree, &["a".into(), "b".into(), "big.bin".into()]).unwrap();
        assert_eq!(tree.size, before - removed.0);
        assert!(find(&tree, &root, &root.join("a/b/big.bin")).is_none());
        std::fs::remove_dir_all(&root).unwrap();
    }
}
