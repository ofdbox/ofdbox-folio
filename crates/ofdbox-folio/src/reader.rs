//! 阅读器门面：格式探测、页面缓存与按需加载。

use std::collections::{HashMap, VecDeque};
use std::io::Cursor;
use std::path::Path;
use std::sync::Arc;

use ofdbox_folio_ir::{
    AttachmentId, AttachmentInfo, DocumentError, DocumentInfo, DocumentSource, FontProvider,
    Metadata, OutlineItem, Page, PageInfo, SourceFormat,
};

use crate::error::Error;

/// 页面缓存选项。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CacheOptions {
    /// 页面结构缓存预算，单位字节；0 表示禁用。
    pub max_bytes: usize,
}

impl Default for CacheOptions {
    fn default() -> Self {
        Self {
            max_bytes: 64 * 1024 * 1024,
        }
    }
}

/// 阅读器选项。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ReaderOptions {
    /// 页面缓存。
    pub cache: CacheOptions,
}

/// 打开选项。
#[derive(Default)]
pub struct OpenOptions {
    /// 显式指定格式；`None` 时自动探测。
    pub format: Option<SourceFormat>,
    /// 页面缓存。
    pub cache: CacheOptions,
    /// 缺失字体的来源。
    pub font_provider: Option<Box<dyn FontProvider>>,
}

/// 缓存统计。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CacheStats {
    /// 缓存预算。
    pub max_bytes: usize,
    /// 当前缓存页数。
    pub pages: usize,
    /// 当前缓存字节数。
    pub bytes: usize,
    /// 命中次数。
    pub hits: u64,
    /// 未命中次数。
    pub misses: u64,
    /// 淘汰次数。
    pub evictions: u64,
}

struct CachedPage {
    page: Arc<Page>,
    cost: usize,
}

struct PageCache {
    max_bytes: usize,
    bytes: usize,
    entries: HashMap<usize, CachedPage>,
    order: VecDeque<usize>,
    hits: u64,
    misses: u64,
    evictions: u64,
}

impl PageCache {
    fn new(max_bytes: usize) -> Self {
        Self {
            max_bytes,
            bytes: 0,
            entries: HashMap::new(),
            order: VecDeque::new(),
            hits: 0,
            misses: 0,
            evictions: 0,
        }
    }

    fn get(&mut self, index: usize) -> Option<Arc<Page>> {
        let Some(entry) = self.entries.get(&index) else {
            self.misses += 1;
            return None;
        };
        let page = Arc::clone(&entry.page);
        self.hits += 1;
        self.touch(index);
        Some(page)
    }

    fn contains(&self, index: usize) -> bool {
        self.entries.contains_key(&index)
    }

    fn insert(&mut self, index: usize, page: Arc<Page>) {
        let cost = page.estimated_bytes().max(1);
        if self.max_bytes == 0 || cost > self.max_bytes {
            return;
        }
        if let Some(previous) = self.entries.remove(&index) {
            self.bytes = self.bytes.saturating_sub(previous.cost);
            self.order.retain(|value| *value != index);
        }
        while self.bytes.saturating_add(cost) > self.max_bytes {
            let Some(front) = self.order.pop_front() else {
                break;
            };
            if let Some(entry) = self.entries.remove(&front) {
                self.bytes = self.bytes.saturating_sub(entry.cost);
                self.evictions += 1;
            }
        }
        self.bytes += cost;
        self.entries.insert(index, CachedPage { page, cost });
        self.order.push_back(index);
    }

    fn touch(&mut self, index: usize) {
        self.order.retain(|value| *value != index);
        self.order.push_back(index);
    }

    fn clear(&mut self) {
        self.entries.clear();
        self.order.clear();
        self.bytes = 0;
        self.hits = 0;
        self.misses = 0;
        self.evictions = 0;
    }

    fn stats(&self) -> CacheStats {
        CacheStats {
            max_bytes: self.max_bytes,
            pages: self.entries.len(),
            bytes: self.bytes,
            hits: self.hits,
            misses: self.misses,
            evictions: self.evictions,
        }
    }
}

/// 多格式文档阅读器。
pub struct Document {
    source: Box<dyn DocumentSource>,
    cache: PageCache,
}

impl Document {
    /// 从已有文档源创建阅读器。
    pub fn from_source(source: Box<dyn DocumentSource>, options: ReaderOptions) -> Self {
        Self {
            source,
            cache: PageCache::new(options.cache.max_bytes),
        }
    }

    /// 从内存字节打开文档。
    pub fn open_bytes(bytes: Vec<u8>, options: OpenOptions) -> Result<Self, Error> {
        let OpenOptions {
            format,
            cache,
            font_provider,
        } = options;
        let format = match format {
            Some(format) => format,
            None => detect_format(&bytes)?,
        };
        let source: Box<dyn DocumentSource> = match format {
            #[cfg(feature = "ofd")]
            SourceFormat::Ofd => Box::new(ofdbox_folio_ofd::OfdDocument::open_bytes_with_fonts(
                bytes,
                ofdbox_folio_ofd::OfdReadOptions::default(),
                font_provider,
            )?),
            SourceFormat::Pdf => {
                return Err(Error::UnsupportedFormat("PDF 适配尚未实现".to_owned()));
            }
            SourceFormat::Docx => {
                return Err(Error::UnsupportedFormat("DOCX 适配尚未实现".to_owned()));
            }
            _ => {
                return Err(Error::UnsupportedFormat(format!("{format:?}")));
            }
        };
        Ok(Self {
            source,
            cache: PageCache::new(cache.max_bytes),
        })
    }

    /// 从文件打开文档。
    #[cfg(not(target_arch = "wasm32"))]
    pub fn open_path(path: impl AsRef<Path>, options: OpenOptions) -> Result<Self, Error> {
        let bytes = std::fs::read(path)?;
        Self::open_bytes(bytes, options)
    }

    /// 文档级信息。
    pub fn info(&self) -> &DocumentInfo {
        self.source.info()
    }

    /// 源格式。
    pub fn format(&self) -> SourceFormat {
        self.info().format
    }

    /// 元数据。
    pub fn metadata(&self) -> &Metadata {
        &self.info().metadata
    }

    /// 页面数量。
    pub fn page_count(&self) -> usize {
        self.info().page_count
    }

    /// 大纲。
    pub fn outline(&self) -> &[OutlineItem] {
        &self.info().outline
    }

    /// 附件元数据。
    pub fn attachments(&self) -> &[AttachmentInfo] {
        &self.info().attachments
    }

    /// 页面信息，不加载页面内容。
    pub fn page_info(&self, index: usize) -> Result<PageInfo, Error> {
        Ok(self.source.page_info(index)?)
    }

    /// 按需加载页面；命中的页面直接来自缓存。
    pub fn page(&mut self, index: usize) -> Result<Arc<Page>, Error> {
        if let Some(page) = self.cache.get(index) {
            return Ok(page);
        }
        let page = Arc::new(self.source.load_page(index)?);
        self.cache.insert(index, Arc::clone(&page));
        Ok(page)
    }

    /// 预取页面到缓存；已缓存时不做任何事。
    pub fn prefetch(&mut self, index: usize) -> Result<(), Error> {
        if self.cache.max_bytes == 0 || self.cache.contains(index) {
            return Ok(());
        }
        let page = Arc::new(self.source.load_page(index)?);
        self.cache.insert(index, page);
        Ok(())
    }

    /// 预取指定页前后若干页。
    pub fn prefetch_around(&mut self, index: usize, radius: usize) -> Result<(), Error> {
        let page_count = self.page_count();
        if page_count == 0 {
            return Ok(());
        }
        let start = index.saturating_sub(radius);
        let end = index.saturating_add(radius).min(page_count - 1);
        for page in start..=end {
            self.prefetch(page)?;
        }
        Ok(())
    }

    /// 读取附件字节。
    pub fn attachment_bytes(&mut self, id: AttachmentId) -> Result<Arc<[u8]>, Error> {
        Ok(self.source.load_attachment(id)?)
    }

    /// 当前缓存统计。
    pub fn cache_stats(&self) -> CacheStats {
        self.cache.stats()
    }

    /// 清空页面缓存。
    pub fn clear_cache(&mut self) {
        self.cache.clear();
    }
}

fn detect_format(bytes: &[u8]) -> Result<SourceFormat, Error> {
    if bytes.starts_with(b"%PDF") {
        return Ok(SourceFormat::Pdf);
    }
    if bytes.starts_with(b"PK\x03\x04") {
        let archive = zip::ZipArchive::new(Cursor::new(bytes))
            .map_err(|error| DocumentError::Corrupt(error.to_string()))?;
        let mut archive = archive;
        if archive.by_name("OFD.xml").is_ok() {
            return Ok(SourceFormat::Ofd);
        }
        if archive.by_name("word/document.xml").is_ok() {
            return Ok(SourceFormat::Docx);
        }
        return Err(DocumentError::UnsupportedFormat.into());
    }
    Err(DocumentError::UnsupportedFormat.into())
}

#[cfg(test)]
mod tests {
    use super::{CacheOptions, Document, OpenOptions, ReaderOptions};
    use ofdbox_folio_ir::{
        AttachmentId, DocumentInfo, DocumentSource, Page, PageError, PageInfo, Rect, Scene, Size,
    };
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct MockSource {
        info: DocumentInfo,
        pages: Vec<Page>,
        loads: Arc<AtomicUsize>,
    }

    impl MockSource {
        fn new(count: usize) -> (Self, Arc<AtomicUsize>) {
            let loads = Arc::new(AtomicUsize::new(0));
            let pages = (0..count).map(|_| sample_page()).collect();
            let info = DocumentInfo {
                page_count: count,
                ..DocumentInfo::default()
            };
            (
                Self {
                    info,
                    pages,
                    loads: Arc::clone(&loads),
                },
                loads,
            )
        }
    }

    impl DocumentSource for MockSource {
        fn info(&self) -> &DocumentInfo {
            &self.info
        }

        fn page_info(&self, index: usize) -> Result<PageInfo, PageError> {
            self.pages
                .get(index)
                .map(|page| page.info.clone())
                .ok_or(PageError::NotFound(index))
        }

        fn load_page(&mut self, index: usize) -> Result<Page, PageError> {
            self.loads.fetch_add(1, Ordering::SeqCst);
            self.pages
                .get(index)
                .cloned()
                .ok_or(PageError::NotFound(index))
        }

        fn load_attachment(&mut self, _id: AttachmentId) -> Result<Arc<[u8]>, PageError> {
            Ok(Arc::from(&b""[..]))
        }
    }

    fn sample_page() -> Page {
        Page::new(
            PageInfo {
                media_box: Rect::new(0.0, 0.0, 100.0, 100.0),
                crop_box: Rect::new(0.0, 0.0, 100.0, 100.0),
                rotation: ofdbox_folio_ir::Rotation::None,
                label: None,
            },
            Scene {
                size: Size::new(100.0, 100.0),
                ..Scene::default()
            },
        )
    }

    fn document(count: usize, max_bytes: usize) -> (Document, Arc<AtomicUsize>) {
        let (source, loads) = MockSource::new(count);
        (
            Document::from_source(
                Box::new(source),
                ReaderOptions {
                    cache: CacheOptions { max_bytes },
                },
            ),
            loads,
        )
    }

    #[test]
    fn caches_pages_by_index() {
        let (mut document, loads) = document(2, 1024 * 1024);
        let first = document.page(0).expect("first");
        let second = document.page(0).expect("second");
        assert!(Arc::ptr_eq(&first, &second));
        assert_eq!(loads.load(Ordering::SeqCst), 1);
        let stats = document.cache_stats();
        assert_eq!(stats.pages, 1);
        assert_eq!(stats.hits, 1);
        assert_eq!(stats.misses, 1);
        assert!(stats.bytes > 0);
    }

    #[test]
    fn evicts_pages_over_budget() {
        let cost = sample_page().estimated_bytes().max(1);
        let (mut document, loads) = document(2, cost);
        document.page(0).expect("page 0");
        document.page(1).expect("page 1");
        assert_eq!(document.cache_stats().evictions, 1);
        document.page(0).expect("page 0 again");
        assert_eq!(loads.load(Ordering::SeqCst), 3);
    }

    #[test]
    fn zero_budget_disables_cache() {
        let (mut document, loads) = document(1, 0);
        document.page(0).expect("first");
        document.page(0).expect("second");
        assert_eq!(loads.load(Ordering::SeqCst), 2);
        assert_eq!(document.cache_stats().pages, 0);
    }

    #[test]
    fn prefetch_avoids_reload() {
        let (mut document, loads) = document(2, 1024 * 1024);
        document.prefetch(1).expect("prefetch");
        assert_eq!(loads.load(Ordering::SeqCst), 1);
        document.page(1).expect("page");
        assert_eq!(loads.load(Ordering::SeqCst), 1);
        assert_eq!(document.cache_stats().hits, 1);
    }

    #[test]
    fn clear_cache_forces_reload() {
        let (mut document, loads) = document(1, 1024 * 1024);
        document.page(0).expect("first");
        document.clear_cache();
        assert_eq!(document.cache_stats().pages, 0);
        document.page(0).expect("second");
        assert_eq!(loads.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn rejects_unknown_format() {
        let error = match Document::open_bytes(vec![0, 1, 2, 3], OpenOptions::default()) {
            Ok(_) => panic!("unknown format should fail"),
            Err(error) => error,
        };
        assert!(matches!(error, super::Error::Document(_)));
    }
}
