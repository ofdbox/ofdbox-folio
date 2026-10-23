//! OFD 附件（`Attachments`）读取。
//!
//! GB/T 33190 §7.10：`Document.xml` 用 `Attachments` 指向附件列表文件，
//! 列表里的 `FileLoc` 是包内路径，相对附件列表文件所在目录解析。

use std::sync::Arc;

use ofdbox_core::{DocumentView, OfdPackage};
use ofdbox_folio_ir::{AttachmentId, AttachmentInfo};

/// 解析后的附件集合。
#[derive(Default)]
pub(crate) struct OfdAttachments {
    /// 供 `DocumentInfo` 使用的附件元数据。
    pub info: Vec<AttachmentInfo>,
    /// 附件 ID 到 `FileLoc` 的映射。
    pub files: Vec<(AttachmentId, String)>,
    /// 附件列表文件所在目录，用于解析 `FileLoc`。
    pub dir: String,
}

/// 读取附件列表；没有声明或解析失败时返回空集合。
pub(crate) fn read(package: &mut OfdPackage, document: &DocumentView) -> OfdAttachments {
    let Some(loc) = document.attachments().map(str::to_owned) else {
        return OfdAttachments::default();
    };
    let dir = attachment_dir(package, &loc);
    let Ok(view) = package.parse_attachments() else {
        return OfdAttachments::default();
    };
    let mut attachments = OfdAttachments {
        dir,
        ..OfdAttachments::default()
    };
    for (index, attachment) in view.attachments().enumerate() {
        let id = AttachmentId::new(index as u64 + 1);
        let name = attachment.name().map(str::trim).unwrap_or_default();
        let name = if name.is_empty() {
            format!("附件 {}", index + 1)
        } else {
            name.to_owned()
        };
        let size = attachment
            .size()
            .filter(|size| *size > 0.0)
            .map_or(0, |size| size as u64);
        attachments.info.push(AttachmentInfo {
            id,
            name,
            format: attachment.format().map(str::to_owned),
            size,
            created: None,
        });
        if let Some(file) = attachment.file_loc() {
            attachments.files.push((id, file.to_owned()));
        }
    }
    attachments
}

/// 读取附件内容。
pub(crate) fn load(
    package: &mut OfdPackage,
    dir: &str,
    file: &str,
) -> Result<Arc<[u8]>, crate::OfdError> {
    crate::frontend::read_resource(package, dir, None, file)
}

/// 附件列表文件所在目录；`FileLoc` 相对它解析。
///
/// 例如 `Document.xml` 在 `Doc_0/`、附件列表是 `Attachments/Attachments.xml`，
/// 目录就是 `Doc_0/Attachments`。
fn attachment_dir(package: &OfdPackage, loc: &str) -> String {
    let document_dir = package
        .document_entry_name()
        .and_then(parent_dir)
        .unwrap_or_default();
    let entry = package
        .resolve_entry(&document_dir, loc)
        .unwrap_or_else(|_| loc.to_owned());
    parent_dir(&entry).unwrap_or(document_dir)
}

/// 取路径的父目录，统一成不带尾斜杠的形式。
fn parent_dir(path: &str) -> Option<String> {
    let path = path.trim_end_matches('/');
    let (parent, _) = path.rsplit_once('/')?;
    Some(parent.trim_end_matches('/').to_owned())
}
