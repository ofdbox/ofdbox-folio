//! Minimal support for bare CFF tables embedded by document formats.
//!
//! PDF (`/FontFile3` with `/CIDFontType0C` or `/Type1C`) and OFD (`FontFile` entries with a CFF
//! header instead of OpenType) both allow a standalone CFF table instead of an SFNT font. The
//! render pipeline (skrifa / ttf-parser) only understands SFNT fonts, so the CFF table is parsed
//! here to recover the CID-to-glyph mapping and then wrapped in a minimal OpenType container.

use std::collections::HashMap;

/// Narrow error type for the CFF adapter.
#[derive(Debug, thiserror::Error)]
pub enum CffError {
    /// The data is too short or structurally invalid.
    #[error("invalid CFF font: {0}")]
    Invalid(String),
}

/// A parsed standalone CFF font.
pub struct BareCff {
    /// Number of glyphs (including `.notdef`).
    pub glyphs: u16,
    /// CID to glyph index mapping; empty for non-CID fonts.
    pub cid_to_gid: HashMap<u16, u16>,
    /// Character code to glyph index mapping from the CFF Encoding; empty for CID fonts.
    pub code_to_gid: HashMap<u8, u16>,
    /// Glyph index to CID (or SID) mapping, indexed by glyph.
    pub cid_of_gid: Vec<u16>,
}

/// Parses a standalone CFF table and recovers its glyph count, charset and encoding mappings.
pub fn parse_bare_cff(data: &[u8]) -> Result<BareCff, CffError> {
    if data.len() < 4 || data[0] != 1 {
        return Err(CffError::Invalid("missing CFF header".to_owned()));
    }
    let header_size = usize::from(data[2]).max(4);
    let (names, position) = read_index(data, header_size)?;
    let _ = names;
    let (top_dicts, position) = read_index(data, position)?;
    let top_dict = top_dicts
        .first()
        .ok_or_else(|| CffError::Invalid("missing top dict".to_owned()))?;
    let (_, position) = read_index(data, position)?;
    let (_, position) = read_index(data, position)?;
    let _ = position;
    let dict = parse_dict(top_dict)?;
    let charstrings_offset = dict
        .get(&17)
        .and_then(|values| values.first())
        .copied()
        .ok_or_else(|| CffError::Invalid("missing CharStrings".to_owned()))?;
    if !(charstrings_offset.is_finite() && charstrings_offset >= 0.0) {
        return Err(CffError::Invalid("invalid CharStrings offset".to_owned()));
    }
    let charstrings_offset = charstrings_offset as usize;
    let (charstrings, _) = read_index(data, charstrings_offset)?;
    let glyphs = u16::try_from(charstrings.len())
        .map_err(|_| CffError::Invalid("too many glyphs".to_owned()))?;
    let is_cid = dict.contains_key(&(0x0C00 | 30));
    let mut cid_of_gid = vec![0_u16; charstrings.len()];
    if let Some(values) = dict.get(&15)
        && let Some(offset) = values.first()
    {
        if !(offset.is_finite() && *offset >= 0.0) {
            return Err(CffError::Invalid("invalid charset offset".to_owned()));
        }
        let offset = *offset as usize;
        read_charset(data, offset, &mut cid_of_gid)?;
    } else {
        // ISOAdobe charset: glyph index equals SID/CID for the first 229 glyphs.
        for (index, cid) in cid_of_gid.iter_mut().enumerate() {
            *cid = u16::try_from(index).unwrap_or(u16::MAX);
        }
    }
    let mut cid_to_gid = HashMap::new();
    for (gid, cid) in cid_of_gid.iter().enumerate() {
        let gid = u16::try_from(gid).unwrap_or(u16::MAX);
        cid_to_gid.entry(*cid).or_insert(gid);
    }
    let code_to_gid = if is_cid {
        HashMap::new()
    } else {
        let mut mapping = HashMap::new();
        if let Some(values) = dict.get(&16)
            && let Some(offset) = values.first()
            && *offset >= 0.0
        {
            read_encoding(data, *offset as usize, &cid_of_gid, &mut mapping)?;
        }
        mapping
    };
    Ok(BareCff {
        glyphs,
        cid_to_gid: if is_cid { cid_to_gid } else { HashMap::new() },
        code_to_gid,
        cid_of_gid,
    })
}

/// Reads a CFF Encoding table (character code -> glyph index).
fn read_encoding(
    data: &[u8],
    offset: usize,
    cid_of_gid: &[u16],
    mapping: &mut HashMap<u8, u16>,
) -> Result<(), CffError> {
    let format = *data
        .get(offset)
        .ok_or_else(|| CffError::Invalid("encoding out of range".to_owned()))?;
    let mut position = offset + 1;
    let mut glyph = 1_usize;
    match format & 0x7F {
        0 => {
            let count = usize::from(
                *data
                    .get(position)
                    .ok_or_else(|| CffError::Invalid("encoding truncated".to_owned()))?,
            );
            position += 1;
            for _ in 0..count {
                let code = *data
                    .get(position)
                    .ok_or_else(|| CffError::Invalid("encoding truncated".to_owned()))?;
                position += 1;
                if let Ok(gid) = u16::try_from(glyph) {
                    mapping.insert(code, gid);
                }
                glyph += 1;
            }
        }
        1 => {
            let ranges = usize::from(
                *data
                    .get(position)
                    .ok_or_else(|| CffError::Invalid("encoding truncated".to_owned()))?,
            );
            position += 1;
            for _ in 0..ranges {
                let first = *data
                    .get(position)
                    .ok_or_else(|| CffError::Invalid("encoding truncated".to_owned()))?;
                let left = *data
                    .get(position + 1)
                    .ok_or_else(|| CffError::Invalid("encoding truncated".to_owned()))?;
                position += 2;
                for index in 0..=left {
                    let code = first.wrapping_add(index);
                    if let Ok(gid) = u16::try_from(glyph) {
                        mapping.insert(code, gid);
                    }
                    glyph += 1;
                }
            }
        }
        other => return Err(CffError::Invalid(format!("encoding format {other}"))),
    }
    if format & 0x80 != 0 {
        let supplements = usize::from(
            *data
                .get(position)
                .ok_or_else(|| CffError::Invalid("encoding truncated".to_owned()))?,
        );
        position += 1;
        for _ in 0..supplements {
            let code = *data
                .get(position)
                .ok_or_else(|| CffError::Invalid("encoding truncated".to_owned()))?;
            let sid = read_u16(data, position + 1)?;
            position += 3;
            if let Some(gid) = cid_of_gid
                .iter()
                .position(|value| *value == sid)
                .and_then(|gid| u16::try_from(gid).ok())
            {
                mapping.insert(code, gid);
            }
        }
    }
    Ok(())
}

fn read_charset(data: &[u8], offset: usize, cid_of_gid: &mut [u16]) -> Result<(), CffError> {
    let format = *data
        .get(offset)
        .ok_or_else(|| CffError::Invalid("charset out of range".to_owned()))?;
    let mut gid = 1_usize;
    let mut position = offset + 1;
    match format {
        0 => {
            while gid < cid_of_gid.len() {
                let cid = read_u16(data, position)?;
                cid_of_gid[gid] = cid;
                gid += 1;
                position += 2;
            }
        }
        1 | 2 => {
            while gid < cid_of_gid.len() {
                let first = read_u16(data, position)?;
                position += 2;
                let left = if format == 1 {
                    let value = *data
                        .get(position)
                        .ok_or_else(|| CffError::Invalid("charset truncated".to_owned()))?;
                    position += 1;
                    u16::from(value)
                } else {
                    let value = read_u16(data, position)?;
                    position += 2;
                    value
                };
                for index in 0..=usize::from(left) {
                    if gid >= cid_of_gid.len() {
                        break;
                    }
                    cid_of_gid[gid] = first.wrapping_add(index as u16);
                    gid += 1;
                }
            }
        }
        other => return Err(CffError::Invalid(format!("charset format {other}"))),
    }
    Ok(())
}

fn read_u16(data: &[u8], position: usize) -> Result<u16, CffError> {
    let bytes = data
        .get(position..position + 2)
        .ok_or_else(|| CffError::Invalid("truncated font data".to_owned()))?;
    Ok(u16::from_be_bytes([bytes[0], bytes[1]]))
}

/// Reads one CFF INDEX at `position`; returns its items and the position after it.
fn read_index(data: &[u8], position: usize) -> Result<(Vec<&[u8]>, usize), CffError> {
    let count = usize::from(read_u16(data, position)?);
    let mut position = position + 2;
    if count == 0 {
        return Ok((Vec::new(), position));
    }
    let offset_size = *data
        .get(position)
        .ok_or_else(|| CffError::Invalid("truncated INDEX".to_owned()))?;
    position += 1;
    if !(1..=4).contains(&offset_size) {
        return Err(CffError::Invalid("invalid INDEX offset size".to_owned()));
    }
    let mut offsets = Vec::with_capacity(count + 1);
    for _ in 0..=count {
        let end = position + usize::from(offset_size);
        let bytes = data
            .get(position..end)
            .ok_or_else(|| CffError::Invalid("truncated INDEX offsets".to_owned()))?;
        let mut value = 0_usize;
        for byte in bytes {
            value = (value << 8) | usize::from(*byte);
        }
        offsets.push(value);
        position = end;
    }
    let base = position
        .checked_sub(1)
        .ok_or_else(|| CffError::Invalid("invalid INDEX".to_owned()))?;
    let mut items = Vec::with_capacity(count);
    for index in 0..count {
        let start = base
            .checked_add(offsets[index])
            .ok_or_else(|| CffError::Invalid("invalid INDEX offset".to_owned()))?;
        let end = base
            .checked_add(offsets[index + 1])
            .ok_or_else(|| CffError::Invalid("invalid INDEX offset".to_owned()))?;
        items.push(
            data.get(start..end)
                .ok_or_else(|| CffError::Invalid("INDEX item out of range".to_owned()))?,
        );
    }
    let end = base
        .checked_add(*offsets.last().unwrap_or(&0))
        .ok_or_else(|| CffError::Invalid("invalid INDEX end".to_owned()))?;
    Ok((items, end))
}

/// Parses a CFF DICT into operator -> operands.
fn parse_dict(data: &[u8]) -> Result<HashMap<u16, Vec<f64>>, CffError> {
    let mut result: HashMap<u16, Vec<f64>> = HashMap::new();
    let mut operands: Vec<f64> = Vec::new();
    let mut position = 0_usize;
    while position < data.len() {
        let byte = data[position];
        match byte {
            0..=21 => {
                let operator = if byte == 12 {
                    let next = *data.get(position + 1).ok_or_else(|| {
                        CffError::Invalid("truncated two-byte operator".to_owned())
                    })?;
                    position += 2;
                    u16::from(next) | 0x0C00
                } else {
                    position += 1;
                    u16::from(byte)
                };
                result.insert(operator, std::mem::take(&mut operands));
            }
            28 => {
                let bytes = data
                    .get(position + 1..position + 3)
                    .ok_or_else(|| CffError::Invalid("truncated DICT number".to_owned()))?;
                operands.push(f64::from(i16::from_be_bytes([bytes[0], bytes[1]])));
                position += 3;
            }
            29 => {
                let bytes = data
                    .get(position + 1..position + 5)
                    .ok_or_else(|| CffError::Invalid("truncated DICT number".to_owned()))?;
                operands.push(f64::from(i32::from_be_bytes([
                    bytes[0], bytes[1], bytes[2], bytes[3],
                ])));
                position += 5;
            }
            30 => {
                // Real number: nibbles, terminated by 0xf.
                position += 1;
                while position < data.len() {
                    let byte = data[position];
                    position += 1;
                    if (byte & 0x0F) == 0x0F || (byte >> 4) == 0x0F {
                        break;
                    }
                }
                operands.push(0.0);
            }
            32..=246 => {
                operands.push(f64::from(byte) - 139.0);
                position += 1;
            }
            247..=250 => {
                let next = *data
                    .get(position + 1)
                    .ok_or_else(|| CffError::Invalid("truncated DICT number".to_owned()))?;
                operands.push(f64::from((u16::from(byte) - 247) * 256 + u16::from(next)) + 108.0);
                position += 2;
            }
            251..=254 => {
                let next = *data
                    .get(position + 1)
                    .ok_or_else(|| CffError::Invalid("truncated DICT number".to_owned()))?;
                operands.push(-(f64::from(byte - 251) * 256.0 + f64::from(next) + 108.0));
                position += 2;
            }
            _ => {
                return Err(CffError::Invalid(format!("unexpected DICT byte {byte:#x}")));
            }
        }
    }
    Ok(result)
}

/// Wraps a bare CFF table in a minimal OpenType container.
///
/// `advances` holds one advance width per glyph in 1000-unit em space; missing entries fall back
/// to 1000. `bbox` is the font bounding box in the same units.
pub fn wrap_otf(
    cff: &BareCff,
    data: &[u8],
    advances: &[u16],
    bbox: [i16; 4],
) -> Result<Vec<u8>, CffError> {
    let glyphs = cff.glyphs.max(1);
    let mut tables: Vec<(&[u8; 4], Vec<u8>)> = Vec::new();

    let mut head = vec![0_u8; 54];
    write_u32(&mut head, 0, 0x0001_0000);
    write_u32(&mut head, 4, 0x0001_0000);
    write_u32(&mut head, 12, 0x5F0F_3CF5);
    write_u16(&mut head, 16, 0x000B);
    write_u16(&mut head, 18, 1000);
    for (index, value) in bbox.iter().enumerate() {
        write_i16(&mut head, 36 + index * 2, *value);
    }
    write_u16(&mut head, 44, 0);
    write_u16(&mut head, 46, 8);
    write_i16(&mut head, 48, 2);
    write_i16(&mut head, 50, 0);
    write_i16(&mut head, 52, 0);
    tables.push((b"head", head));

    let mut maxp = vec![0_u8; 6];
    write_u32(&mut maxp, 0, 0x0000_5000);
    write_u16(&mut maxp, 4, glyphs);
    tables.push((b"maxp", maxp));

    let ascent = bbox[3];
    let descent = bbox[1];
    let advance_max = advances.iter().copied().max().unwrap_or(1000);
    let mut hhea = vec![0_u8; 36];
    write_u32(&mut hhea, 0, 0x0001_0000);
    write_i16(&mut hhea, 4, ascent);
    write_i16(&mut hhea, 6, descent);
    write_u16(&mut hhea, 10, advance_max);
    write_i16(&mut hhea, 12, 0);
    write_i16(&mut hhea, 14, advance_max.min(i16::MAX as u16) as i16);
    write_i16(&mut hhea, 16, bbox[2]);
    write_i16(&mut hhea, 18, 1);
    write_i16(&mut hhea, 20, 0);
    write_i16(&mut hhea, 22, 0);
    write_u16(&mut hhea, 34, glyphs);
    tables.push((b"hhea", hhea));

    let mut hmtx = vec![0_u8; usize::from(glyphs) * 4];
    for glyph in 0..usize::from(glyphs) {
        let advance = advances.get(glyph).copied().unwrap_or(1000);
        write_u16(&mut hmtx, glyph * 4, advance);
        write_i16(&mut hmtx, glyph * 4 + 2, 0);
    }
    tables.push((b"hmtx", hmtx));

    let mut cmap = vec![0_u8; 36];
    write_u16(&mut cmap, 0, 0);
    write_u16(&mut cmap, 2, 1);
    write_u16(&mut cmap, 4, 3);
    write_u16(&mut cmap, 6, 1);
    write_u32(&mut cmap, 8, 12);
    let subtable = 12_usize;
    write_u16(&mut cmap, subtable, 4);
    write_u16(&mut cmap, subtable + 2, 24);
    write_u16(&mut cmap, subtable + 4, 0);
    write_u16(&mut cmap, subtable + 6, 2);
    write_u16(&mut cmap, subtable + 8, 2);
    write_u16(&mut cmap, subtable + 10, 0);
    write_u16(&mut cmap, subtable + 12, 0);
    write_u16(&mut cmap, subtable + 14, 0xFFFF);
    write_u16(&mut cmap, subtable + 16, 0);
    write_u16(&mut cmap, subtable + 18, 0xFFFF);
    write_u16(&mut cmap, subtable + 20, 1);
    write_u16(&mut cmap, subtable + 22, 0);
    tables.push((b"cmap", cmap));

    let mut name = vec![0_u8; 6];
    write_u16(&mut name, 0, 0);
    write_u16(&mut name, 2, 0);
    write_u16(&mut name, 4, 6);
    tables.push((b"name", name));

    let mut os2 = vec![0_u8; 78];
    write_u16(&mut os2, 0, 0);
    write_i16(&mut os2, 2, 500);
    write_u16(&mut os2, 4, 400);
    write_u16(&mut os2, 6, 5);
    write_u16(&mut os2, 62, 0x0040);
    write_u16(&mut os2, 64, 0x0020);
    write_u16(&mut os2, 66, 0xFFFF);
    write_i16(&mut os2, 68, ascent);
    write_i16(&mut os2, 70, descent);
    write_i16(&mut os2, 72, 0);
    write_u16(&mut os2, 74, u16::try_from(ascent.max(0)).unwrap_or(800));
    write_u16(&mut os2, 76, descent.min(0).unsigned_abs());
    tables.push((b"OS/2", os2));

    let mut post = vec![0_u8; 32];
    write_u32(&mut post, 0, 0x0003_0000);
    tables.push((b"post", post));

    tables.push((b"CFF ", data.to_vec()));

    // Assemble the table directory sorted by tag, as required by the SFNT format.
    tables.sort_by(|left, right| left.0.cmp(right.0));
    let count = tables.len() as u16;
    let mut directory = vec![0_u8; 12 + usize::from(count) * 16];
    write_u32(&mut directory, 0, 0x4F54_544F); // 'OTTO'
    write_u16(&mut directory, 4, count);
    let mut power = 1_u16;
    let mut selector = 0_u16;
    while power * 2 <= count {
        power *= 2;
        selector += 1;
    }
    write_u16(&mut directory, 6, power * 16);
    write_u16(&mut directory, 8, selector);
    write_u16(&mut directory, 10, count * 16 - power * 16);
    let mut offset = directory.len();
    for (index, (tag, table)) in tables.iter().enumerate() {
        let record = 12 + index * 16;
        directory[record..record + 4].copy_from_slice(*tag);
        write_u32(&mut directory, record + 4, 0);
        write_u32(&mut directory, record + 8, offset as u32);
        write_u32(&mut directory, record + 12, table.len() as u32);
        offset += (table.len() + 3) & !3;
    }
    let mut output = directory;
    for (_, table) in &tables {
        output.extend_from_slice(table);
        while !output.len().is_multiple_of(4) {
            output.push(0);
        }
    }
    Ok(output)
}

fn write_u16(data: &mut [u8], offset: usize, value: u16) {
    data[offset..offset + 2].copy_from_slice(&value.to_be_bytes());
}

fn write_i16(data: &mut [u8], offset: usize, value: i16) {
    data[offset..offset + 2].copy_from_slice(&value.to_be_bytes());
}

fn write_u32(data: &mut [u8], offset: usize, value: u32) {
    data[offset..offset + 4].copy_from_slice(&value.to_be_bytes());
}

#[cfg(test)]
mod tests {
    use super::{parse_bare_cff, wrap_otf};

    const FIXTURE: &[u8] = include_bytes!("../tests/fixtures/cff-cid-subset.bin");

    #[test]
    fn parses_cid_cff_subset() {
        let font = parse_bare_cff(FIXTURE).expect("parse CFF");
        assert_eq!(font.glyphs, 2);
        assert!(!font.cid_to_gid.is_empty());
        assert!(font.code_to_gid.is_empty());
    }

    #[test]
    fn parses_type1_cff_encoding() {
        let data = include_bytes!("../tests/fixtures/cff-type1-subset.bin");
        let font = parse_bare_cff(data).expect("parse Type1C");
        assert_eq!(font.glyphs, 35);
        assert_eq!(font.code_to_gid.get(&33), Some(&1));
        assert_eq!(font.code_to_gid.get(&66), Some(&34));
        let wrapped = wrap_otf(
            &font,
            data,
            &vec![1000; usize::from(font.glyphs)],
            [-500, -250, 1500, 1000],
        )
        .expect("wrap");
        let face = ttf_parser::Face::parse(&wrapped, 0).expect("parse wrapped font");
        assert_eq!(face.number_of_glyphs(), 35);
    }

    #[test]
    fn wraps_cff_into_a_parseable_opentype_font() {
        let font = parse_bare_cff(FIXTURE).expect("parse CFF");
        let wrapped = wrap_otf(&font, FIXTURE, &[484, 484], [-117, -117, 351, 351]).expect("wrap");
        let face = ttf_parser::Face::parse(&wrapped, 0).expect("parse wrapped font");
        assert_eq!(face.number_of_glyphs(), font.glyphs);
        assert_eq!(face.units_per_em(), 1000);
    }
}
