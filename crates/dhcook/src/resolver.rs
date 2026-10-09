//! Multi-package object resolution (imports -> exports across packages).

use anyhow::{Context, Result};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use upk::texture::TfcCache;
use upk::Package;

/// A resolved object: an export inside a loaded package.
#[derive(Clone)]
pub struct Obj {
    pub pkg: Arc<Package>,
    pub idx: i32,
}

impl Obj {
    pub fn class(&self) -> String {
        self.pkg.class_name(self.idx)
    }
    pub fn path(&self) -> String {
        self.pkg.obj_path(self.idx)
    }
    pub fn name(&self) -> &str {
        self.pkg.obj_name(self.idx)
    }
    pub fn key(&self) -> String {
        self.path().to_ascii_lowercase()
    }
    pub fn props(&self) -> Result<upk::Props> {
        Ok(upk::read_object(&self.pkg, self.idx)?.props)
    }
}

pub struct Assets {
    pub cooked_dir: PathBuf,
    files: HashMap<String, PathBuf>,
    loaded: Mutex<HashMap<String, Option<Arc<Package>>>>,
    /// Packages searched (in order) when resolving imports.
    search: Mutex<Vec<Arc<Package>>>,
    pub tfc: TfcCache,
}

/// Packages that are always resident in the original game and hold shared content.
pub const GLOBAL_PACKAGES: &[&str] = &["Startup", "DishonoredGame", "Engine", "GameFramework", "Core"];

impl Assets {
    pub fn new(cooked_dir: &Path) -> Result<Self> {
        let mut files = HashMap::new();
        // the game's packages, then the DLC's (`DLC\PCConsole\DLCnn`: their own maps, UI and
        // tweaks; where a name is in both, the game's)
        for dir in std::iter::once(cooked_dir.to_path_buf()).chain(dlc_dirs(cooked_dir)) {
            let Ok(list) = std::fs::read_dir(&dir) else { continue };
            for e in list.flatten() {
                let p = e.path();
                if p.extension().map(|x| x.eq_ignore_ascii_case("upk")).unwrap_or(false) {
                    let stem = p.file_stem().unwrap().to_string_lossy().to_ascii_lowercase();
                    files.entry(stem).or_insert(p);
                }
            }
        }
        Ok(Self {
            cooked_dir: cooked_dir.to_path_buf(),
            files,
            loaded: Mutex::new(HashMap::new()),
            search: Mutex::new(Vec::new()),
            tfc: TfcCache::new(cooked_dir),
        })
    }

    /// Where a package's file is.
    pub fn package_path(&self, name: &str) -> Option<&Path> {
        self.files.get(&name.to_ascii_lowercase()).map(|p| p.as_path())
    }

    pub fn has_package(&self, name: &str) -> bool {
        self.files.contains_key(&name.to_ascii_lowercase())
    }

    pub fn package(&self, name: &str) -> Option<Arc<Package>> {
        let key = name.to_ascii_lowercase();
        if let Some(p) = self.loaded.lock().unwrap().get(&key) {
            return p.clone();
        }
        let p = self.files.get(&key).and_then(|path| match Package::open(path) {
            Ok(p) => Some(Arc::new(p)),
            Err(e) => {
                log::warn!("failed to open package {name}: {e:#}");
                None
            }
        });
        self.loaded.lock().unwrap().insert(key, p.clone());
        p
    }

    /// Add a package to the import search set.
    pub fn add_search(&self, pkg: Arc<Package>) {
        let mut s = self.search.lock().unwrap();
        if !s.iter().any(|p| Arc::ptr_eq(p, &pkg)) {
            s.push(pkg);
        }
    }

    /// Build the UE3 class hierarchy from script packages so components are detected exactly.
    pub fn init_class_registry(&self) {
        let pk: Vec<Arc<Package>> = ["Core", "Engine", "GameFramework", "DishonoredGame", "AkAudio", "GFxUI"]
            .iter()
            .filter_map(|n| self.package(n))
            .collect();
        let refs: Vec<&Package> = pk.iter().map(|p| p.as_ref()).collect();
        upk::props::set_component_classes(upk::props::component_classes_from(&refs));
    }

    pub fn load_globals(&self) {
        self.init_class_registry();
        for name in GLOBAL_PACKAGES {
            if let Some(p) = self.package(name) {
                self.add_search(p);
            }
        }
    }

    /// Find an export by full dotted path in the search set.
    pub fn find(&self, path: &str) -> Option<Obj> {
        let search = self.search.lock().unwrap().clone();
        for p in &search {
            if let Some(i) = p.find_export(path) {
                if p.exports[i as usize - 1].size > 0 || p.class_name(i) == "Package" {
                    return Some(Obj { pkg: p.clone(), idx: i });
                }
            }
        }
        // Path rooted at a package file: "Engine.Default__Foo" -> Engine.upk : "Default__Foo"
        let (root, rest) = path.split_once('.')?;
        let p = self.package(root)?;
        let i = p.find_export(rest)?;
        Some(Obj { pkg: p, idx: i })
    }

    /// Resolve an object reference from `pkg` to a concrete export.
    pub fn resolve(&self, pkg: &Arc<Package>, idx: i32) -> Option<Obj> {
        if idx > 0 {
            if idx as usize > pkg.exports.len() {
                return None;
            }
            return Some(Obj { pkg: pkg.clone(), idx });
        }
        if idx == 0 {
            return None;
        }
        let path = pkg.obj_path(idx);
        // Prefer a copy inside the same package (forced exports), then the search set.
        if let Some(i) = pkg.find_export(&path) {
            return Some(Obj { pkg: pkg.clone(), idx: i });
        }
        self.find(&path)
    }
}

/// The installed DLC's package folders (`DishonoredGame\DLC\PCConsole\DLC05`...), beside the
/// game's `CookedPCConsole`.
pub fn dlc_dirs(cooked_dir: &Path) -> Vec<PathBuf> {
    let root = cooked_dir.parent().map(|p| p.join("DLC").join("PCConsole")).unwrap_or_default();
    let mut out: Vec<PathBuf> = std::fs::read_dir(&root)
        .map(|l| l.flatten().map(|e| e.path()).filter(|p| p.is_dir() && p.file_name().is_some_and(|n| n.to_string_lossy().starts_with("DLC"))).collect())
        .unwrap_or_default();
    out.sort();
    out
}
