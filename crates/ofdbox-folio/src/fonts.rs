//! System font resolution: maps "font family name + bold/italic" to a local font file.
//!
//! Line breaking, page count and glyphs during layout all depend on the font that is
//! actually resolved, so this mapping must have exactly one implementation and produce
//! explainable results: resolved family names and the actual files are both recorded in
//! `SystemFonts::used_fonts` for `--report` output.
//!
//! Only the minimal "family name → file" mapping is done; font internals are not parsed.
//! When nothing matches, resolution falls back through generic fonts that carry CJK
//! glyphs, so Chinese text never ends up entirely on a Latin-only font.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use ofdbox_folio_ir::{FontError, FontProvider, FontRequest};

/// One successful resolution: family name and actual font file.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResolvedFont {
    /// Requested font family name (may be empty).
    pub family: String,
    /// Font file that was actually hit.
    pub path: PathBuf,
}

/// A [`FontProvider`] that picks fonts by family name from the local font directories.
#[derive(Debug, Default)]
pub struct SystemFonts {
    dirs: Vec<PathBuf>,
    /// `family|bold|italic` → file; `None` means resolution failed (cached as well, to
    /// avoid rescanning the directories over and over).
    resolved: Mutex<HashMap<String, Option<PathBuf>>>,
    /// File → bytes.
    bytes: Mutex<HashMap<PathBuf, Option<Arc<[u8]>>>>,
    /// Family name → actual file, for reporting.
    used: Mutex<HashMap<String, PathBuf>>,
}

impl SystemFonts {
    /// Uses the default font directories of the current platform.
    pub fn new() -> Self {
        Self {
            dirs: default_font_dirs(),
            resolved: Mutex::new(HashMap::new()),
            bytes: Mutex::new(HashMap::new()),
            used: Mutex::new(HashMap::new()),
        }
    }

    /// Font directories to use (searched in order; for tests or custom font bundles).
    pub fn with_dirs(dirs: Vec<PathBuf>) -> Self {
        Self {
            dirs,
            ..Self::new()
        }
    }

    /// Fonts resolved so far, sorted by family name.
    pub fn used_fonts(&self) -> Vec<ResolvedFont> {
        let used = self.used.lock().expect("font usage table");
        let mut items: Vec<ResolvedFont> = used
            .iter()
            .map(|(family, path)| ResolvedFont {
                family: family.clone(),
                path: path.clone(),
            })
            .collect();
        items.sort_by(|left, right| left.family.cmp(&right.family));
        items
    }

    /// Resolves a family name to a font file; returns `None` when nothing is found.
    pub fn resolve(&self, family: &str) -> Option<PathBuf> {
        resolve_in(&self.dirs, family, false, false, &self.resolved)
    }

    fn load_bytes(&self, path: &Path) -> Option<Arc<[u8]>> {
        let mut bytes = self.bytes.lock().expect("font byte cache");
        if let Some(cached) = bytes.get(path) {
            return cached.clone();
        }
        let loaded = std::fs::read(path).ok().map(Arc::<[u8]>::from);
        bytes.insert(path.to_path_buf(), loaded.clone());
        loaded
    }
}

impl FontProvider for SystemFonts {
    fn load(&mut self, request: &FontRequest<'_>) -> Result<Option<Arc<[u8]>>, FontError> {
        let family = request
            .family_name
            .or(request.font_name)
            .unwrap_or_default()
            .to_owned();
        let Some(path) = resolve_in(
            &self.dirs,
            &family,
            request.bold,
            request.italic,
            &self.resolved,
        ) else {
            return Ok(None);
        };
        let Some(bytes) = self.load_bytes(&path) else {
            return Ok(None);
        };
        self.used
            .lock()
            .expect("font usage table")
            .entry(family)
            .or_insert(path);
        Ok(Some(bytes))
    }
}

/// Default font directories of the current platform.
pub fn default_font_dirs() -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    if cfg!(windows) {
        dirs.push(PathBuf::from("C:/Windows/Fonts"));
        if let Ok(local) = std::env::var("LOCALAPPDATA") {
            dirs.push(PathBuf::from(local).join("Microsoft/Windows/Fonts"));
        }
    }
    dirs.extend([
        PathBuf::from("/usr/share/fonts"),
        PathBuf::from("/usr/local/share/fonts"),
        PathBuf::from("/System/Library/Fonts"),
        PathBuf::from("/Library/Fonts"),
        PathBuf::from(std::env::var("HOME").unwrap_or_default()).join(".fonts"),
    ]);
    dirs.retain(|dir| !dir.as_os_str().is_empty());
    dirs
}

/// Candidate file names for a family name after bold/italic adjustment, in priority order.
///
/// A pure function so it is easy to unit test: it never touches the filesystem.
pub fn font_candidates(family: &str, bold: bool, italic: bool) -> Vec<String> {
    let name = family.to_ascii_lowercase();
    let mut candidates: Vec<String> = Vec::new();
    let mut push = |items: &[&str]| {
        for item in items {
            let item = item.to_string();
            if !candidates.contains(&item) {
                candidates.push(item);
            }
        }
    };

    // Common CJK family names (including Word's Chinese and English aliases).
    let cjk_serif = name.contains("sun")
        || name.contains("song")
        || name.contains("ming")
        || name.contains("宋")
        || name.contains("明")
        || name.contains("serif");
    let cjk_kai = name.contains("kai") || name.contains("楷");
    let cjk_hei = name.contains("hei")
        || name.contains("黑")
        || name.contains("yahei")
        || name.contains("雅黑");
    if cjk_serif {
        push(&["simsun.ttc", "simsun.ttf", "times.ttf", "DejaVuSerif.ttf"]);
    } else if cjk_kai {
        push(&["simkai.ttf", "simsun.ttc", "DejaVuSerif.ttf"]);
    } else if cjk_hei {
        push(&["simhei.ttf", "msyh.ttc", "simsun.ttc"]);
    } else if name.contains("times") {
        push(if bold {
            &["timesbd.ttf", "times.ttf", "DejaVuSerif-Bold.ttf"]
        } else {
            &["times.ttf", "timesbd.ttf", "DejaVuSerif.ttf"]
        });
    } else if name.contains("arial") || name.contains("helvetica") {
        push(if bold {
            &["arialbd.ttf", "arial.ttf", "DejaVuSans-Bold.ttf"]
        } else {
            &["arial.ttf", "arialbd.ttf", "DejaVuSans.ttf"]
        });
    } else if name.contains("courier") || name.contains("mono") || name.contains("consol") {
        push(&["cour.ttf", "consola.ttf", "DejaVuSansMono.ttf"]);
    } else if name.contains("tahoma") {
        push(&["tahoma.ttf", "tahomabd.ttf"]);
    } else if name.contains("calibri") || name.contains("cambria") {
        push(&["calibri.ttf", "cambria.ttc", "arial.ttf"]);
    } else if name.contains("symbol") || name.contains("wingding") {
        push(&["wingding.ttf", "DejaVuSans.ttf"]);
    }
    if italic && !cjk_serif && !cjk_kai && !cjk_hei {
        // CJK fonts generally have no separate italic file, so insert these only once
        // among the Latin families.
        push(&["ariali.ttf", "timesi.ttf"]);
    }

    // Generic fallback: even when the family name is completely unknown, prefer fonts that
    // carry CJK glyphs so Chinese text does not turn into whole blocks of tofu.
    push(&[
        "simhei.ttf",
        "simsun.ttc",
        "msyh.ttc",
        "HarmonyOS_Sans_SC_Regular.ttf",
        "NotoSansCJK-Regular.ttc",
        "NotoSansSC-Regular.otf",
        "arial.ttf",
        "DejaVuSans.ttf",
        "LiberationSans-Regular.ttf",
    ]);
    candidates
}

/// Looks files up in the given directories in candidate order; the result is written into `cache`.
fn resolve_in(
    dirs: &[PathBuf],
    family: &str,
    bold: bool,
    italic: bool,
    cache: &Mutex<HashMap<String, Option<PathBuf>>>,
) -> Option<PathBuf> {
    let key = format!("{family}|{bold}|{italic}");
    if let Some(cached) = cache.lock().expect("font resolution cache").get(&key) {
        return cached.clone();
    }
    let found = find_font(dirs, &font_candidates(family, bold, italic));
    cache
        .lock()
        .expect("font resolution cache")
        .insert(key, found.clone());
    found
}

/// Looks up by candidate file name, finally degrading to "the first font in the directory
/// sorted by name".
fn find_font(dirs: &[PathBuf], candidates: &[String]) -> Option<PathBuf> {
    for dir in dirs {
        for candidate in candidates {
            let path = dir.join(candidate);
            if path.is_file() {
                return Some(path);
            }
        }
    }
    // Fallback: scan the directory and take the first font sorted by file name, so the
    // result stays stable and reproducible.
    for dir in dirs {
        let Ok(entries) = std::fs::read_dir(dir) else {
            continue;
        };
        let mut names: Vec<PathBuf> = entries
            .flatten()
            .map(|entry| entry.path())
            .filter(|path| {
                path.is_file()
                    && path
                        .extension()
                        .and_then(|extension| extension.to_str())
                        .is_some_and(|extension| {
                            extension.eq_ignore_ascii_case("ttf")
                                || extension.eq_ignore_ascii_case("ttc")
                                || extension.eq_ignore_ascii_case("otf")
                        })
            })
            .collect();
        names.sort();
        if let Some(path) = names.into_iter().next() {
            return Some(path);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cjk_family_names_prefer_cjk_fonts() {
        let candidates = font_candidates("宋体", false, false);
        assert_eq!(candidates[0], "simsun.ttc");
        let candidates = font_candidates("SimSun", false, false);
        assert_eq!(candidates[0], "simsun.ttc");
        let candidates = font_candidates("黑体", false, false);
        assert_eq!(candidates[0], "simhei.ttf");
        let candidates = font_candidates("楷体", false, false);
        assert_eq!(candidates[0], "simkai.ttf");
    }

    #[test]
    fn latin_families_respect_bold() {
        let regular = font_candidates("Arial", false, false);
        assert_eq!(regular[0], "arial.ttf");
        let bold = font_candidates("Arial", true, false);
        assert_eq!(bold[0], "arialbd.ttf");
        let italic = font_candidates("Arial", false, true);
        assert!(italic.iter().any(|name| name == "ariali.ttf"));
    }

    #[test]
    fn unknown_family_falls_back_to_cjk_capable_fonts() {
        let candidates = font_candidates("", false, false);
        assert_eq!(candidates[0], "simhei.ttf");
        assert!(candidates.contains(&"arial.ttf".to_owned()));
        // Unknown Latin family names also rank after the CJK fallback.
        let candidates = font_candidates("SomeUnknownFace", false, false);
        assert_eq!(candidates[0], "simhei.ttf");
    }

    #[test]
    fn resolves_in_custom_directory_and_records_usage() {
        let dir = std::env::temp_dir().join(format!("ofdbox-fonts-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("create directory");
        std::fs::write(dir.join("simhei.ttf"), b"fake").expect("write file");
        std::fs::write(dir.join("arial.ttf"), b"fake").expect("write file");

        let mut fonts = SystemFonts::with_dirs(vec![dir.clone()]);
        let bytes = fonts
            .load(&FontRequest {
                family_name: Some("黑体"),
                ..FontRequest::default()
            })
            .expect("load")
            .expect("hit");
        assert_eq!(&bytes[..], b"fake");

        let used = fonts.used_fonts();
        assert_eq!(used.len(), 1);
        assert_eq!(used[0].family, "黑体");
        assert_eq!(used[0].path, dir.join("simhei.ttf"));

        // An unknown family name falls back to the CJK-capable candidates (the directory
        // contains simhei.ttf).
        let path = fonts.resolve("NoSuchFontFaceExists");
        assert_eq!(path, Some(dir.join("simhei.ttf")));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn unknown_names_fall_back_to_sorted_directory_scan() {
        let dir = std::env::temp_dir().join(format!("ofdbox-fonts-scan-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("create directory");
        // None of the candidates exist, so the first font sorted by file name is taken to
        // keep the result reproducible.
        std::fs::write(dir.join("zzz-face.ttf"), b"fake").expect("write file");
        std::fs::write(dir.join("aaa-face.ttf"), b"fake").expect("write file");

        let fonts = SystemFonts::with_dirs(vec![dir.clone()]);
        assert_eq!(fonts.resolve("Arial"), Some(dir.join("aaa-face.ttf")));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn missing_directory_resolves_to_nothing() {
        let fonts = SystemFonts::with_dirs(vec![std::env::temp_dir().join("ofdbox-no-such-dir")]);
        assert_eq!(fonts.resolve("Arial"), None);
    }
}
