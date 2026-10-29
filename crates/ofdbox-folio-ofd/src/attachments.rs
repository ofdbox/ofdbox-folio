//! Reading OFD attachments (`Attachments`).
//!
//! GB/T 33190 §7.10: `Document.xml` uses `Attachments` to point at the attachment list file,
//! and the `FileLoc` entries in that list are package-internal paths, resolved relative to
//! the directory containing the attachment list file.

use std::rc::Rc;
use std::sync::Arc;

use ofdbox_core::model::attachments::Attachments;
use ofdbox_core::model::definition::StLoc;
use ofdbox_core::model::document::Document;
use ofdbox_folio_ir::{AttachmentId, AttachmentInfo};

use crate::package::{Ctx, Loaded};

/// Parsed collection of attachments.
#[derive(Default)]
pub(crate) struct OfdAttachments {
    /// Attachment metadata used by `DocumentInfo`.
    pub info: Vec<AttachmentInfo>,
    /// Mapping from attachment ID to `FileLoc`.
    pub files: Vec<(AttachmentId, StLoc)>,
    /// Attachment index; used to resolve `FileLoc` when reading attachment contents.
    index: Option<Rc<Loaded<Attachments>>>,
}

/// Reads the attachment list; returns an empty collection when it is not declared or fails to parse.
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
            format!("Attachment {}", position + 1)
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

/// Reads attachment contents.
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
        .ok_or_else(|| crate::OfdError::MissingEntry(format!("attachment {}", id.get())))?;
    let bytes = index.context().read_entry(file)?;
    Ok(Arc::from(bytes))
}
