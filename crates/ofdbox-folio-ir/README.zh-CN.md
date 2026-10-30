[English](README.md) | 简体中文

# ofdbox-folio-ir

`ofdbox-folio-ir` 是 OFDBox Folio 的格式中立核心：它定义 Flow IR 与 Fixed IR、各 crate 共用的几何/路径/画笔/资源类型、文本层，以及 `DocumentSource` Reader 契约。本 crate 不依赖任何文档格式库，也不做任何 I/O，因此 OFD、PDF、DOCX 适配器、排版引擎与渲染器可以共用同一套数据模型。crate 分工与全部契约见[架构文档](../../docs/architecture.zh-CN.md)。

## 在工作区中的位置

```text
ofdbox-folio-ofd     ──► ofdbox-folio-ir
ofdbox-folio-pdf     ──► ofdbox-folio-ir
ofdbox-folio-docx    ──► ofdbox-folio-ir
ofdbox-folio-layout  ──► ofdbox-folio-ir
ofdbox-folio-render  ──► ofdbox-folio-ir
```

依赖方向不会反转：`ofdbox-folio-ir` 不依赖格式库、不依赖渲染后端、不访问文件系统，`ofdbox-folio` 门面则位于它们之上。

## 两种 IR

### Flow IR

`FlowDocument` 是语义化、可编辑的文档模型：分节、段落与行内 run、标题、表格、图片、字段、目录、页眉页脚、脚注尾注。它记录的是意图——一个标题、一个居中段落、一个分页符——而不是位置，也从不携带绝对坐标。

### Fixed IR

`FixedDocument` 是不可变、可缓存的固定版式页面模型，由排版产出或由读取固定版式格式得到。每个 `Page` 携带：

- `Scene`：页面内容树（`Text`、`Image`、`Path`、`Group`、`Clip`），供渲染与写出消费；
- `TextLayer`：按逻辑阅读顺序排列的文本，以及字符/字形簇映射，供选择、复制与搜索；
- `PageInfo`：media/crop box、旋转与页码标签；
- 链接区域与模板层。

文档保留页面共享的部分（`resources`、`templates`、`outline`、`metadata`、`report`、`engine`），页面本身仍可独立加载与缓存。`FixedDocument` 是排版的完整输出；`Page` 是阅读器的加载单位。

## 单位与坐标

| 模型 | 长度单位 | 表示方式 |
| --- | --- | --- |
| Flow IR | twip（1/1440 英寸） | 整数 `Twips` |
| Fixed IR | 毫米 | `f64` |

- Flow IR 中的一切都是 twip：字号、缩进、行距、页边距与页面尺寸。`Twips` 是整数 newtype，`from_pt`/`from_mm` 在入口处取整，因此经 pt 或毫米往返不会累积漂移。
- Fixed IR 使用毫米：原点在页面左上角，x 向右、y 向下。字形坐标以基线为原点，`Glyph::advance` 是到下一字形的步进。
- 边界只换算一次：`mm = twip * 25.4 / 1440`。排版内部用毫米浮点计算，写回 IR 时取整到 `1e-4 mm`；OFD 输出量化到 `0.001 mm`。

```rust
use ofdbox_folio_ir::Twips;

// 1440 twips == 25.4 mm == 1 英寸；IR 只存整数。
assert_eq!(Twips::from_mm(25.4), Twips::new(1440));
assert_eq!(Twips::from_pt(12.0), Twips::new(240));
```

## Reader 契约

`DocumentSource` 是每个格式适配器都要实现的 trait；缓存、选择、搜索与命中测试由门面在其上完成：

- `info()` 返回文档级 `DocumentInfo`：格式、元数据、页数、大纲与附件元数据；
- `page_info(index)` 返回页面几何信息，不加载页面内容；
- `load_page(index)` 解析并返回一页 `Page`；
- `load_attachment(id)` 返回附件字节；
- `font_names()` 与 `clear_caches()` 是可选钩子，默认实现为空。

打开文档时只解析元数据、页面表、大纲与资源索引；`load_page` 按需解析单页，阅读器维护带字节预算的 LRU 缓存。单页损坏只影响该页：阅读器显示占位内容并保留诊断。

`FixedDocumentSource` 为已经排版好的 `FixedDocument` 实现同一 trait：

```rust
use ofdbox_folio_ir::{DocumentSource, FixedDocumentSource, SourceFormat};

// 把排版结果接回阅读器栈。
let mut source = FixedDocumentSource::new(fixed, SourceFormat::Docx);
let info = source.info();
let page = source.load_page(0)?;
```

正是这层桥接，让 DOCX 包或通过创建 API 构建的文档，也能与原生 OFD、PDF 文件共用同一套页面缓存、选择、搜索与命中测试 API。`document()` 借用被包装的固定文档，`into_inner()` 将其取出。

`DocumentSource` 目前不要求 `Send`，因为 `ofdbox-core` 的包读取器是单线程的；该约束以后可以收紧。

## 确定性

- 解析与排版不读时钟、不取随机数、不读环境变量，也不依赖 `HashMap` 的遍历顺序。
- 相同 Flow 输入 + 相同字体字节 + 相同选项 + 相同引擎版本 → 完全相同的 `FixedDocument` 与相同输出字节。
- `FixedDocument.engine` 携带 `EngineInfo`：排版引擎版本与参与排版的字体内容指纹（`FontFingerprint`），因此结果可归因到具体的引擎构建与字体字节。
- 阅读器的加载时序可以变化，但页面内容不会因此改变。
- 不写日志、不打印：无法表示的项进入 `Report` 诊断，失败返回 `Result` 错误。

## 格式中立约束

- OFD、PDF、DOCX、ZIP、XML 类型不出现在公开 API 中；本 crate 唯一的依赖是 `thiserror`。
- 公开签名禁止依赖泄漏：`ofdbox_core`、`lopdf`、`quick_xml`、`zip`、`rustybuzz`、`ttf_parser`、`vello`、`wgpu` 类型只留在各自 crate 内。
- 本 crate 只定义类型、校验与文本算法：不访问文件或网络，不渲染，也不排版。
- Fixed IR 不保留指向 Flow IR 的反向引用，因此单个页面脱离源文档仍然有效。
- 字体通过 `FontProvider` 注入；本 crate 从不扫描系统字体，其 TrueType 辅助函数（`extract_truetype_face`、`subset_truetype`）也不暴露字体库的类型。

## 延伸阅读

- [架构](../../docs/architecture.zh-CN.md)：crate 分工、两种 IR 与关键契约。
- [布局契约](../ofdbox-folio-layout/CONTRACT.zh-CN.md)：Flow IR 排成 Fixed IR 的规则。
- [项目 README](../../README.zh-CN.md)：目标、当前状态与用法。

## 许可

双许可，MIT 或 Apache-2.0，任选其一：[LICENSE-MIT](../../LICENSE-MIT) · [LICENSE-APACHE](../../LICENSE-APACHE)。
