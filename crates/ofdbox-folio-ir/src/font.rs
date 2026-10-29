//! Font provider contract and TrueType subsetting.
//!
//! Format adapters and the layout engine obtain missing font bytes through this
//! trait. The library itself does not scan system fonts; callers may inject system
//! fonts, directory indexes, or remote font services.

use std::collections::HashSet;
use std::sync::Arc;

/// A font loading request.
#[derive(Clone, Debug, Default)]
pub struct FontRequest<'a> {
    /// Font resource ID in the source format.
    pub font_id: Option<&'a str>,
    /// Font name.
    pub font_name: Option<&'a str>,
    /// Font family name.
    pub family_name: Option<&'a str>,
    /// Charset.
    pub charset: Option<&'a str>,
    /// Whether bold.
    pub bold: bool,
    /// Whether italic.
    pub italic: bool,
    /// Whether serif.
    pub serif: bool,
    /// Whether fixed width.
    pub fixed_width: bool,
    /// Font file path declared by the source file.
    pub font_file: Option<&'a str>,
}

/// A font provider.
pub trait FontProvider {
    /// Return the font file bytes; returns `None` if not found.
    fn load(&mut self, request: &FontRequest<'_>) -> Result<Option<Arc<[u8]>>, FontError>;
}

/// A font provider error.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[error("font load failed: {0}")]
pub struct FontError(pub String);

impl FontError {
    /// Create an error.
    pub fn new(message: impl Into<String>) -> Self {
        Self(message.into())
    }
}

/// Split a single face out of a TrueType collection (`.ttc`) into a standalone font.
///
/// The SimSun and Microsoft YaHei fonts on Windows are collections: OFD's `FontFile`
/// can only hold a single font, so extraction by `face_index` is required before
/// embedding and subsetting. For a plain TTF or an out-of-range `face_index` this
/// returns `None`, and the caller continues with the original bytes.
pub fn extract_truetype_face(data: &[u8], face_index: u32) -> Option<Vec<u8>> {
    if !data.starts_with(b"ttcf") || data.len() < 12 {
        return None;
    }
    let count = usize::try_from(be_u32(data, 8)?).ok()?;
    let index = usize::try_from(face_index).ok()?;
    if index >= count {
        return None;
    }
    let face_offset = usize::try_from(be_u32(data, 12 + index * 4)?).ok()?;
    let num_tables = usize::from(be_u16(data, face_offset + 4)?);
    if num_tables == 0 || face_offset + 12 + num_tables * 16 > data.len() {
        return None;
    }
    let mut tables = Vec::with_capacity(num_tables);
    for table in 0..num_tables {
        let record = face_offset + 12 + table * 16;
        let tag: [u8; 4] = data.get(record..record + 4)?.try_into().ok()?;
        let offset = usize::try_from(be_u32(data, record + 8)?).ok()?;
        let length = usize::try_from(be_u32(data, record + 12)?).ok()?;
        let body = data.get(offset..offset.checked_add(length)?)?;
        tables.push((tag, body));
    }
    tables.sort_by_key(|table| table.0);

    let mut output = data.get(face_offset..face_offset + 12)?.to_vec();
    let table_count = u16::try_from(tables.len()).ok()?;
    output[4..6].copy_from_slice(&table_count.to_be_bytes());
    let table_offset = 12 + tables.len() * 16;
    let mut body = Vec::new();
    for (tag, table) in &tables {
        while body.len() % 4 != 0 {
            body.push(0);
        }
        let offset = table_offset + body.len();
        output.extend_from_slice(tag);
        output.extend_from_slice(&checksum(table).to_be_bytes());
        output.extend_from_slice(&u32::try_from(offset).ok()?.to_be_bytes());
        output.extend_from_slice(&u32::try_from(table.len()).ok()?.to_be_bytes());
        body.extend_from_slice(table);
    }
    output.extend_from_slice(&body);
    Some(output)
}

/// Trim a TrueType font down to the glyphs actually used.
///
/// Glyph IDs and tables such as `cmap`/`hmtx` are preserved and only unreferenced
/// outlines are cleared, so text that references the original GIDs (PDF Identity-H,
/// OFD `CGTransform`) remains valid; subglyphs referenced by composite glyphs are
/// kept as well.
///
/// Collection fonts are first split into a single face by `face_index`; for
/// TrueType fonts that do not use glyf outlines (really CFF/OTF) or for
/// structurally incomplete data, this returns `None` and the caller should fall
/// back to the original bytes.
pub fn subset_truetype(data: &[u8], face_index: u32, used: &HashSet<u16>) -> Option<Vec<u8>> {
    if data.starts_with(b"ttcf") {
        let face = extract_truetype_face(data, face_index)?;
        return subset_truetype(&face, 0, used);
    }
    if face_index != 0 || data.len() < 12 {
        return None;
    }
    let num_tables = usize::from(be_u16(data, 4)?);
    if num_tables == 0 || 12 + num_tables * 16 > data.len() {
        return None;
    }
    let mut tables = Vec::with_capacity(num_tables);
    for index in 0..num_tables {
        let record = 12 + index * 16;
        let tag: [u8; 4] = data.get(record..record + 4)?.try_into().ok()?;
        let offset = be_u32(data, record + 8)? as usize;
        let length = be_u32(data, record + 12)? as usize;
        let body = data.get(offset..offset.checked_add(length)?)?;
        tables.push((tag, body));
    }
    let find = |tag: &[u8; 4]| {
        tables
            .iter()
            .find(|(name, _)| name == tag)
            .map(|(_, body)| *body)
    };
    let head = find(b"head")?;
    let maxp = find(b"maxp")?;
    let loca = find(b"loca")?;
    let glyf = find(b"glyf")?;
    if head.len() < 54 || maxp.len() < 6 {
        return None;
    }
    let num_glyphs = usize::from(be_u16(maxp, 4)?);
    if num_glyphs == 0 {
        return None;
    }
    let long_loca = be_i16(head, 50)? != 0;
    let mut offsets = Vec::with_capacity(num_glyphs + 1);
    for index in 0..=num_glyphs {
        let offset = if long_loca {
            usize::try_from(be_u32(loca, index * 4)?).ok()?
        } else {
            usize::from(be_u16(loca, index * 2)?) * 2
        };
        offsets.push(offset);
    }
    if offsets.last().copied()? > glyf.len() {
        return None;
    }

    let mut used = used
        .iter()
        .copied()
        .filter(|glyph| usize::from(*glyph) < num_glyphs)
        .collect::<HashSet<_>>();
    used.insert(0);
    let mut queue = used.iter().copied().collect::<Vec<_>>();
    while let Some(glyph) = queue.pop() {
        let index = usize::from(glyph);
        let (Some(start), Some(end)) = (offsets.get(index), offsets.get(index + 1)) else {
            continue;
        };
        let (start, end) = (*start, *end);
        if end <= start {
            continue;
        }
        // A malformed single glyph should not force the whole font to skip subsetting; just skip it.
        let Some(body) = glyf.get(start..end) else {
            continue;
        };
        if be_i16(body, 0).is_none_or(|version| version >= 0) {
            continue;
        }
        let mut position = 10;
        while let (Some(flags), Some(component)) =
            (be_u16(body, position), be_u16(body, position + 2))
        {
            position += 4;
            position += if flags & 0x0001 != 0 { 4 } else { 2 };
            if flags & 0x0008 != 0 {
                position += 2;
            } else if flags & 0x0040 != 0 {
                position += 4;
            } else if flags & 0x0080 != 0 {
                position += 8;
            }
            if usize::from(component) < num_glyphs && used.insert(component) {
                queue.push(component);
            }
            if flags & 0x0020 == 0 {
                break;
            }
        }
    }

    let mut new_glyf = Vec::new();
    let mut new_offsets = Vec::with_capacity(num_glyphs + 1);
    for index in 0..num_glyphs {
        new_offsets.push(u32::try_from(new_glyf.len()).ok()?);
        if used.contains(&(index as u16))
            && let (Some(start), Some(end)) = (offsets.get(index), offsets.get(index + 1))
            && let Some(body) = glyf.get(*start..*end)
        {
            new_glyf.extend_from_slice(body);
            while new_glyf.len() % 4 != 0 {
                new_glyf.push(0);
            }
        }
    }
    new_offsets.push(u32::try_from(new_glyf.len()).ok()?);
    let mut new_loca = Vec::with_capacity(new_offsets.len() * 4);
    for offset in new_offsets {
        new_loca.extend_from_slice(&offset.to_be_bytes());
    }
    let mut new_head = head.to_vec();
    new_head[50..52].copy_from_slice(&1_i16.to_be_bytes());
    new_head[8..12].fill(0);

    let mut output_tables = tables
        .iter()
        .map(|(tag, body)| (*tag, body.to_vec()))
        .collect::<Vec<_>>();
    for (tag, body) in &mut output_tables {
        match tag.as_slice() {
            b"glyf" => *body = new_glyf.clone(),
            b"loca" => *body = new_loca.clone(),
            b"head" => *body = new_head.clone(),
            // Glyph names are useless for rendering; switching to version 3 can save from a few hundred KB to several MB.
            b"post" if body.len() >= 32 => {
                let mut post = body[..32].to_vec();
                post[0..4].copy_from_slice(&0x0003_0000_u32.to_be_bytes());
                *body = post;
            }
            _ => {}
        }
    }
    // Tables for bitmaps, color glyphs, and digital signatures are useless for vector rendering; drop them.
    output_tables.retain(|(tag, _)| !is_dropped_table(tag));
    output_tables.sort_by_key(|table| table.0);

    let mut output = data[..12].to_vec();
    let table_count = u16::try_from(output_tables.len()).ok()?;
    output[4..6].copy_from_slice(&table_count.to_be_bytes());
    let table_offset = 12 + output_tables.len() * 16;
    let mut body = Vec::new();
    for (tag, table) in &output_tables {
        while body.len() % 4 != 0 {
            body.push(0);
        }
        let offset = table_offset + body.len();
        output.extend_from_slice(tag);
        output.extend_from_slice(&checksum(table).to_be_bytes());
        output.extend_from_slice(&u32::try_from(offset).ok()?.to_be_bytes());
        output.extend_from_slice(&u32::try_from(table.len()).ok()?.to_be_bytes());
        body.extend_from_slice(table);
    }
    output.extend_from_slice(&body);
    Some(output)
}

/// Tables dropped when subsetting: bitmap glyphs, color glyphs, digital signatures, and hinting data.
///
/// Tables required for text such as `cmap`/`hmtx`/`name` are kept; dropping embedded
/// bitmaps such as `EBDT`/`EBLC` (several MB in SimSun) is what makes the subset size
/// actually track the number of characters used.
fn is_dropped_table(tag: &[u8; 4]) -> bool {
    matches!(
        tag,
        b"EBDT"
            | b"EBLC"
            | b"EBSC"
            | b"SVG "
            | b"sbix"
            | b"COLR"
            | b"CPAL"
            | b"CBDT"
            | b"CBLC"
            | b"DSIG"
            | b"hdmx"
            | b"LTSH"
            | b"VDMX"
            | b"PCLT"
    )
}

fn be_u16(data: &[u8], offset: usize) -> Option<u16> {
    Some(u16::from_be_bytes(
        data.get(offset..offset + 2)?.try_into().ok()?,
    ))
}

fn be_i16(data: &[u8], offset: usize) -> Option<i16> {
    Some(i16::from_be_bytes(
        data.get(offset..offset + 2)?.try_into().ok()?,
    ))
}

fn be_u32(data: &[u8], offset: usize) -> Option<u32> {
    Some(u32::from_be_bytes(
        data.get(offset..offset + 4)?.try_into().ok()?,
    ))
}

fn checksum(data: &[u8]) -> u32 {
    let mut sum = 0_u32;
    for chunk in data.chunks(4) {
        let mut word = [0_u8; 4];
        word[..chunk.len()].copy_from_slice(chunk);
        sum = sum.wrapping_add(u32::from_be_bytes(word));
    }
    sum
}

#[cfg(test)]
mod tests {
    use super::{extract_truetype_face, subset_truetype};
    use std::collections::HashSet;

    /// Build a minimal TrueType containing only head/maxp/loca/glyf.
    ///
    /// Glyph 0 is empty, and glyphs 1/2 each hold 16 bytes of empty outline data
    /// (long enough to locate, not required to be renderable).
    fn tiny_font(glyphs: usize) -> Vec<u8> {
        let mut glyf = Vec::new();
        for _ in 0..glyphs {
            glyf.extend_from_slice(&[0_u8; 16]);
        }
        let mut loca = Vec::new();
        for index in 0..=glyphs {
            loca.extend_from_slice(&(u16::try_from(index * 8).unwrap()).to_be_bytes());
        }
        let mut head = vec![0_u8; 54];
        // indexToLocFormat = 0 (short offsets).
        head[50..52].copy_from_slice(&0_i16.to_be_bytes());
        let mut maxp = vec![0_u8; 6];
        maxp[4..6].copy_from_slice(&(u16::try_from(glyphs).unwrap()).to_be_bytes());

        let tables: [(&[u8; 4], Vec<u8>); 4] = [
            (b"glyf", glyf),
            (b"head", head),
            (b"loca", loca),
            (b"maxp", maxp),
        ];
        let mut output = vec![0_u8; 12];
        output[0..4].copy_from_slice(&0x0001_0000_u32.to_be_bytes());
        output[4..6].copy_from_slice(&(tables.len() as u16).to_be_bytes());
        let table_offset = 12 + tables.len() * 16;
        let mut body = Vec::new();
        for (tag, table) in &tables {
            while body.len() % 4 != 0 {
                body.push(0);
            }
            output.extend_from_slice(*tag);
            output.extend_from_slice(&0_u32.to_be_bytes());
            output.extend_from_slice(
                &(u32::try_from(table_offset + body.len()).unwrap()).to_be_bytes(),
            );
            output.extend_from_slice(&(u32::try_from(table.len()).unwrap()).to_be_bytes());
            body.extend_from_slice(table);
        }
        output.extend_from_slice(&body);
        output
    }

    #[test]
    fn keeps_only_used_glyph_outlines() {
        let font = tiny_font(4);
        let used = HashSet::from([1_u16]);
        let subset = subset_truetype(&font, 0, &used).expect("subset");
        assert!(subset.len() < font.len(), "subset should be smaller");
        // The structure is still parseable: the first 12 header bytes are kept and the table count matches.
        assert_eq!(&subset[0..4], &font[0..4]);
        assert_eq!(u16::from_be_bytes([subset[4], subset[5]]), 4);
    }

    #[test]
    fn refuses_fonts_it_cannot_subset() {
        let used = HashSet::from([1_u16]);
        assert!(subset_truetype(b"", 0, &used).is_none());
        assert!(subset_truetype(b"ttcf\x00\x01\x00\x00", 0, &used).is_none());
        let font = tiny_font(4);
        // Face 2 of a collection font cannot be safely trimmed.
        assert!(subset_truetype(&font, 1, &used).is_none());
    }

    #[test]
    fn subsets_real_system_fonts() {
        let path = if cfg!(windows) {
            "C:/Windows/Fonts/simsun.ttc"
        } else {
            "/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf"
        };
        let Ok(data) = std::fs::read(path) else {
            return;
        };
        let used = HashSet::from([10_u16, 20, 30]);
        if data.starts_with(b"ttcf") {
            let face = extract_truetype_face(&data, 0).expect("extract face");
            assert!(
                face.len() < data.len(),
                "face {} >= ttc {}",
                face.len(),
                data.len()
            );
            let subset = subset_truetype(&data, 0, &used).expect("subset collection font");
            assert!(
                subset.len() < face.len(),
                "subset {} >= face {}",
                subset.len(),
                face.len()
            );
        } else {
            let subset = subset_truetype(&data, 0, &used).expect("subset");
            assert!(subset.len() < data.len());
        }
    }
}
