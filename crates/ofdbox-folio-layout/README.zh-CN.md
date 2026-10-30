[English](README.md) | 简体中文

# ofdbox-folio-layout

`ofdbox-folio-layout` 是 OFDBox Folio 的单向排版引擎。它接收 `FlowDocument` 与调用方提供的 `FontProvider`，产出分页后的 `FixedDocument`；排版是 Flow IR 到 Fixed IR 的唯一通路。它实现的规则冻结在[布局契约](CONTRACT.zh-CN.md)中，它在 crate 图中的位置见[架构文档](../../docs/architecture.zh-CN.md)。

## 单向方向

```text
FlowDocument ──► layout() / layout_with() ──► FixedDocument ──► 写出器 / 渲染器
```

- 只提供 Flow → Fixed：不反向，也不提供 Fixed IR 编辑 API。
- 布局只消费 IR。它不解析文件、不访问文件系统、不渲染位图；字体通过 `FontProvider` 注入，容器与格式差异留在各自的适配层。
- 布局结果确定：相同输入 + 相同字体字节 + 相同选项 → 相同输出字节。
- 问题一律记录，不被隐藏。缺字体、无法断行、图片超宽、字段无法解析等都会进入 `Report` 诊断，不允许静默产生错误坐标。

## 入口

### `layout`

`layout(document, fonts)` 是简写形式，按 `LayoutOptions::default()` 排版，只在文档需要时追加额外遍次。

### `layout_with`

`layout_with(document, fonts, options)` 是完整入口。文档没有分节时返回 `Err(LayoutError::NoSection)`；文档需要时它会追加额外遍次：

- 先空跑一遍统计总页数，供 `FieldKind::TotalPages` 在下一遍代入；
- 自动目录先空跑收集标题页码，再重新排版，直到目录条目与总页数稳定，最多四遍。

```rust
use ofdbox_folio_layout::{layout, layout_with, LayoutOptions};

let fixed = layout(&flow, &mut fonts)?;

let options = LayoutOptions {
    default_image_dpi: 96.0,
    endnote_title: "Endnotes".to_owned(),
};
let fixed = layout_with(&flow, &mut fonts, options)?;
```

### `LayoutOptions`

| 字段 | 类型 | 默认值 | 含义 |
| --- | --- | --- | --- |
| `default_image_dpi` | `f64` | `96.0` | 图片未声明 DPI 时假定的分辨率。 |
| `endnote_title` | `String` | `"Endnotes"` | 尾注页上打印的标题。 |

## 排版范围

- **断行**：CJK 边界、空格与连字符都可断行；行首/行尾禁则把标点回吞；行尾空格不占行宽。
- **段落**：首行/左右缩进；单倍、多倍、固定与最小行距；段前段后间距；左、居中、右与两端对齐。
- **行高与字体回退**：单倍行距取字体 `hhea` 的 ascent、descent 与 lineGap，基线按 ascent 定位；缺字形时逐字回退到兜底字体。
- **分页与分栏**：按行分页，段前间距在页首丢弃，`PageSetup::columns` 按等宽分栏流式排布；孤行/寡行控制保证每页至少两行。
- **图片**：块级图片按声明尺寸或 DPI 换算，超宽时等比缩放并记录诊断；行内图片是不可断单元，底部贴基线。
- **大纲**：`heading_level` 生成 `OutlineItem` 树，目标页与 y 坐标取自标题首行。
- **表格**：支持固定与权重列宽、单元格子框、按行跨页并重复表头、单行高于剩余空间时拆分续排、逐格边框。
- **页眉页脚**：页眉从上边距上方往下排，页脚先量高再贴底；支持首页不同与偶数页变体，`PAGE`/`NUMPAGES` 与正文共用同一套页码。
- **浮动对象**：`Block::Floating` 按页面坐标绝对定位，支持不绕排、上下型与四周型绕排。
- **制表位**：支持左、居中、右与小数点对齐，前导符可为点线、虚线或实线；没有显式制表位时按 12.5 mm 格点前进。
- **样式**：命名样式沿 `based_on` 链继承，非默认的直接格式覆盖它们。

## 脚注、尾注与目录

### 脚注与尾注

- `Inline::Note` 是正文中的引用。注文按出现顺序编号（脚注 1、2…；尾注 1、2…），标记以同行的小号数字排布。
- 脚注在引用页底部预留空间：引用行落版后，正文下边界立即抬高，注文排队；到页尾时，在页脚上方、正文下方画一条版心宽度三分之一的水平线，然后按编号顺序以 8 mm 左缩进排版注文。
- 注文把正文挤过页面底部时，已经落版的行不会回退；后续行自动移到下一栏或下一页。
- 尾注不占页底。它们收集到文档末尾，从单独一页开始，先排由 `LayoutOptions::endnote_title`（默认 `"Endnotes"`）给出的标题，再按编号顺序排布。
- 找不到注文时记录 `DiagnosticCode::Unsupported` 诊断并跳过该注。

### 自动目录

- `Block::Toc` 展开为一个标题加上每条目一段：条目文字、制表符、页码。
- 条目按层级左缩进，右制表位落在版心右边缘，因此页码右对齐；启用 `TocStyle::dot_leader` 时用点线前导符填满间隙，与 Word 目录的观感一致。
- 页码取自空跑阶段登记的标题；`layout_with` 代入后重新排版，直到条目与总页数稳定，最多四遍。
- 目录条目的字体按全文档字符集预解析，因此生成的条目文字不会被画成空白方块。

## 已知近似

- 纵向合并单元格的高度取自起始单元格所在行；行不会跨该边界拉伸。
- 基础样式中显式写出的默认值会被直接格式覆盖。
- 没有空格的两端对齐行把余量均摊到字间；拉伸后的词间距可能为负，字形从不压缩。
- 使用 `PositionFrom::Paragraph` 的浮动对象按锚定到版心处理，在单栏排版中与 `Margin`/`Column` 相同。
- 表格单元格或页眉页脚内的浮动对象按普通块排版，并记录诊断。
- 孤行/寡行控制是「每页至少两行」，而非 Word 的精确规则。
- 不做 kerning，也不做 ligature：宽度为 `advance * size / units_per_em`，缺字形时按 `.notdef` 宽度处理。
- 字体度量无法解析时，行高回退到 `1.2 × 字号`。
- 表格行的固定行高要等整行排完才生效。
- 引擎未处理的块类型会被跳过，并记录 `DiagnosticCode::Geometry` 诊断。

## 延伸阅读

- [布局契约](CONTRACT.zh-CN.md)：冻结的 A0 范围、单位、盒模型、行布局、分页与输出映射。
- [架构](../../docs/architecture.zh-CN.md)：crate 分工，以及排版如何衔接两种 IR。
- [项目 README](../../README.zh-CN.md)：目标、当前状态与用法。

## 许可

双许可，MIT 或 Apache-2.0，任选其一：[LICENSE-MIT](../../LICENSE-MIT) · [LICENSE-APACHE](../../LICENSE-APACHE)。
