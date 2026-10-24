# Flow → Fixed 布局契约（A0）

更新日期：2026-10-24
范围：`ofdbox-folio-layout` 的输入输出边界、排版规则与 DOCX 映射范围。本文冻结
A1 实现的范围；标记「A2」的条目留给后续里程碑。

## 1. 方向与职责

- 只提供 `FlowDocument → FixedDocument`，不反向、不提供 Fixed IR 编辑 API。
- 布局只消费 IR：不解析文件、不访问文件系统、不渲染位图。字体通过
  `FontProvider` 注入，容器与格式差异留在各自的适配层。
- 布局结果必须是确定的：相同输入 + 相同字体字节 → 相同输出字节。
- 内建诊断走 `Report`：缺字体、无法断行、图片超宽、字段无法解析等都要记录，
  不允许静默产生错误坐标。

## 2. 单位与坐标

| 项目 | 约定 |
| --- | --- |
| 输入长度 | twip（1/1440 英寸） |
| 输出长度 | 毫米（`f64`） |
| 原点 | 页面左上角，y 向下 |
| 文本 | 字形坐标以基线为原点；`Glyph::x/y` 为局部坐标，`advance` 为到下一字形的步进 |
| 页面盒 | `PageInfo.media_box == crop_box == (0, 0, w, h)`，`rotation = None` |

换算：`mm = twip * 25.4 / 1440`。布局内部用毫米浮点计算，只在写回 IR 时取整到
`1e-4 mm`，避免累积误差。

## 3. 盒模型

```text
页面 ─ 页边距 ─► 内容区（宽 W）
  块级元素依次纵向排布：段落 / 图片 / 分页符
  段落 = 左缩进 + 可用宽度 + 右缩进
  行盒 = 行内盒水平排列，行高由行距规则决定
```

- 内容区宽度 `W = page.width - margins.left - margins.right`（负值按 0 处理并记诊断）。
- 段落可用宽度 `Wp = W - left_indent - right_indent`；首行额外减去 `first_line_indent`。
- 竖直游标 `y` 从 `margins.top` 开始；`y + 行高 > page.height - margins.bottom`
  时换页（A1 不做页脚占位）。
- 段前/段后间距：只在同一页内生效；换页丢弃段前间距（避免页首空白）。

## 4. 行布局

1. 把段落的行内内容展开成「测量单元」序列：每个字符一个单元，记录宽度、样式、
   来源 run 索引；行内图片是一个不可断单元。
2. 测量：`glyph_index(ch)` 取不到字形时记诊断并按 .notdef 宽度处理；宽度 =
   `advance_units * size / units_per_em`，不含字距调整（A1 不做 kerning/ligature）。
3. 断行优先级：
   - 显式换行 `\n` 强制断行；
   - CJK（含全角标点）在任意字符之间可断；
   - 拉丁文本在空格、连字符后可断，空格悬挂在行尾、不计入行宽；
   - 禁止行首字符（`，。、；：？！）》」』%` 等）向前吞一个字符；禁止行尾字符
     （`（《「『`）向后带一个字符；
   - 单个单元宽度超过行宽时独占一行、不缩放也不溢出裁剪（A2 再定缩放规则）。
4. 行高（毫米）：
   - `LineSpacing::Single` → `1.2 * 行内最大字号`；
   - `Multiple(f)` → `f * 1.2 * 最大字号`；
   - `Exact(t)` → `t`；
   - `AtLeast(t)` → `max(t, 1.2 * 最大字号)`。
   基线在行盒内位置：`baseline = top + 0.8 * 最大字号`（首行不再额外抬高）。
5. 对齐：
   - Left/Center/Right 按整行宽度平移；
   - Justify 优先拉伸行内空格；整行没有空格时把余量均摊到字间（中文行）；
     最后一行按 Left；
   - 拉伸后允许单词间距为负到 0（不压缩字形）。

## 5. 分页

- 段落按行跨页：一行放不下就换页续排，不做孤行/寡行控制（A2 仍未做）。
- `Block::PageBreak` 立即换页；页尾空白保留。
- 段落在页首时省略 `space_before`。
- 表格按行跨页：整行放不下就换页，换页后重排 `Table::header_rows` 指定的表头行；
  单行高于整页时允许溢出并记诊断（不做行内拆分）。
- A1 未实现的块（`Toc`/未知类型）跳过并记 `DiagnosticCode::Geometry` 诊断。
- A2 补充：`keep_with_next`、`keep_lines_together`、孤行/寡行控制仍未做；表格行内拆分不计划支持。

### 5.1 表格

- 列宽：`ColumnWidth::Twips` 固定值，`ColumnWidth::Weight` 按剩余宽度分配；总和超过
  版心时等比缩到版心宽。
- 单元格：内容在「列宽 − 左右内边距」的子框里排版，子框内不分页；行高取各单元格
  内容高度与 `Row::height` 的较大值。
- 边框：每个单元格画四条边。表格最外圈用 `TableBorders::outer`，内部横线用
  `horizontal`、竖线用 `vertical`；相邻单元格的公共边会重画一次（同色同宽，视觉无差异）。
- 单元格内可以有嵌套表格，递归使用同一套子框逻辑。

### 5.2 页眉页脚

- 页眉从上边距上方的 `header_distance` 处向下排，底边界是上边距；内容超出上边距时
  记诊断（会与正文重叠）。
- 页脚先量高度，再贴着「页高 − `footer_distance`」向上排，因此页脚内容多高都贴底。
- 页眉页脚取当前分节的 `HeaderFooterSet`：分节首页用 `first`（存在时），偶数页用
  `even`（存在时），其余用 `default`。
- 页眉页脚里的 `PAGE`/`NUMPAGES` 字段与正文用同一套页码；引用总页数时整篇排两遍，
  第二遍代入第一遍的页数。
- 只有正文的页面才产出：空页（例如连续两个分页符）不留空白页，也不单独产出页眉页脚。

### 5.3 自动目录

- `Block::Toc` 展开成「标题 + 两列无边框表格」：左列目录条目（按层级左缩进、
  可在单元格宽度内折行），右列页码（右对齐）。
- 条目与页码来自上一遍排版登记的标题；`layout_with` 先空跑一遍拿到标题页码，
  再代入目录重排，直到目录条目与总页数稳定（最多 4 遍）。
- 目录条目文字用的是排版时生成的段落，字体在 `prepare_fonts` 里按全文字符集预解析，
  目录标题的字符也会被算进去。
- 没有二级引导点线（tab leader）：右侧页码靠列对齐，而不是「……. 12」的填充。

## 6. 样式解析

- 段落：`style_id` 命中的命名样式沿 `based_on` 链（最多 8 层、检测环）作为底，
  `Paragraph.style` 里**非默认值**作为直接格式覆盖；没有 `style_id` 时底是
  `StyleSheet.default_paragraph`。基础样式里显式写成默认值的字段会被直接格式
  覆盖，这是已知的近似。
- 字符：`TextRun.style` + `TextRun.style_id` 同理，底来自 `StyleSheet.character`
  或 `StyleSheet.default_character`。
- A1 不做「段落样式继承字符样式」的组合规则，A3 接入 DOCX 时再定。

## 7. 字段

- `FieldKind::PageNumber`：所在页的页码（1 基）。
- `FieldKind::TotalPages`：两遍布局——第一遍统计总页数，第二遍代入。
- `FieldKind::Date`：用 `cached_text`，缺失时渲染为空并记诊断。
- `FieldKind::Ref`：A1 渲染 `cached_text`；锚点解析用文档顺序中的标题锚点。

## 8. 图片

- 固有尺寸（毫米）= 像素 / dpi × 25.4；`ImageAsset.dpi` 缺省 96。
- `ImageBlock`：宽度取 `size` 或固有宽度；超过内容区宽度时等比缩到内容区宽并记诊断。
  高度超过可用区域时同样等比缩放（A1），仍放不下则换页。
- `InlineImage`：作为不可断单元参与行布局，按行高基线对齐（底部贴基线）。
- 行内图片的高度不计入行高（A1）；高于行高时记诊断。

## 9. 标题与大纲

- `Paragraph.heading_level = Some(level)` 在布局完成后生成 `OutlineItem`：
  标题文本取段落纯文本，层级按 `level` 嵌套（同层或更浅时回退到对应父级）。
- 标题被分页时，大纲目标指向该标题首行所在页。
- `anchor` 锚点记入文档顺序索引，供 `FieldKind::Ref` 与链接使用（A1 只有解析）。

## 10. 输出映射

| Flow 概念 | Fixed IR 落点 |
| --- | --- |
| 段落行 | `Element::Text(TextElement)`，一段一个 run（样式相同可合并） |
| 文本层 | `TextItem`（run + 变换 + bbox + 字体 + 字号）与 `TextLine`（items 范围 + bbox + baseline） |
| 行内/块级图片 | `Element::Image(ImageElement)`，`transform` 为毫米缩放 |
| 分页 | 一页一个 `Page`，`PageInfo` 用页面设置 |
| 分页符 | 结束当前页，开始新页 |
| 超链接 | `Page.links`（A1 记录 URI 与锚点区域） |
| 大纲 | `FixedDocument.outline` |

字体资源：每个用到的字体字节在 `FixedDocument.resources` 中登记为 `FontAsset`，
`TextItem.font` 与 `TextElement.font` 引用同一个 `Arc`。

## 11. DOCX 支持矩阵（A3 目标）

| DOCX 特性 | A3 | 说明 |
| --- | --- | --- |
| 段落 / 字符样式、加粗斜体下划线删除线、颜色、字号 | 是 | `styles.xml` + `basedOn`，读入时拍平成直接格式 |
| 对齐、缩进、行距、段前段后 | 是 | |
| 分节（页面大小/边距/分栏） | 单栏 | 多栏归 A4 |
| 表格（列宽、边框、表头行） | 读入是，落版待 A2 | 合并单元格按普通单元格并记诊断 |
| 图片（内联/浮动） | 是 | 浮动图按内联降级并记诊断 |
| 页眉页脚、页码字段 | 读入是，落版待补 | 页眉页脚进 `HeaderFooterSet`，布局尚未排入页面 |
| 自动目录（TOC 域） | 是 | 内容控件映射成 `TocBlock`，等 A2 落版 |
| 列表 | 部分 | 标记文本 + 悬挂缩进；多级编号与自定义符号是近似 |
| 脚注/尾注、批注、表单控件、SmartArt、VML、嵌入对象 | 否 | 记 `Unsupported` 诊断 |

A3 的样式策略：读入时按 XML 的**出现与否**逐字段层叠（`docDefaults` → 默认样式 →
`basedOn` 链 → 直接格式），结果拍平进 `Paragraph.style` / `TextStyle`，不依赖 IR 的
命名样式链。这样 `<w:ind w:firstLine="0"/>` 这类「显式写回默认值」也能覆盖样式
里的非零值，代价是 `FlowDocument.styles` 为空——只有代码构造的 Flow 文档才会用到
命名样式。

## 12. 验收

- A1：构造一个含标题、正文、两端对齐、行内图片与分页的 Flow 文档，
  `layout` 输出 N 页；`ofd_to_png` 渲染结果与预期一致；文本层可被搜索与选择；
  标题生成大纲且页码正确。
- A2：表格跨页重复表头、自动目录页码回填。当前状态：表格（列宽、单元格子框、边框、
  跨页重排表头）、页眉页脚（含页码字段、贴底页脚）与自动目录（两遍回填页码）均已完成。
- A3：真实 DOCX 转 OFD 后逐页渲染对比 Word 导出 PDF。当前状态：`sample.docx`
  （标题、多格式正文、列表、表格、图片、页脚页码）读入 17 块、排版 2 页、0 条缺字
  诊断，页内文字与 Word 版式一致；表格（A2）、页眉页脚落版仍是空白。
