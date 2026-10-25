//! 系统字体解析：把「字体族名 + 粗体/斜体」映射到本机字体文件。
//!
//! 排版的断行、页数与字形都取决于实际取到的字体，所以这层映射必须只有一份实现、
//! 且结果可解释：解析过的族名与实际文件都记在 [`SystemFonts::used_fonts`] 里，
//! 供 `--report` 输出。
//!
//! 只做「族名 → 文件」的最小映射，不解析字体内部信息；找不到时会依次退到
//! 带 CJK 字形的通用字体，保证中文不会整段落到纯拉丁字体上。

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use ofdbox_folio_ir::{FontError, FontProvider, FontRequest};

/// 一次成功解析：族名与实际字体文件。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResolvedFont {
    /// 请求的字体族名（可能为空）。
    pub family: String,
    /// 实际命中的字体文件。
    pub path: PathBuf,
}

/// 从本机字体目录按族名取字体的 [`FontProvider`]。
#[derive(Debug, Default)]
pub struct SystemFonts {
    dirs: Vec<PathBuf>,
    /// `族名|粗|斜` → 文件；`None` 表示解析失败（同样缓存，避免反复扫目录）。
    resolved: Mutex<HashMap<String, Option<PathBuf>>>,
    /// 文件 → 字节。
    bytes: Mutex<HashMap<PathBuf, Option<Arc<[u8]>>>>,
    /// 族名 → 实际文件，供报告使用。
    used: Mutex<HashMap<String, PathBuf>>,
}

impl SystemFonts {
    /// 使用当前平台的默认字体目录。
    pub fn new() -> Self {
        Self {
            dirs: default_font_dirs(),
            resolved: Mutex::new(HashMap::new()),
            bytes: Mutex::new(HashMap::new()),
            used: Mutex::new(HashMap::new()),
        }
    }

    /// 指定字体目录（按顺序查找，用于测试或自定义字体包）。
    pub fn with_dirs(dirs: Vec<PathBuf>) -> Self {
        Self {
            dirs,
            ..Self::new()
        }
    }

    /// 已经解析过的字体，按族名排序。
    pub fn used_fonts(&self) -> Vec<ResolvedFont> {
        let used = self.used.lock().expect("字体使用表");
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

    /// 解析族名到字体文件；找不到返回 `None`。
    pub fn resolve(&self, family: &str) -> Option<PathBuf> {
        resolve_in(&self.dirs, family, false, false, &self.resolved)
    }

    fn load_bytes(&self, path: &Path) -> Option<Arc<[u8]>> {
        let mut bytes = self.bytes.lock().expect("字体缓存");
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
            .expect("字体使用表")
            .entry(family)
            .or_insert(path);
        Ok(Some(bytes))
    }
}

/// 当前平台的默认字体目录。
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

/// 族名经过粗体/斜体修正后的候选文件名，按优先级排列。
///
/// 纯函数，便于单测：不碰文件系统。
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

    // 中日韩常见族名（含 Word 的中英文别名）。
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
        // 中文字体一般没有独立斜体文件，这里只在拉动词族里插一次。
        push(&["ariali.ttf", "timesi.ttf"]);
    }

    // 通用兜底：即使族名完全未知，也优先带 CJK 字形的字体，避免中文整段豆腐块。
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

/// 在给定目录里按候选顺序找文件；结果写进 `cache`。
fn resolve_in(
    dirs: &[PathBuf],
    family: &str,
    bold: bool,
    italic: bool,
    cache: &Mutex<HashMap<String, Option<PathBuf>>>,
) -> Option<PathBuf> {
    let key = format!("{family}|{bold}|{italic}");
    if let Some(cached) = cache.lock().expect("字体解析缓存").get(&key) {
        return cached.clone();
    }
    let found = find_font(dirs, &font_candidates(family, bold, italic));
    cache
        .lock()
        .expect("字体解析缓存")
        .insert(key, found.clone());
    found
}

/// 按候选文件名查找，最后退化成「目录里按名字排序的第一个字体」。
fn find_font(dirs: &[PathBuf], candidates: &[String]) -> Option<PathBuf> {
    for dir in dirs {
        for candidate in candidates {
            let path = dir.join(candidate);
            if path.is_file() {
                return Some(path);
            }
        }
    }
    // 兜底：扫目录，按文件名排序取第一个字体，保证结果稳定可复现。
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
        // 未知拉丁族名同样排在 CJK 兜底之后。
        let candidates = font_candidates("SomeUnknownFace", false, false);
        assert_eq!(candidates[0], "simhei.ttf");
    }

    #[test]
    fn resolves_in_custom_directory_and_records_usage() {
        let dir = std::env::temp_dir().join(format!("ofdbox-fonts-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("创建目录");
        std::fs::write(dir.join("simhei.ttf"), b"fake").expect("写文件");
        std::fs::write(dir.join("arial.ttf"), b"fake").expect("写文件");

        let mut fonts = SystemFonts::with_dirs(vec![dir.clone()]);
        let bytes = fonts
            .load(&FontRequest {
                family_name: Some("黑体"),
                ..FontRequest::default()
            })
            .expect("加载")
            .expect("命中");
        assert_eq!(&bytes[..], b"fake");

        let used = fonts.used_fonts();
        assert_eq!(used.len(), 1);
        assert_eq!(used[0].family, "黑体");
        assert_eq!(used[0].path, dir.join("simhei.ttf"));

        // 未知族名退到带 CJK 字形的兜底候选（目录里有 simhei.ttf）。
        let path = fonts.resolve("完全不存在的字体");
        assert_eq!(path, Some(dir.join("simhei.ttf")));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn unknown_names_fall_back_to_sorted_directory_scan() {
        let dir = std::env::temp_dir().join(format!("ofdbox-fonts-scan-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("创建目录");
        // 候选表里一个都没有，只能按文件名排序取第一个，保证结果可复现。
        std::fs::write(dir.join("zzz-face.ttf"), b"fake").expect("写文件");
        std::fs::write(dir.join("aaa-face.ttf"), b"fake").expect("写文件");

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
