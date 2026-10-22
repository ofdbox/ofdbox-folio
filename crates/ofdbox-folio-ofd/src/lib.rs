//! OFDBox Folio 的 OFD 适配层。
//!
//! 负责把 OFD 包解析为 Fixed IR 页面，以及把 Fixed IR 写出为 OFD 包。
//! 当前实现提供读取路径；写出与签名能力随后补齐。对外不暴露
//! `ofdbox-core` 的类型。

#![allow(clippy::too_many_lines)]

mod frontend;

use std::cell::RefCell;
use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;

use ofdbox_core::{DocumentView, OfdPackage, PageView};
use ofdbox_folio_ir::{
    AttachmentId, DocumentInfo, DocumentSource, FontProvider, Metadata, Page, PageError, PageInfo,
    Rect, ResourceStore, Rotation, SourceFormat,
};

pub use frontend::{OfdReadOptions, UnsupportedPolicy};

/// OFD 适配层错误。
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum OfdError {
    /// OFD 结构读取或解析失败。
    #[error(transparent)]
    Core(#[from] ofdbox_core::OfdError),
    /// 文档结构不符合预期。
    #[error("OFD 结构错误: {0}")]
    InvalidStructure(String),
    /// 包内缺少条目。
    #[error("缺少包条目: {0}")]
    MissingEntry(String),
    /// 当前实现不支持的特性。
    #[error("不支持的 OFD 特性: {0}")]
    Unsupported(String),
    /// 资源缺失。
    #[error("缺少资源: {0}")]
    MissingResource(String),
    /// 图片处理失败。
    #[error("图片处理失败: {0}")]
    Image(String),
    /// 字体处理失败。
    #[error("字体处理失败: {0}")]
    Font(String),
    /// I/O 错误。
    #[error("I/O 错误: {0}")]
    Io(#[from] std::io::Error),
}

impl From<OfdError> for PageError {
    fn from(error: OfdError) -> Self {
        match error {
            OfdError::Unsupported(message) => Self::Unsupported(message),
            OfdError::MissingEntry(message) | OfdError::MissingResource(message) => {
                Self::Decode(message)
            }
            other => Self::Decode(other.to_string()),
        }
    }
}

/// OFD 文档源。
pub struct OfdDocument {
    package: RefCell<OfdPackage>,
    document: DocumentView,
    pages: Vec<PageView>,
    info: DocumentInfo,
    store: ResourceStore,
    font_cache: HashMap<String, Option<frontend::ResolvedFont>>,
    font_provider: Option<Box<dyn FontProvider>>,
    page_infos: RefCell<Vec<Option<PageInfo>>>,
    options: OfdReadOptions,
}

impl OfdDocument {
    /// 从内存字节打开 OFD。
    pub fn open_bytes(bytes: Vec<u8>, options: OfdReadOptions) -> Result<Self, OfdError> {
        Self::open_bytes_with_fonts(bytes, options, None)
    }

    /// 从内存字节打开 OFD，并注入缺失字体的来源。
    pub fn open_bytes_with_fonts(
        bytes: Vec<u8>,
        options: OfdReadOptions,
        font_provider: Option<Box<dyn FontProvider>>,
    ) -> Result<Self, OfdError> {
        let mut package = OfdPackage::from_bytes(bytes)?;
        let document = package.parse_document()?;
        let pages = document
            .pages()
            .map(|pages| pages.pages().collect::<Vec<_>>())
            .unwrap_or_default();
        let info = DocumentInfo {
            format: SourceFormat::Ofd,
            metadata: Metadata::default(),
            page_count: pages.len(),
            outline: Vec::new(),
            attachments: Vec::new(),
        };
        Ok(Self {
            package: RefCell::new(package),
            document,
            info,
            page_infos: RefCell::new(vec![None; pages.len()]),
            pages,
            store: ResourceStore::new(),
            font_cache: HashMap::new(),
            font_provider,
            options,
        })
    }

    /// 从文件打开 OFD。
    #[cfg(not(target_arch = "wasm32"))]
    pub fn open_path(path: impl AsRef<Path>, options: OfdReadOptions) -> Result<Self, OfdError> {
        Self::open_path_with_fonts(path, options, None)
    }

    /// 从文件打开 OFD，并注入缺失字体的来源。
    #[cfg(not(target_arch = "wasm32"))]
    pub fn open_path_with_fonts(
        path: impl AsRef<Path>,
        options: OfdReadOptions,
        font_provider: Option<Box<dyn FontProvider>>,
    ) -> Result<Self, OfdError> {
        let bytes = std::fs::read(path)?;
        Self::open_bytes_with_fonts(bytes, options, font_provider)
    }

    /// 页面数量。
    pub fn page_count(&self) -> usize {
        self.pages.len()
    }

    /// 资源表。
    pub fn resources(&self) -> &ResourceStore {
        &self.store
    }

    fn page_physical_box(&self, page: &PageView) -> Result<Rect, PageError> {
        let mut package = self.package.borrow_mut();
        let content = package
            .parse_page_for(page)
            .map_err(|error| PageError::Decode(error.to_string()))?;
        let physical = content
            .area()
            .and_then(|area| area.physical_box().and_then(parse_box))
            .or_else(|| {
                self.document
                    .common_data()
                    .and_then(|common| common.page_area())
                    .and_then(|area| area.physical_box().and_then(parse_box))
            })
            .unwrap_or_else(|| Rect::new(0.0, 0.0, 210.0, 297.0));
        Ok(Rect::new(0.0, 0.0, physical.width, physical.height))
    }
}

impl DocumentSource for OfdDocument {
    fn info(&self) -> &DocumentInfo {
        &self.info
    }

    fn page_info(&self, index: usize) -> Result<PageInfo, PageError> {
        if let Some(info) = self.page_infos.borrow().get(index).cloned().flatten() {
            return Ok(info);
        }
        let page = self.pages.get(index).ok_or(PageError::NotFound(index))?;
        let physical = self.page_physical_box(page)?;
        let info = PageInfo {
            media_box: physical,
            crop_box: physical,
            rotation: Rotation::None,
            label: None,
        };
        if let Some(slot) = self.page_infos.borrow_mut().get_mut(index) {
            *slot = Some(info.clone());
        }
        Ok(info)
    }

    fn load_page(&mut self, index: usize) -> Result<Page, PageError> {
        let page = self
            .pages
            .get(index)
            .cloned()
            .ok_or(PageError::NotFound(index))?;
        let mut package = self.package.borrow_mut();
        frontend::build_page(
            &mut package,
            &page,
            &self.options,
            &mut self.store,
            &mut self.font_cache,
            &mut self.font_provider,
        )
        .map_err(PageError::from)
    }

    fn load_attachment(&mut self, _id: AttachmentId) -> Result<Arc<[u8]>, PageError> {
        Err(PageError::Unsupported("附件读取尚未实现".to_owned()))
    }
}

fn parse_box(text: &str) -> Option<Rect> {
    let values = text
        .split(|character: char| character.is_whitespace() || character == ',')
        .filter(|value| !value.is_empty())
        .filter_map(|value| value.parse::<f64>().ok())
        .collect::<Vec<_>>();
    if values.len() < 4 {
        return None;
    }
    Some(Rect::new(values[0], values[1], values[2], values[3]))
}

#[cfg(test)]
mod tests {
    use super::{OfdDocument, OfdReadOptions, parse_box};
    use ofdbox_folio_ir::{DocumentSource, Element, Rect};
    use std::io::{Cursor, Write};

    fn minimal_ofd() -> Vec<u8> {
        let mut zip = zip::ZipWriter::new(Cursor::new(Vec::new()));
        let options = zip::write::SimpleFileOptions::default();
        let entries = [
            (
                "OFD.xml",
                r#"<?xml version="1.0" encoding="UTF-8"?>
<ofd:OFD xmlns:ofd="http://www.ofdspec.org/2016" Version="1.1" DocType="OFD">
  <ofd:DocBody>
    <ofd:DocInfo><ofd:DocID>test</ofd:DocID><ofd:Creator>test</ofd:Creator></ofd:DocInfo>
    <ofd:DocRoot>Doc_0/Document.xml</ofd:DocRoot>
  </ofd:DocBody>
</ofd:OFD>"#,
            ),
            (
                "Doc_0/Document.xml",
                r#"<?xml version="1.0" encoding="UTF-8"?>
<ofd:Document xmlns:ofd="http://www.ofdspec.org/2016">
  <ofd:CommonData>
    <ofd:MaxUnitID>3</ofd:MaxUnitID>
    <ofd:PageArea><ofd:PhysicalBox>0 0 100 100</ofd:PhysicalBox></ofd:PageArea>
  </ofd:CommonData>
  <ofd:Pages><ofd:Page ID="1" BaseLoc="Pages/Page_0/Content.xml"/></ofd:Pages>
</ofd:Document>"#,
            ),
            (
                "Doc_0/Pages/Page_0/Content.xml",
                r#"<?xml version="1.0" encoding="UTF-8"?>
<ofd:Page xmlns:ofd="http://www.ofdspec.org/2016">
  <ofd:Area><ofd:PhysicalBox>0 0 100 100</ofd:PhysicalBox></ofd:Area>
  <ofd:Content>
    <ofd:Layer ID="2">
      <ofd:PathObject ID="3" Boundary="10 10 20 20" Fill="true" Stroke="false">
        <ofd:FillColor Value="255 0 0"/>
        <ofd:AbbreviatedData>M 10 10 L 30 10 L 30 30 L 10 30 C</ofd:AbbreviatedData>
      </ofd:PathObject>
    </ofd:Layer>
  </ofd:Content>
</ofd:Page>"#,
            ),
        ];
        for (name, content) in entries {
            zip.start_file(name, options).expect("zip entry");
            zip.write_all(content.as_bytes()).expect("zip write");
        }
        zip.finish().expect("zip finish").into_inner()
    }

    fn count_elements(children: &[Element]) -> usize {
        children
            .iter()
            .map(|element| match element {
                Element::Group(group) => 1 + count_elements(&group.children),
                _ => 1,
            })
            .sum()
    }

    #[test]
    fn parses_physical_box() {
        assert_eq!(
            parse_box("0 0 210 297"),
            Some(Rect::new(0.0, 0.0, 210.0, 297.0))
        );
    }

    #[test]
    fn reads_minimal_ofd_package() {
        let mut document =
            OfdDocument::open_bytes(minimal_ofd(), OfdReadOptions::default()).expect("open");
        assert_eq!(document.info().page_count, 1);
        let info = document.page_info(0).expect("info");
        assert_eq!(info.media_box, Rect::new(0.0, 0.0, 100.0, 100.0));
        let page = document.load_page(0).expect("page");
        assert_eq!(page.content.size.width, 100.0);
        assert!(count_elements(&page.content.root.children) >= 2);
    }
}
