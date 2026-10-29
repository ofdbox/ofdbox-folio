//! OFDBox Folio 的 OFD 适配层。
//!
//! 负责把 OFD 包解析为 Fixed IR 页面，以及把 Fixed IR 写出为 OFD 包。
//! 对外不暴露 `ofdbox_core` 的类型。
//!
//! 读取基于 `OfdFile`（容器、解密、entry 访问）与 folio 自己的宽松解析：
//! 先按 typed model 严格解析，失败后规范化 XML 再试一次。

#![allow(clippy::too_many_lines)]

mod attachments;
mod frontend;
mod lenient;
mod outline;
mod package;
mod writer;

use std::cell::RefCell;
use std::collections::{BTreeSet, HashMap};
#[cfg(not(target_arch = "wasm32"))]
use std::path::Path;
use std::rc::Rc;
use std::sync::Arc;

use ofdbox_core::OfdPackage;
use ofdbox_core::model::definition::StBox;
use ofdbox_core::model::document::{Document, PageEntry};
use ofdbox_core::model::page::Page as PageModel;
use ofdbox_core::store::OfdFile;
use ofdbox_folio_ir::{
    AttachmentId, DocumentInfo, DocumentSource, FontProvider, Metadata, Page, PageError, PageInfo,
    Rect, ResourceStore, Rotation, SourceFormat,
};

use crate::package::{Ctx, Loaded, PackageReader};

pub use frontend::{OfdReadOptions, UnsupportedPolicy};
pub use writer::{OfdWriteOptions, write_fixed};

/// 宽松解析模式。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum LenientMode {
    /// 只用严格解析。
    Off,
    /// 严格解析失败后回退宽松解析（默认）。
    #[default]
    OnFailure,
    /// 所有 entry 都先规范化再解析。
    Always,
}

/// 宽松解析时执行的一类修复。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum OfdRepair {
    /// 命名空间前缀/URI 规范化。
    Namespace,
    /// 注入缺失的图元/图层 ID。
    InjectId,
    /// 注入缺失的必选属性。
    InjectAttribute,
    /// 注入缺失的必选子元素。
    InjectChild,
    /// 丢弃无法解析的图元（例如缺资源引用的图片）。
    DropObject,
    /// 清洗数值分隔符。
    CleanNumber,
}

/// 一条宽松解析诊断。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OfdDiagnostic {
    /// 发生修复的 entry。
    pub entry: String,
    /// 修复类型。
    pub repair: OfdRepair,
    /// 面向开发者的说明。
    pub detail: String,
}

/// 探测字节是否为 OFD 包。
///
/// 复用 `ofdbox-core` 的 ZIP 读取，因此能容忍前置字节、空 EOCD 等
/// 真实文件里常见的非标准封装；不解析页面。
pub fn probe(bytes: &[u8]) -> bool {
    OfdPackage::probe(bytes).unwrap_or(false)
}

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
    reader: Rc<PackageReader>,
    document: Loaded<Document>,
    pages: Vec<PageEntry>,
    info: DocumentInfo,
    store: ResourceStore,
    attachments: attachments::OfdAttachments,
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
        let file = OfdFile::open_bytes(bytes)?;
        let reader = PackageReader::open(file, &options)?;
        let root = Ctx::root(Rc::clone(&reader));
        let ofd = reader.parse_ofd("OFD.xml")?;
        let doc_root = ofd
            .doc_bodies
            .first()
            .map(|body| body.doc_root.clone())
            .ok_or_else(|| OfdError::InvalidStructure("OFD.xml 没有 DocBody".to_owned()))?;
        let document_entry = root.resolve(&doc_root)?;
        let document_data = reader.parse_document(document_entry.as_str())?;
        let document = Loaded::new(
            document_data,
            Ctx::child(Rc::clone(&reader), &document_entry),
            document_entry,
        );
        Self::from_document(reader, document, options, font_provider)
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

    fn from_document(
        reader: Rc<PackageReader>,
        document: Loaded<Document>,
        options: OfdReadOptions,
        font_provider: Option<Box<dyn FontProvider>>,
    ) -> Result<Self, OfdError> {
        let pages = document.data.pages.pages.clone();
        let page_count = pages.len();
        let attachments = attachments::read(document.context(), &document.data);
        let info = DocumentInfo {
            format: SourceFormat::Ofd,
            metadata: Metadata::default(),
            page_count: pages.len(),
            outline: outline::build_outline(&document.data, &pages),
            attachments: attachments.info.clone(),
        };
        Ok(Self {
            reader,
            document,
            pages,
            info,
            attachments,
            store: ResourceStore::new(),
            font_cache: HashMap::new(),
            font_provider,
            page_infos: RefCell::new(vec![None; page_count]),
            options,
        })
    }

    /// 页面数量。
    pub fn page_count(&self) -> usize {
        self.pages.len()
    }

    /// 资源表。
    pub fn resources(&self) -> &ResourceStore {
        &self.store
    }

    /// 读取过程中发生的宽松修复（含已加载页面）。
    pub fn diagnostics(&self) -> Vec<OfdDiagnostic> {
        self.reader.diagnostics()
    }

    /// 页面尺寸：页面 Area → 模板页 Area → 文档 PageArea。
    fn page_physical_box(&self, page: &PageEntry) -> Result<Rect, PageError> {
        let loaded = self
            .document
            .context()
            .load_page(&page.base_loc)
            .map_err(|error| PageError::Decode(error.to_string()))?;
        let physical = loaded
            .data
            .area
            .as_ref()
            .map(|area| area.physical_box)
            .or_else(|| self.template_physical_box(&loaded))
            .unwrap_or(self.document.data.common_data.page_area.physical_box);
        let physical = stbox_rect(physical);
        if physical.width > 0.0 && physical.height > 0.0 {
            Ok(physical)
        } else {
            Ok(Rect::new(0.0, 0.0, 210.0, 297.0))
        }
    }

    /// 页面引用的模板页里第一个声明了 Area 的尺寸。
    fn template_physical_box(&self, page: &Loaded<PageModel>) -> Option<StBox> {
        for reference in &page.data.templates {
            let Some(template) = self
                .document
                .data
                .common_data
                .template_pages
                .iter()
                .find(|template| template.id.0 == reference.template_id.0)
            else {
                continue;
            };
            let Ok(loaded) = self.document.context().load_page(&template.base_loc) else {
                continue;
            };
            if let Some(area) = &loaded.data.area {
                return Some(area.physical_box);
            }
        }
        None
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
        let entry = self
            .pages
            .get(index)
            .ok_or(PageError::NotFound(index))?
            .clone();
        let loaded = self
            .document
            .context()
            .load_page(&entry.base_loc)
            .map_err(|error| PageError::Decode(error.to_string()))?
            .with_id(entry.id.clone());
        let mut page = frontend::build_page(
            &self.document,
            &loaded,
            &self.options,
            &mut self.store,
            &mut self.font_cache,
            &mut self.font_provider,
        )
        .map_err(PageError::from)?;
        page.text.page = index;
        Ok(page)
    }

    fn load_attachment(&mut self, id: AttachmentId) -> Result<Arc<[u8]>, PageError> {
        attachments::load(&self.attachments, id).map_err(PageError::from)
    }

    fn font_names(&mut self) -> Result<Vec<String>, PageError> {
        let mut names = BTreeSet::new();
        let common = &self.document.data.common_data;
        for location in common.public_res.iter().chain(common.document_res.iter()) {
            let Ok(res) = self.document.context().load_res(location) else {
                continue;
            };
            let Some(fonts) = res.data.fonts.as_ref() else {
                continue;
            };
            for font in &fonts.fonts {
                for name in [Some(font.font_name.as_str()), font.family_name.as_deref()]
                    .into_iter()
                    .flatten()
                {
                    let name = name.trim();
                    if !name.is_empty() {
                        names.insert(name.to_owned());
                    }
                }
            }
        }
        Ok(names.into_iter().collect())
    }

    fn clear_caches(&mut self) {
        self.font_cache.clear();
        if let Ok(mut infos) = self.page_infos.try_borrow_mut() {
            infos.fill(None);
        }
    }
}

/// `StBox` 到 Fixed IR 矩形（原点固定为左上角）。
fn stbox_rect(value: StBox) -> Rect {
    Rect::new(0.0, 0.0, value.width, value.height)
}

#[cfg(test)]
mod tests {
    use super::{OfdDocument, OfdReadOptions, stbox_rect};
    use ofdbox_core::model::definition::StBox;
    use ofdbox_folio_ir::{DocumentSource, Element, Rect};
    use std::io::{Cursor, Write};

    fn minimal_ofd() -> Vec<u8> {
        let mut zip = zip::ZipWriter::new(Cursor::new(Vec::new()));
        let options = zip::write::SimpleFileOptions::default();
        let entries = [
            (
                "OFD.xml",
                r#"<?xml version="1.0" encoding="UTF-8"?>
<ofd:OFD xmlns:ofd="http://www.ofdspec.org/2016" Version="1.0" DocType="OFD">
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
    fn converts_physical_box() {
        assert_eq!(
            stbox_rect(StBox::new(0.0, 0.0, 210.0, 297.0)),
            Rect::new(0.0, 0.0, 210.0, 297.0)
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

    /// 带两级大纲的两页 OFD。
    fn ofd_with_outline() -> Vec<u8> {
        let mut zip = zip::ZipWriter::new(Cursor::new(Vec::new()));
        let options = zip::write::SimpleFileOptions::default();
        let page = r#"<?xml version="1.0" encoding="UTF-8"?>
<ofd:Page xmlns:ofd="http://www.ofdspec.org/2016">
  <ofd:Area><ofd:PhysicalBox>0 0 100 100</ofd:PhysicalBox></ofd:Area>
  <ofd:Content><ofd:Layer ID="2"/></ofd:Content>
</ofd:Page>"#;
        let entries = [
            (
                "OFD.xml",
                r#"<?xml version="1.0" encoding="UTF-8"?>
<ofd:OFD xmlns:ofd="http://www.ofdspec.org/2016" Version="1.0" DocType="OFD">
  <ofd:DocBody>
    <ofd:DocInfo><ofd:DocID>outline</ofd:DocID><ofd:Creator>test</ofd:Creator></ofd:DocInfo>
    <ofd:DocRoot>Doc_0/Document.xml</ofd:DocRoot>
  </ofd:DocBody>
</ofd:OFD>"#,
            ),
            (
                "Doc_0/Document.xml",
                r#"<?xml version="1.0" encoding="UTF-8"?>
<ofd:Document xmlns:ofd="http://www.ofdspec.org/2016">
  <ofd:CommonData>
    <ofd:MaxUnitID>9</ofd:MaxUnitID>
    <ofd:PageArea><ofd:PhysicalBox>0 0 100 100</ofd:PhysicalBox></ofd:PageArea>
  </ofd:CommonData>
  <ofd:Pages>
    <ofd:Page ID="101" BaseLoc="Pages/Page_0/Content.xml"/>
    <ofd:Page ID="629" BaseLoc="Pages/Page_1/Content.xml"/>
  </ofd:Pages>
  <ofd:Outlines>
    <ofd:OutlineElem Title="扉页">
      <ofd:Actions><ofd:Action Event="DO"><ofd:Goto><ofd:Dest Type="Fit" PageID="101"/></ofd:Goto></ofd:Action></ofd:Actions>
    </ofd:OutlineElem>
    <ofd:OutlineElem Title="正文">
      <ofd:Actions><ofd:Action Event="DO"><ofd:Goto><ofd:Dest Type="XYZ" PageID="629" Left="12.5" Top="34"/></ofd:Goto></ofd:Action></ofd:Actions>
      <ofd:OutlineElem Title="第一节">
        <ofd:Actions><ofd:Action Event="DO"><ofd:Goto><ofd:Dest Type="Fit" PageID="629"/></ofd:Goto></ofd:Action></ofd:Actions>
      </ofd:OutlineElem>
    </ofd:OutlineElem>
  </ofd:Outlines>
</ofd:Document>"#,
            ),
            ("Doc_0/Pages/Page_0/Content.xml", page),
            ("Doc_0/Pages/Page_1/Content.xml", page),
        ];
        for (name, content) in entries {
            zip.start_file(name, options).expect("zip entry");
            zip.write_all(content.as_bytes()).expect("zip write");
        }
        zip.finish().expect("zip finish").into_inner()
    }

    #[test]
    fn reads_outline_with_nesting() {
        let document =
            OfdDocument::open_bytes(ofd_with_outline(), OfdReadOptions::default()).expect("open");
        let outline = &document.info().outline;
        assert_eq!(outline.len(), 2, "顶层节点: {outline:?}");
        assert_eq!(outline[0].title, "扉页");
        assert_eq!(outline[0].destination.page, 0);
        assert!(outline[0].children.is_empty());

        let body = &outline[1];
        assert_eq!(body.title, "正文");
        assert_eq!(body.destination.page, 1);
        assert!((body.destination.point.x - 12.5).abs() < 1e-6);
        assert!((body.destination.point.y - 34.0).abs() < 1e-6);
        assert_eq!(body.children.len(), 1);
        assert_eq!(body.children[0].title, "第一节");
        assert_eq!(body.children[0].destination.page, 1);
    }

    #[test]
    fn outline_is_empty_without_outlines() {
        let document =
            OfdDocument::open_bytes(minimal_ofd(), OfdReadOptions::default()).expect("open");
        assert!(document.info().outline.is_empty());
    }

    /// 带一个附件的 OFD。
    fn ofd_with_attachment() -> Vec<u8> {
        let mut zip = zip::ZipWriter::new(Cursor::new(Vec::new()));
        let options = zip::write::SimpleFileOptions::default();
        let entries: [(&str, &[u8]); 5] = [
            (
                "OFD.xml",
                br#"<?xml version="1.0" encoding="UTF-8"?>
<ofd:OFD xmlns:ofd="http://www.ofdspec.org/2016" Version="1.0" DocType="OFD">
  <ofd:DocBody>
    <ofd:DocInfo><ofd:DocID>attach</ofd:DocID><ofd:Creator>test</ofd:Creator></ofd:DocInfo>
    <ofd:DocRoot>Doc_0/Document.xml</ofd:DocRoot>
  </ofd:DocBody>
</ofd:OFD>"#,
            ),
            (
                "Doc_0/Document.xml",
                br#"<?xml version="1.0" encoding="UTF-8"?>
<ofd:Document xmlns:ofd="http://www.ofdspec.org/2016">
  <ofd:CommonData>
    <ofd:MaxUnitID>3</ofd:MaxUnitID>
    <ofd:PageArea><ofd:PhysicalBox>0 0 100 100</ofd:PhysicalBox></ofd:PageArea>
  </ofd:CommonData>
  <ofd:Pages><ofd:Page ID="1" BaseLoc="Pages/Page_0/Content.xml"/></ofd:Pages>
  <ofd:Attachments>Attachments/Attachments.xml</ofd:Attachments>
</ofd:Document>"#,
            ),
            (
                "Doc_0/Pages/Page_0/Content.xml",
                br#"<?xml version="1.0" encoding="UTF-8"?>
<ofd:Page xmlns:ofd="http://www.ofdspec.org/2016">
  <ofd:Area><ofd:PhysicalBox>0 0 100 100</ofd:PhysicalBox></ofd:Area>
  <ofd:Content><ofd:Layer ID="2"/></ofd:Content>
</ofd:Page>"#,
            ),
            (
                "Doc_0/Attachments/Attachments.xml",
                br#"<?xml version="1.0" encoding="UTF-8"?>
<ofd:Attachments xmlns:ofd="http://www.ofdspec.org/2016">
  <ofd:Attachment ID="1" Name="note.txt" Format="txt" Size="5">
    <ofd:FileLoc>note.txt</ofd:FileLoc>
  </ofd:Attachment>
</ofd:Attachments>"#,
            ),
            ("Doc_0/Attachments/note.txt", b"hello"),
        ];
        for (name, content) in entries {
            zip.start_file(name, options).expect("zip entry");
            zip.write_all(content).expect("zip write");
        }
        zip.finish().expect("zip finish").into_inner()
    }

    #[test]
    fn reads_attachment_list_and_bytes() {
        let mut document =
            OfdDocument::open_bytes(ofd_with_attachment(), OfdReadOptions::default())
                .expect("open");
        let attachments = &document.info().attachments;
        assert_eq!(attachments.len(), 1, "附件: {attachments:?}");
        assert_eq!(attachments[0].name, "note.txt");
        assert_eq!(attachments[0].format.as_deref(), Some("txt"));
        assert_eq!(attachments[0].size, 5);

        let bytes = document
            .load_attachment(attachments[0].id)
            .expect("load attachment");
        assert_eq!(&bytes[..], b"hello");
    }

    #[test]
    fn attachments_are_empty_without_declaration() {
        let document =
            OfdDocument::open_bytes(minimal_ofd(), OfdReadOptions::default()).expect("open");
        assert!(document.info().attachments.is_empty());
    }

    #[test]
    fn lenient_mode_repairs_missing_required_fields() {
        let mut zip = zip::ZipWriter::new(Cursor::new(Vec::new()));
        let options = zip::write::SimpleFileOptions::default();
        let entries = [
            (
                "OFD.xml",
                r#"<?xml version="1.0" encoding="UTF-8"?>
<ofd:OFD xmlns:ofd="http://www.ofdspec.org/2016" Version="1.0" DocType="OFD">
  <ofd:DocBody>
    <ofd:DocInfo><ofd:DocID>lenient</ofd:DocID></ofd:DocInfo>
    <ofd:DocRoot>Doc_0/Document.xml</ofd:DocRoot>
  </ofd:DocBody>
</ofd:OFD>"#,
            ),
            (
                "Doc_0/Document.xml",
                r#"<?xml version="1.0" encoding="UTF-8"?>
<ofd:Document xmlns:ofd="http://www.ofdspec.org/2016">
  <ofd:CommonData><ofd:MaxUnitID>3</ofd:MaxUnitID></ofd:CommonData>
  <ofd:Pages><ofd:Page ID="1" BaseLoc="Pages/Page_0/Content.xml"/></ofd:Pages>
</ofd:Document>"#,
            ),
            (
                "Doc_0/Pages/Page_0/Content.xml",
                r#"<?xml version="1.0" encoding="UTF-8"?>
<ofd:Page xmlns:ofd="http://www.ofdspec.org/2016">
  <ofd:Content>
    <ofd:Layer>
      <ofd:PathObject Boundary="0 0 10 10">
        <ofd:AbbreviatedData>M 0 0 L 10 10 C</ofd:AbbreviatedData>
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
        let bytes = zip.finish().expect("zip finish").into_inner();
        let mut document = OfdDocument::open_bytes(bytes, OfdReadOptions::default()).expect("open");
        let page = document.load_page(0).expect("page");
        assert_eq!(page.content.size.width, 210.0);
        assert!(!document.diagnostics().is_empty());
    }
}
