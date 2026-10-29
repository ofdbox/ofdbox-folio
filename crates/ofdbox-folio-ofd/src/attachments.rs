//! OFD 附件（`Attachments`）读取。
//!
//! GB/T 33190 §7.10：`Document.xml` 用 `Attachments` 指向附件列表文件，
//! 列表里的 `FileLoc` 是包内路径，相对附件列表文件所在目录解析。

use std::rc::Rc;
use std::sync::Arc;

use ofdbox_core::model::attachments::Attachments;
use ofdbox_core::model::definition::StLoc;
use ofdbox_core::model::document::Document;
use ofdbox_folio_ir::{AttachmentId, AttachmentInfo};

use crate::package::{Ctx, Loaded};

/// 解析后的附件集合。
#[derive(Default)]
pub(crate) struct OfdAttachments {
    /// 供 `DocumentInfo` 使用的附件元数据。
    pub info: Vec<AttachmentInfo>,
    /// 附件 ID 到 `FileLoc` 的映射。
    pub files: Vec<(AttachmentId, StLoc)>,
    /// 附件索引；读取附件内容时用它解析 `FileLoc`。
    index: Option<Rc<Loaded<Attachments>>>,
}

/// 读取附件列表；没有声明或解析失败时返回空集合。
pub(crate) fn read(ctx: &Ctx, document: &Document) -> OfdAttachments {
    let Some(loc) = document.attachments.as_ref() else {
        return OfdAttachments::default();
    };
    let Ok(index) = ctx.load_attachments(loc) else {
        return OfdAttachments::default();
    };
    let mut attachments = OfdAttachments::default();
    for (position, attachment) in index.data.attachments.iter().enumerate() {
        let id = AttachmentId::new(position as u64 + 1);
        let name = attachment.name.trim();
        let name = if name.is_empty() {
            format!("附件 {}", position + 1)
        } else {
            name.to_owned()
        };
        let size = attachment
            .size
            .filter(|size| *size > 0.0)
            .map_or(0, |size| size as u64);
        attachments.info.push(AttachmentInfo {
            id,
            name,
            format: attachment.format.clone(),
            size,
            created: None,
        });
        if !attachment.file_loc.as_str().trim().is_empty() {
            attachments.files.push((id, attachment.file_loc.clone()));
        }
    }
    attachments.index = Some(index);
    attachments
}

/// 读取附件内容。
pub(crate) fn load(
    attachments: &OfdAttachments,
    id: AttachmentId,
) -> Result<Arc<[u8]>, crate::OfdError> {
    let index = attachments
        .index
        .as_ref()
        .ok_or_else(|| crate::OfdError::MissingEntry("Attachments.xml".to_owned()))?;
    let file = attachments
        .files
        .iter()
        .find(|(attachment, _)| *attachment == id)
        .map(|(_, file)| file)
        .ok_or_else(|| crate::OfdError::MissingEntry(format!("附件 {}", id.get())))?;
    let bytes = index.context().read_entry(file)?;
    Ok(Arc::from(bytes))
}
