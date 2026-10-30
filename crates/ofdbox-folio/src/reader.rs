//! Reader facade: format probing, page caching and on-demand loading.

use std::collections::{HashMap, VecDeque};
use std::io::Cursor;
#[cfg(not(target_arch = "wasm32"))]
use std::path::Path;
use std::sync::Arc;

use ofdbox_folio_ir::{
    AttachmentId, AttachmentInfo, DocumentError, DocumentInfo, DocumentSource, FixedDocument,
    FixedDocumentSource, FlowDocument, FontError, FontProvider, FontRequest, Metadata, OutlineItem,
    Page, PageInfo, Point, SearchHit, SearchQuery, Selection, SourceFormat, TextPosition,
    TextRange,
};

use crate::error::Error;

/// Page cache options.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CacheOptions {
    /// Budget for cached page structures, in bytes; 0 disables the cache.
    pub max_bytes: usize,
}

impl Default for CacheOptions {
    fn default() -> Self {
        Self {
            max_bytes: 64 * 1024 * 1024,
        }
    }
}

/// Reader options.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ReaderOptions {
    /// Page cache.
    pub cache: CacheOptions,
}

/// Open options.
#[derive(Default)]
pub struct OpenOptions {
    /// Explicit format; auto-detected when `None`.
    pub format: Option<SourceFormat>,
    /// Page cache.
    pub cache: CacheOptions,
    /// Source for missing fonts.
    pub font_provider: Option<Box<dyn FontProvider>>,
    /// Password of an encrypted document.
    pub password: Option<String>,
}

/// Cache statistics.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CacheStats {
    /// Cache budget.
    pub max_bytes: usize,
    /// Number of pages currently cached.
    pub pages: usize,
    /// Number of bytes currently cached.
    pub bytes: usize,
    /// Number of hits.
    pub hits: u64,
    /// Number of misses.
    pub misses: u64,
    /// Number of evictions.
    pub evictions: u64,
}

/// Selection rectangles on one page.
#[derive(Clone, Debug, PartialEq)]
pub struct PageSelection {
    /// Page index.
    pub page: usize,
    /// Rectangles in page space.
    pub rects: Vec<ofdbox_folio_ir::Rect>,
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

/// Multi-format document reader.
pub struct Document {
    source: Box<dyn DocumentSource>,
    cache: PageCache,
}

impl Document {
    /// Creates a reader from an existing document source.
    pub fn from_source(source: Box<dyn DocumentSource>, options: ReaderOptions) -> Self {
        Self {
            source,
            cache: PageCache::new(options.cache.max_bytes),
        }
    }

    /// Creates a reader over an already laid-out fixed document.
    ///
    /// This lets output produced by the layout engine (for example from DOCX) use
    /// the same page cache, selection, search and hit-testing APIs as native
    /// OFD/PDF readers.
    pub fn from_fixed(
        document: FixedDocument,
        format: SourceFormat,
        options: ReaderOptions,
    ) -> Self {
        Self::from_source(
            Box::new(FixedDocumentSource::new(document, format)),
            options,
        )
    }

    /// Lays out Flow IR and wraps the result in a reader.
    #[cfg(feature = "layout")]
    pub fn from_flow(
        flow: &FlowDocument,
        fonts: &mut dyn FontProvider,
        format: SourceFormat,
        options: ReaderOptions,
    ) -> Result<Self, Error> {
        let fixed = ofdbox_folio_layout::layout_with(
            flow,
            fonts,
            ofdbox_folio_layout::LayoutOptions::default(),
        )?;
        Ok(Self::from_fixed(fixed, format, options))
    }

    /// Opens a document from in-memory bytes.
    pub fn open_bytes(bytes: Vec<u8>, options: OpenOptions) -> Result<Self, Error> {
        let OpenOptions {
            format,
            cache,
            font_provider,
            password,
        } = options;
        let format = match format {
            Some(format) => format,
            None => detect_format(&bytes)?,
        };
        #[cfg(not(feature = "pdf"))]
        let _ = &password;
        let source: Box<dyn DocumentSource> = match format {
            #[cfg(feature = "ofd")]
            SourceFormat::Ofd => Box::new(ofdbox_folio_ofd::OfdDocument::open_bytes_with_fonts(
                bytes,
                ofdbox_folio_ofd::OfdReadOptions::default(),
                font_provider,
            )?),
            #[cfg(feature = "pdf")]
            SourceFormat::Pdf => Box::new(ofdbox_folio_pdf::PdfDocument::open_bytes_with_fonts(
                bytes,
                ofdbox_folio_pdf::PdfReadOptions {
                    password,
                    ..ofdbox_folio_pdf::PdfReadOptions::default()
                },
                font_provider,
            )?),
            #[cfg(not(feature = "pdf"))]
            SourceFormat::Pdf => {
                return Err(Error::UnsupportedFormat(
                    "PDF support is not enabled".to_owned(),
                ));
            }
            #[cfg(all(feature = "docx", feature = "layout"))]
            SourceFormat::Docx => {
                let flow = ofdbox_folio_docx::read_flow(
                    &bytes,
                    &ofdbox_folio_docx::DocxReadOptions::default(),
                )?;
                let mut fonts = font_provider.unwrap_or_else(|| Box::new(NoFonts));
                let fixed = ofdbox_folio_layout::layout_with(
                    &flow,
                    fonts.as_mut(),
                    ofdbox_folio_layout::LayoutOptions::default(),
                )?;
                Box::new(FixedDocumentSource::new(fixed, SourceFormat::Docx))
            }
            #[cfg(not(all(feature = "docx", feature = "layout")))]
            SourceFormat::Docx => {
                return Err(Error::UnsupportedFormat(
                    "DOCX support is not enabled".to_owned(),
                ));
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

    /// Opens a document from a file.
    #[cfg(not(target_arch = "wasm32"))]
    pub fn open_path(path: impl AsRef<Path>, options: OpenOptions) -> Result<Self, Error> {
        let bytes = std::fs::read(path)?;
        Self::open_bytes(bytes, options)
    }

    /// Document-level information.
    pub fn info(&self) -> &DocumentInfo {
        self.source.info()
    }

    /// Source format.
    pub fn format(&self) -> SourceFormat {
        self.info().format
    }

    /// Metadata.
    pub fn metadata(&self) -> &Metadata {
        &self.info().metadata
    }

    /// Number of pages.
    pub fn page_count(&self) -> usize {
        self.info().page_count
    }

    /// Outline.
    pub fn outline(&self) -> &[OutlineItem] {
        &self.info().outline
    }

    /// Attachment metadata.
    pub fn attachments(&self) -> &[AttachmentInfo] {
        &self.info().attachments
    }

    /// Page information without loading the page content.
    pub fn page_info(&self, index: usize) -> Result<PageInfo, Error> {
        Ok(self.source.page_info(index)?)
    }

    /// Loads a page on demand; a page that is already cached comes straight from the cache.
    pub fn page(&mut self, index: usize) -> Result<Arc<Page>, Error> {
        if let Some(page) = self.cache.get(index) {
            return Ok(page);
        }
        let page = Arc::new(self.source.load_page(index)?);
        self.cache.insert(index, Arc::clone(&page));
        Ok(page)
    }

    /// Prefetches a page into the cache; does nothing when it is already cached.
    pub fn prefetch(&mut self, index: usize) -> Result<(), Error> {
        if self.cache.max_bytes == 0 || self.cache.contains(index) {
            return Ok(());
        }
        let page = Arc::new(self.source.load_page(index)?);
        self.cache.insert(index, page);
        Ok(())
    }

    /// Returns the plain text of one page.
    pub fn page_text(&mut self, index: usize) -> Result<String, Error> {
        Ok(self.page(index)?.text.plain_text())
    }

    /// Converts a character offset within a page into a text position.
    ///
    /// `char_offset` accumulates over the logical order of the page's text items; when it
    /// is out of range, the position at the end of the page is returned.
    pub fn text_position(&mut self, page: usize, char_offset: u32) -> Result<TextPosition, Error> {
        let page = self.page(page)?;
        let layer = &page.text;
        let mut remaining = char_offset;
        for (index, item) in layer.items.iter().enumerate() {
            let length = item.run.char_count() as u32;
            if remaining <= length {
                return Ok(TextPosition::new(layer.page, index as u32, remaining));
            }
            remaining -= length;
        }
        Ok(layer.end_position())
    }

    /// Searches across pages; stops early when the callback returns `false`.
    ///
    /// Returns the number of hits visited. Pages are loaded on demand and enter the cache.
    pub fn search(
        &mut self,
        query: &SearchQuery,
        mut visit: impl FnMut(&SearchHit) -> bool,
    ) -> Result<usize, Error> {
        let mut count = 0;
        for index in 0..self.page_count() {
            let page = self.page(index)?;
            for hit in ofdbox_folio_ir::search(&page.text, query) {
                count += 1;
                if !visit(&hit) {
                    return Ok(count);
                }
            }
        }
        Ok(count)
    }

    /// Searches across pages and collects all hits.
    pub fn search_all(&mut self, query: &SearchQuery) -> Result<Vec<SearchHit>, Error> {
        let mut hits = Vec::new();
        self.search(query, |hit| {
            hits.push(hit.clone());
            true
        })?;
        Ok(hits)
    }

    /// Returns the text for a cross-page selection; different pages are joined with a
    /// newline.
    pub fn selection_text(&mut self, selection: &Selection) -> Result<String, Error> {
        let range = selection.range();
        if range.is_empty() {
            return Ok(String::new());
        }
        self.validate_pages(range.start.page, range.end.page)?;
        let mut output = String::new();
        for index in range.start.page..=range.end.page {
            let page = self.page(index)?;
            let Some(local) = page_range(&page, &range, index) else {
                continue;
            };
            let text = ofdbox_folio_ir::selection_text(&page.text, &local);
            if text.is_empty() {
                continue;
            }
            if !output.is_empty() {
                output.push('\n');
            }
            output.push_str(&text);
        }
        Ok(output)
    }

    /// Hit testing: which character a point in page coordinates (millimetres, top-left
    /// origin) falls on.
    pub fn hit_test(&mut self, page: usize, point: Point) -> Result<Option<TextPosition>, Error> {
        let page = self.page(page)?;
        Ok(ofdbox_folio_ir::hit_test(&page.text, point))
    }

    /// Returns the page rectangles covered by a cross-page selection.
    pub fn selection_rects(&mut self, selection: &Selection) -> Result<Vec<PageSelection>, Error> {
        let range = selection.range();
        if range.is_empty() {
            return Ok(Vec::new());
        }
        self.validate_pages(range.start.page, range.end.page)?;
        let mut pages = Vec::new();
        for index in range.start.page..=range.end.page {
            let page = self.page(index)?;
            let Some(local) = page_range(&page, &range, index) else {
                continue;
            };
            let rects = ofdbox_folio_ir::selection_rects(&page.text, &local);
            if !rects.is_empty() {
                pages.push(PageSelection { page: index, rects });
            }
        }
        Ok(pages)
    }

    fn validate_pages(&self, start: usize, end: usize) -> Result<(), Error> {
        let count = self.page_count();
        if start >= count || end >= count {
            return Err(ofdbox_folio_ir::PageError::NotFound(start.max(end)).into());
        }
        Ok(())
    }

    /// Prefetches a few pages before and after the given page.
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

    /// Reads the bytes of an attachment.
    pub fn attachment_bytes(&mut self, id: AttachmentId) -> Result<Arc<[u8]>, Error> {
        Ok(self.source.load_attachment(id)?)
    }

    /// Current cache statistics.
    pub fn cache_stats(&self) -> CacheStats {
        self.cache.stats()
    }

    /// Clears the page cache.
    pub fn clear_cache(&mut self) {
        self.cache.clear();
        self.source.clear_caches();
    }

    /// Font names declared by the document.
    pub fn font_names(&mut self) -> Result<Vec<String>, Error> {
        Ok(self.source.font_names()?)
    }
}

fn page_range(page: &Page, range: &TextRange, index: usize) -> Option<TextRange> {
    let layer = &page.text;
    let start = if index == range.start.page {
        TextPosition::new(index, range.start.item, range.start.char_offset)
    } else {
        layer.start_position()
    };
    let end = if index == range.end.page {
        TextPosition::new(index, range.end.item, range.end.char_offset)
    } else {
        layer.end_position()
    };
    (start < end).then(|| TextRange::new(start, end))
}

fn detect_format(bytes: &[u8]) -> Result<SourceFormat, Error> {
    #[cfg(feature = "pdf")]
    if ofdbox_folio_pdf::probe(bytes) {
        return Ok(SourceFormat::Pdf);
    }
    #[cfg(not(feature = "pdf"))]
    if bytes.starts_with(b"%PDF") {
        return Ok(SourceFormat::Pdf);
    }
    #[cfg(feature = "ofd")]
    if ofdbox_folio_ofd::probe(bytes) {
        return Ok(SourceFormat::Ofd);
    }
    if let Ok(mut archive) = zip::ZipArchive::new(Cursor::new(bytes))
        && archive.by_name("word/document.xml").is_ok()
    {
        return Ok(SourceFormat::Docx);
    }
    Err(DocumentError::UnsupportedFormat.into())
}

/// Font provider used when the caller does not supply one.
struct NoFonts;

impl FontProvider for NoFonts {
    fn load(&mut self, _request: &FontRequest<'_>) -> Result<Option<Arc<[u8]>>, FontError> {
        Ok(None)
    }
}

#[cfg(test)]
mod tests {
    use super::{CacheOptions, Document, OpenOptions, ReaderOptions};
    use ofdbox_folio_ir::{
        AttachmentId, Block, Cluster, DocumentInfo, DocumentSource, FixedDocument,
        FixedDocumentSource, FontAsset, Glyph, GlyphRun, Inline, Matrix, Metadata, Page, PageError,
        PageInfo, ParaStyle, Paragraph, Rect, Rotation, Scene, SearchQuery, Selection, Size,
        SourceFormat, TextItem, TextLayer, TextLine, TextPosition, TextRun, TextStyle,
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
            let pages = (0..count).map(sample_page).collect();
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

    fn sample_page(page: usize) -> Page {
        let text = "Hello";
        let glyphs = text
            .chars()
            .enumerate()
            .map(|(index, _)| Glyph {
                id: index as u32,
                x: index as f64 * 10.0,
                y: 10.0,
                advance: 10.0,
            })
            .collect::<Vec<_>>();
        let clusters = text
            .chars()
            .enumerate()
            .map(|(index, _)| Cluster {
                text_start: index as u32,
                text_end: index as u32 + 1,
                glyph_start: index as u32,
                glyph_end: index as u32 + 1,
            })
            .collect::<Vec<_>>();
        let run = Arc::new(GlyphRun::new(text, glyphs, clusters));
        let font = Arc::new(FontAsset::new(Arc::from(&b""[..]), 0));
        let bbox = Rect::new(0.0, 0.0, 50.0, 12.0);
        Page {
            info: PageInfo {
                media_box: Rect::new(0.0, 0.0, 100.0, 100.0),
                crop_box: Rect::new(0.0, 0.0, 100.0, 100.0),
                rotation: ofdbox_folio_ir::Rotation::None,
                label: None,
            },
            content: Scene {
                size: Size::new(100.0, 100.0),
                ..Scene::default()
            },
            text: TextLayer {
                page,
                items: vec![TextItem {
                    run,
                    transform: Matrix::IDENTITY,
                    bbox,
                    font,
                    font_size: 10.0,
                }],
                lines: vec![TextLine {
                    items: 0..1,
                    bbox,
                    baseline: 10.0,
                }],
            },
            templates: Vec::new(),
            links: Vec::new(),
        }
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
        let cost = sample_page(0).estimated_bytes().max(1);
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
    fn searches_across_pages() {
        let (mut document, _) = document(2, 1024 * 1024);
        let query = SearchQuery {
            text: "Hello".to_owned(),
            ..SearchQuery::default()
        };
        let hits = document.search_all(&query).expect("search");
        assert_eq!(hits.len(), 2);
        assert_eq!(hits[0].range.start.page, 0);
        assert_eq!(hits[1].range.start.page, 1);
    }

    #[test]
    fn search_can_stop_early() {
        let (mut document, _) = document(3, 1024 * 1024);
        let query = SearchQuery {
            text: "Hello".to_owned(),
            ..SearchQuery::default()
        };
        let count = document.search(&query, |_| false).expect("search");
        assert_eq!(count, 1);
    }

    #[test]
    fn selection_spans_pages() {
        let (mut document, _) = document(2, 1024 * 1024);
        let selection = Selection {
            anchor: TextPosition::new(0, 0, 2),
            focus: TextPosition::new(1, 0, 5),
        };
        assert_eq!(
            document.selection_text(&selection).expect("text"),
            "llo\nHello"
        );
        let pages = document.selection_rects(&selection).expect("rects");
        assert_eq!(pages.len(), 2);
        assert_eq!(pages[0].page, 0);
        assert_eq!(pages[1].page, 1);
        assert!(!pages[0].rects.is_empty());
    }

    #[test]
    fn page_text_uses_cached_text_layer() {
        let (mut document, _) = document(1, 1024 * 1024);
        assert_eq!(document.page_text(0).expect("text"), "Hello");
    }

    #[test]
    fn rejects_unknown_format() {
        let error = match Document::open_bytes(vec![0, 1, 2, 3], OpenOptions::default()) {
            Ok(_) => panic!("unknown format should fail"),
            Err(error) => error,
        };
        assert!(matches!(error, super::Error::Document(_)));
    }

    #[cfg(feature = "pdf")]
    #[test]
    fn opens_pdf_and_reads_pages() {
        use ofdbox_folio_ir::FixedDocument;
        let mut document = FixedDocument::default();
        document.pages.push(Page::new(
            PageInfo {
                media_box: Rect::new(0.0, 0.0, 100.0, 80.0),
                crop_box: Rect::new(0.0, 0.0, 100.0, 80.0),
                rotation: Rotation::None,
                label: None,
            },
            Scene {
                size: Size::new(100.0, 80.0),
                ..Scene::default()
            },
        ));
        let bytes =
            crate::pdf::write_fixed(&document, &crate::pdf::PdfOptions::default()).expect("pdf");
        let mut reader = Document::open_bytes(bytes, OpenOptions::default()).expect("open");
        assert_eq!(reader.format(), SourceFormat::Pdf);
        assert_eq!(reader.info().page_count, 1);
        let page = reader.page(0).expect("page");
        assert!((page.display_size().width - 100.0).abs() < 0.01);
        assert!((page.display_size().height - 80.0).abs() < 0.01);
    }

    #[test]
    fn fixed_document_source_serves_reader_apis() {
        let mut fixed = FixedDocument::default();
        fixed.pages.push(sample_page(0));
        fixed.pages.push(sample_page(1));
        let source = FixedDocumentSource::new(fixed, SourceFormat::Docx);
        assert_eq!(source.info().page_count, 2);
        let mut document = Document::from_source(
            Box::new(source),
            ReaderOptions {
                cache: CacheOptions {
                    max_bytes: 1024 * 1024,
                },
            },
        );
        assert_eq!(document.format(), SourceFormat::Docx);
        assert_eq!(document.page_count(), 2);
        assert_eq!(document.page_text(0).expect("text"), "Hello");
        let query = SearchQuery {
            text: "Hello".to_owned(),
            ..SearchQuery::default()
        };
        assert_eq!(document.search_all(&query).expect("search").len(), 2);
    }

    #[test]
    fn from_fixed_document_uses_reader_apis() {
        let mut fixed = FixedDocument::default();
        fixed.pages.push(sample_page(3));
        let mut document = Document::from_fixed(
            fixed,
            SourceFormat::Docx,
            ReaderOptions {
                cache: CacheOptions {
                    max_bytes: 1024 * 1024,
                },
            },
        );
        assert_eq!(document.page_count(), 1);
        assert_eq!(document.page_text(0).expect("text"), "Hello");
    }

    #[cfg(all(feature = "docx", feature = "layout"))]
    #[test]
    fn opens_docx_through_reader() {
        let mut flow = ofdbox_folio_ir::FlowDocument::new(Metadata::default());
        let mut paragraph = Paragraph::new(ParaStyle::default());
        paragraph.runs.push(Inline::Text(TextRun::new(
            "Hello DOCX",
            TextStyle::default(),
        )));
        flow.push(Block::Paragraph(paragraph));
        let bytes = crate::docx::write_flow(&flow, &crate::docx::DocxWriteOptions::default())
            .expect("write docx");
        let mut document = Document::open_bytes(bytes, OpenOptions::default()).expect("open docx");
        assert_eq!(document.format(), SourceFormat::Docx);
        assert!(document.page_count() >= 1);
        let page = document.page(0).expect("page");
        assert!(page.display_size().width > 0.0);
        assert!(page.display_size().height > 0.0);
    }

    #[cfg(all(feature = "docx", feature = "layout"))]
    #[test]
    fn from_flow_lays_out_and_reads() {
        let mut flow = ofdbox_folio_ir::FlowDocument::new(Metadata::default());
        let mut paragraph = Paragraph::new(ParaStyle::default());
        paragraph.runs.push(Inline::Text(TextRun::new(
            "Hello Flow",
            TextStyle::default(),
        )));
        flow.push(Block::Paragraph(paragraph));
        let mut fonts = super::NoFonts;
        let document = Document::from_flow(
            &flow,
            &mut fonts,
            SourceFormat::Docx,
            ReaderOptions {
                cache: CacheOptions {
                    max_bytes: 1024 * 1024,
                },
            },
        )
        .expect("layout");
        assert_eq!(document.page_count(), 1);
    }
}
