//! 字体提供者契约与 TrueType 子集化。
//!
//! 格式适配器和布局引擎通过本 trait 获取缺失的字体字节。库本身不扫描
//! 系统字体，调用方可以注入系统字体、目录索引或远程字体服务。

use std::collections::HashSet;
use std::sync::Arc;

/// 字体加载请求。
#[derive(Clone, Debug, Default)]
pub struct FontRequest<'a> {
    /// 源格式中的字体资源 ID。
    pub font_id: Option<&'a str>,
    /// 字体名称。
    pub font_name: Option<&'a str>,
    /// 字体家族名。
    pub family_name: Option<&'a str>,
    /// 字符集。
    pub charset: Option<&'a str>,
    /// 是否粗体。
    pub bold: bool,
    /// 是否斜体。
    pub italic: bool,
    /// 是否衬线体。
    pub serif: bool,
    /// 是否等宽体。
    pub fixed_width: bool,
    /// 源文件声明的字体文件路径。
    pub font_file: Option<&'a str>,
}

/// 字体提供者。
pub trait FontProvider {
    /// 返回字体文件字节；找不到时返回 `None`。
    fn load(&mut self, request: &FontRequest<'_>) -> Result<Option<Arc<[u8]>>, FontError>;
}

/// 字体提供者错误。
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[error("字体加载失败: {0}")]
pub struct FontError(pub String);

impl FontError {
    /// 创建错误。
    pub fn new(message: impl Into<String>) -> Self {
        Self(message.into())
    }
}

/// 把 TrueType 集合（`.ttc`）里的某个 face 拆成独立字体。
///
/// Windows 的宋体、微软雅黑都是集合：OFD 的 `FontFile` 只能放单份字体，按
/// `face_index` 提取后才能内嵌与子集化。普通 TTF 或 `face_index` 越界时返回
/// `None`，调用方继续用原字节。
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

/// 按用到的字形裁剪 TrueType 字体。
///
/// 保留字形 ID 与 `cmap`/`hmtx` 等表，只把未引用的轮廓清空，因此引用原始 GID
/// 的文本（PDF 的 Identity-H、OFD 的 `CGTransform`）仍然有效；复合字形引用的
/// 子字形会一并保留。
///
/// 集合字体先按 `face_index` 拆出单个 face；不是 glyf 轮廓的 TrueType（真身
/// CFF/OTF）或结构不完整时返回 `None`，调用方应回退到原始字节。
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
        // 单个字形的结构异常不该让整份字体放弃子集化，跳过即可。
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
            // 字形名对渲染无用，选集里改成 version 3 可以省下几百 KB 到几 MB。
            b"post" if body.len() >= 32 => {
                let mut post = body[..32].to_vec();
                post[0..4].copy_from_slice(&0x0003_0000_u32.to_be_bytes());
                *body = post;
            }
            _ => {}
        }
    }
    // 位图、彩色字形与数字签名等表对矢量渲染无用，直接丢掉。
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

/// 子集化时丢弃的表：位图字形、彩色字形、数字签名与调优数据。
///
/// 保留 `cmap`/`hmtx`/`name` 等文本必需的表；丢掉 `EBDT`/`EBLC` 这类内嵌点阵
/// （SimSun 有几 MB）后，子集体积才真正跟着用字量走。
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

    /// 造一份只含 head/maxp/loca/glyf 的最小 TrueType。
    ///
    /// 字形 0 为空，字形 1/2 各 16 字节空轮廓数据（长度足够定位，不要求可渲染）。
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
        // indexToLocFormat = 0（短偏移）。
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
        assert!(subset.len() < font.len(), "子集应更小");
        // 结构仍可解析：前 12 字节表头保留，表数量一致。
        assert_eq!(&subset[0..4], &font[0..4]);
        assert_eq!(u16::from_be_bytes([subset[4], subset[5]]), 4);
    }

    #[test]
    fn refuses_fonts_it_cannot_subset() {
        let used = HashSet::from([1_u16]);
        assert!(subset_truetype(b"", 0, &used).is_none());
        assert!(subset_truetype(b"ttcf\x00\x01\x00\x00", 0, &used).is_none());
        let font = tiny_font(4);
        // 集合字体的第 2 个 face 无法安全裁剪。
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
            let face = extract_truetype_face(&data, 0).expect("拆 face");
            assert!(
                face.len() < data.len(),
                "face {} >= ttc {}",
                face.len(),
                data.len()
            );
            let subset = subset_truetype(&data, 0, &used).expect("集合字体子集化");
            assert!(
                subset.len() < face.len(),
                "子集 {} >= face {}",
                subset.len(),
                face.len()
            );
        } else {
            let subset = subset_truetype(&data, 0, &used).expect("子集化");
            assert!(subset.len() < data.len());
        }
    }
}
