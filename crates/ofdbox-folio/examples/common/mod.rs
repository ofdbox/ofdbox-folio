//! 示例共用的系统字体提供者。
//!
//! 只做「家族名 → 系统字体文件」的最小映射，方便示例在自己的机器上跑通；
//! 库本身不扫描系统字体。

use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;
use std::sync::Arc;

use ofdbox_folio::ir::{FontError, FontProvider, FontRequest};

/// 按字体名从系统字体目录取字体。
pub struct SystemFonts {
    cache: HashMap<String, Option<Arc<[u8]>>>,
}

impl SystemFonts {
    /// 创建空的字体缓存。
    pub fn new() -> Self {
        Self {
            cache: HashMap::new(),
        }
    }

    fn roots() -> Vec<PathBuf> {
        vec![
            PathBuf::from("C:/Windows/Fonts"),
            PathBuf::from("/usr/share/fonts"),
            PathBuf::from("/System/Library/Fonts"),
        ]
    }

    fn load_named(&mut self, family: &str) -> Option<Arc<[u8]>> {
        if let Some(cached) = self.cache.get(family) {
            return cached.clone();
        }
        let name = family.to_ascii_lowercase();
        let mut candidates: Vec<&str> = Vec::new();
        if name.contains("sun")
            || name.contains("song")
            || name.contains("ming")
            || name.contains("宋")
            || name.contains("明")
            || name.contains("serif")
        {
            candidates.extend(["simsun.ttc", "times.ttf", "DejaVuSerif.ttf"]);
        } else if name.contains("kai") || name.contains("楷") {
            candidates.extend(["simkai.ttf", "simsun.ttc", "DejaVuSerif.ttf"]);
        } else if name.contains("hei") || name.contains("黑") || name.contains("yahei") {
            candidates.extend(["simhei.ttf", "msyh.ttc", "simsun.ttc", "DejaVuSans.ttf"]);
        } else {
            // 示例正文是中文，优先用带 CJK 字形的字体。
            candidates.extend([
                "simhei.ttf",
                "simsun.ttc",
                "msyh.ttc",
                "arial.ttf",
                "DejaVuSans.ttf",
            ]);
        }
        let mut found = None;
        for root in Self::roots() {
            for candidate in &candidates {
                let path = root.join(candidate);
                if path.is_file() {
                    found = fs::read(&path).ok().map(Arc::<[u8]>::from);
                    if found.is_some() {
                        break;
                    }
                }
            }
            if found.is_some() {
                break;
            }
        }
        self.cache.insert(family.to_owned(), found.clone());
        found
    }
}

impl Default for SystemFonts {
    fn default() -> Self {
        Self::new()
    }
}

impl FontProvider for SystemFonts {
    fn load(&mut self, request: &FontRequest<'_>) -> Result<Option<Arc<[u8]>>, FontError> {
        let family = request
            .family_name
            .or(request.font_name)
            .unwrap_or("Arial")
            .to_owned();
        Ok(self.load_named(&family))
    }
}
