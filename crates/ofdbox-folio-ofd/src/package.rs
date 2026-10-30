//! Package reading: container/decryption, entry path resolution, lenient parsing and model loading.

use std::cell::RefCell;
use std::rc::Rc;

use ofdbox_core::model::annotation::PageAnnot;
use ofdbox_core::model::annotations::Annotations;
use ofdbox_core::model::attachments::Attachments;
use ofdbox_core::model::definition::StLoc;
use ofdbox_core::model::document::Document;
use ofdbox_core::model::ofd::Ofd;
use ofdbox_core::model::page::Page;
use ofdbox_core::model::res::Res;
use ofdbox_core::model_xml;
use ofdbox_core::store::OfdFile;

use crate::lenient;
use crate::{LenientMode, OfdDiagnostic, OfdError, OfdReadOptions};

/// Package reader: holds an `OfdFile` and handles decryption, path resolution, and
/// "strict first, lenient on failure" model parsing.
pub(crate) struct PackageReader {
    file: OfdFile,
    lenient: LenientMode,
    names: RefCell<Option<Rc<Vec<String>>>>,
    diagnostics: RefCell<Vec<OfdDiagnostic>>,
}

impl PackageReader {
    pub(crate) fn open(file: OfdFile, options: &OfdReadOptions) -> Result<Rc<Self>, OfdError> {
        if file.is_encrypted()? {
            if let Some(password) = options.password.clone() {
                file.enable_decryption(Box::new(ofdbox_crypto::PasswordBackend::new(password)))?;
            } else if let Some(private_key) = options.certificate_private_key.clone() {
                file.enable_decryption(Box::new(ofdbox_crypto::CertificateBackend::new(
                    private_key,
                )))?;
            } else {
                return Err(OfdError::Unsupported(
                    "OFD is encrypted; a password or certificate private key is required"
                        .to_owned(),
                ));
            }
        }
        Ok(Rc::new(Self {
            file,
            lenient: options.lenient,
            names: RefCell::new(None),
            diagnostics: RefCell::new(Vec::new()),
        }))
    }

    pub(crate) fn diagnostics(&self) -> Vec<OfdDiagnostic> {
        self.diagnostics.borrow().clone()
    }

    pub(crate) fn read_entry(&self, entry: &str) -> Result<Vec<u8>, OfdError> {
        self.file.read_entry(entry).map_err(OfdError::from)
    }

    fn names(&self) -> Result<Rc<Vec<String>>, OfdError> {
        if let Some(names) = self.names.borrow().as_ref() {
            return Ok(Rc::clone(names));
        }
        let names = Rc::new(self.file.entry_names()?);
        *self.names.borrow_mut() = Some(Rc::clone(&names));
        Ok(names)
    }

    pub(crate) fn resolve_existing(&self, base_dir: &str, loc: &StLoc) -> Option<String> {
        let names = self.names().ok()?;
        let candidates = candidates(base_dir, loc.as_str());
        for candidate in &candidates {
            if names.iter().any(|name| name == candidate) {
                return Some(candidate.clone());
            }
        }
        for candidate in &candidates {
            if let Some(name) = names
                .iter()
                .find(|name| name.eq_ignore_ascii_case(candidate))
            {
                return Some(name.clone());
            }
        }
        None
    }

    fn parse_with<T>(
        &self,
        entry: &str,
        parse: impl Fn(&[u8]) -> Result<T, model_xml::XmlError>,
    ) -> Result<T, OfdError> {
        let bytes = self.read_entry(entry)?;
        if self.lenient == LenientMode::Always {
            let (repaired, rules) = lenient::sanitize(entry, &bytes)?;
            let value = parse(&repaired)
                .map_err(|error| OfdError::InvalidStructure(format!("{entry}: {error}")))?;
            self.diagnostics.borrow_mut().extend(rules);
            return Ok(value);
        }
        match parse(&bytes) {
            Ok(value) => Ok(value),
            Err(strict) => {
                if self.lenient == LenientMode::Off {
                    return Err(OfdError::InvalidStructure(format!("{entry}: {strict}")));
                }
                let (repaired, rules) = lenient::sanitize(entry, &bytes)?;
                match parse(&repaired) {
                    Ok(value) => {
                        self.diagnostics.borrow_mut().extend(rules);
                        Ok(value)
                    }
                    Err(lenient_error) => Err(OfdError::InvalidStructure(format!(
                        "{entry}: strict parsing failed ({strict}), lenient parsing also failed ({lenient_error})"
                    ))),
                }
            }
        }
    }

    pub(crate) fn parse_ofd(&self, entry: &str) -> Result<Ofd, OfdError> {
        self.parse_with(entry, model_xml::from_bytes::<Ofd>)
    }

    pub(crate) fn parse_document(&self, entry: &str) -> Result<Document, OfdError> {
        self.parse_with(entry, model_xml::from_bytes::<Document>)
    }

    pub(crate) fn parse_page(&self, entry: &str) -> Result<Page, OfdError> {
        self.parse_with(entry, model_xml::from_bytes::<Page>)
    }

    pub(crate) fn parse_res(&self, entry: &str) -> Result<Res, OfdError> {
        self.parse_with(entry, model_xml::from_bytes::<Res>)
    }

    pub(crate) fn parse_annotations(&self, entry: &str) -> Result<Annotations, OfdError> {
        self.parse_with(entry, model_xml::from_bytes::<Annotations>)
    }

    pub(crate) fn parse_attachments(&self, entry: &str) -> Result<Attachments, OfdError> {
        self.parse_with(entry, model_xml::from_bytes::<Attachments>)
    }

    pub(crate) fn parse_page_annot(&self, entry: &str) -> Result<PageAnnot, OfdError> {
        self.parse_with(entry, model_xml::from_bytes::<PageAnnot>)
    }
}

/// Context of one XML entry: shared reader + the directory the current entry lives in.
pub(crate) struct Ctx {
    reader: Rc<PackageReader>,
    dir: String,
}

impl Ctx {
    pub(crate) fn root(reader: Rc<PackageReader>) -> Self {
        Self {
            reader,
            dir: String::new(),
        }
    }

    pub(crate) fn child(reader: Rc<PackageReader>, entry: &StLoc) -> Self {
        Self {
            reader,
            dir: parent_dir(entry.as_str()).to_owned(),
        }
    }

    pub(crate) fn resolve_existing(&self, loc: &StLoc) -> Option<String> {
        self.reader.resolve_existing(&self.dir, loc)
    }

    pub(crate) fn resolve(&self, loc: &StLoc) -> Result<StLoc, OfdError> {
        self.resolve_existing(loc)
            .map(StLoc::new)
            .ok_or_else(|| OfdError::MissingEntry(loc.0.clone()))
    }

    pub(crate) fn read_entry(&self, loc: &StLoc) -> Result<Vec<u8>, OfdError> {
        let entry = self.resolve(loc)?;
        self.reader.read_entry(entry.as_str())
    }

    pub(crate) fn load_page(&self, loc: &StLoc) -> Result<Loaded<Page>, OfdError> {
        let entry = self.resolve(loc)?;
        let data = self.reader.parse_page(entry.as_str())?;
        Ok(Loaded::new(
            data,
            Self::child(Rc::clone(&self.reader), &entry),
            entry,
        ))
    }

    pub(crate) fn load_res(&self, loc: &StLoc) -> Result<Rc<Loaded<Res>>, OfdError> {
        let entry = self.resolve(loc)?;
        let data = self.reader.parse_res(entry.as_str())?;
        Ok(Rc::new(Loaded::new(
            data,
            Self::child(Rc::clone(&self.reader), &entry),
            entry,
        )))
    }

    pub(crate) fn load_annotations(
        &self,
        loc: &StLoc,
    ) -> Result<Rc<Loaded<Annotations>>, OfdError> {
        let entry = self.resolve(loc)?;
        let data = self.reader.parse_annotations(entry.as_str())?;
        Ok(Rc::new(Loaded::new(
            data,
            Self::child(Rc::clone(&self.reader), &entry),
            entry,
        )))
    }

    pub(crate) fn load_attachments(
        &self,
        loc: &StLoc,
    ) -> Result<Rc<Loaded<Attachments>>, OfdError> {
        let entry = self.resolve(loc)?;
        let data = self.reader.parse_attachments(entry.as_str())?;
        Ok(Rc::new(Loaded::new(
            data,
            Self::child(Rc::clone(&self.reader), &entry),
            entry,
        )))
    }

    pub(crate) fn load_page_annot(&self, loc: &StLoc) -> Result<PageAnnot, OfdError> {
        let entry = self.resolve(loc)?;
        self.reader.parse_page_annot(entry.as_str())
    }

    pub(crate) fn read_res_file(
        &self,
        res: &Loaded<Res>,
        file: &StLoc,
    ) -> Result<Vec<u8>, OfdError> {
        let base = res.data.base_loc.as_str().trim();
        let raw = file.as_str().trim();
        let dir = res.ctx.dir.as_str();
        let mut candidates = Vec::new();
        if !base.is_empty() && base != "." {
            candidates.push(format!("{dir}/{base}/{raw}"));
            candidates.push(format!("/{base}/{raw}"));
        }
        candidates.push(format!("{dir}/{raw}"));
        candidates.push(format!("/{raw}"));
        for candidate in candidates {
            if let Some(entry) = self.reader.resolve_existing("", &StLoc::new(candidate)) {
                return self.reader.read_entry(&entry);
            }
        }
        Err(OfdError::MissingEntry(file.0.clone()))
    }
}

/// An object that has been parsed and had its context bound.
pub(crate) struct Loaded<T> {
    pub data: T,
    ctx: Ctx,
    location: StLoc,
    id: Option<String>,
}

impl<T> Loaded<T> {
    pub(crate) fn new(data: T, ctx: Ctx, location: StLoc) -> Self {
        Self {
            data,
            ctx,
            location,
            id: None,
        }
    }

    pub(crate) fn context(&self) -> &Ctx {
        &self.ctx
    }

    pub(crate) fn location(&self) -> &StLoc {
        &self.location
    }

    pub(crate) fn id(&self) -> Option<&str> {
        self.id.as_deref()
    }

    pub(crate) fn with_id(mut self, id: String) -> Self {
        self.id = Some(id);
        self
    }
}

/// Generates candidate entries: path relative to the current directory, and package-root path.
pub(crate) fn candidates(base_dir: &str, loc: &str) -> Vec<String> {
    let raw = loc.trim();
    let mut values = Vec::new();
    let mut push = |value: String| {
        let value = normalize(&value);
        if !value.is_empty() && !values.contains(&value) {
            values.push(value);
        }
    };
    if raw.starts_with('/') {
        push(raw.to_owned());
    } else {
        push(format!("{base_dir}/{raw}"));
        push(raw.to_owned());
    }
    values
}

fn normalize(path: &str) -> String {
    let mut parts: Vec<&str> = Vec::new();
    for part in path.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                parts.pop();
            }
            other => parts.push(other),
        }
    }
    parts.join("/")
}

/// Returns the parent directory of a path, normalized to have no trailing slash.
pub(crate) fn parent_dir(path: &str) -> &str {
    let path = path.trim_end_matches('/');
    match path.rsplit_once('/') {
        Some((parent, _)) => parent,
        None => "",
    }
}
