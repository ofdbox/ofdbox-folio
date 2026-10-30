[English](README.md) | 简体中文

# ofdbox-folio

`ofdbox-folio` 是 [OFDBox Folio](../../README.zh-CN.md) 的门面 crate：用同一个阅读器打开 OFD、PDF 与 DOCX，并在 Fixed IR 之上统一页面缓存、选择、搜索与命中测试。

## 快速开始

OFD、PDF 与 DOCX 使用同一套 `Document` API 打开；除非 `OpenOptions::format` 显式指定，格式由文件内容探测。OFD 与 PDF 保持惰性加载，DOCX 则读入 Flow IR 后立即排版，因此打开后所有页面都已存在：

```rust
use ofdbox_folio::{Document, OpenOptions};

let mut document = Document::open_path("input.ofd", OpenOptions::default())?;
let page = document.page(0)?;
println!("{} x {} mm", page.display_size().width, page.display_size().height);

// PDF：同样的调用，格式由文件头字节探测。
let mut document = Document::open_path("input.pdf", OpenOptions::default())?;
let page = document.page(0)?;

// DOCX：打开时读入 Flow IR 并立即完成排版，因此所有页面都已存在。
let mut document = Document::open_path("input.docx", OpenOptions::default())?;
let page = document.page(0)?;
```

随附示例端到端验证同一套 API：

```bash
cargo run -p ofdbox-folio --example ofd_to_png -- input.ofd output.png 0 96
cargo run -p ofdbox-folio --example ofd_search -- input.ofd query
```

## 统一 API

每个格式适配器都实现 `DocumentSource`，`Document` 通过同一个页面缓存驱动任意 source：

- **`Document`** — 用 `open_path` / `open_bytes` 打开，再用 `info`、`metadata`、`page_count`、`outline`、`attachments`、`page_info`、`page`、`page_text`、`text_position`、`prefetch`、`prefetch_around`、`cache_stats` 与 `clear_cache`。
- **`DocumentSource`** — 每个格式实现的契约：`info`、`page_info`、`load_page`、`load_attachment`，以及可选的 `font_names` 与 `clear_caches`。`FixedDocumentSource` 把排版后的 `FixedDocument` 适配到同一契约。
- **排版结果也是 Source** — `Document::from_fixed` 直接包装 `FixedDocument`；`Document::from_flow` 先排版 `FlowDocument`（需要 `layout` feature）。两者都接收 `ReaderOptions`，复用缓存、选择、搜索与命中测试。
- **缓存** — 每个文档一份 `CacheOptions`，通过 `OpenOptions::cache` 或 `ReaderOptions::cache` 设置；见[阅读器缓存](#阅读器缓存)。
- **搜索、选择与命中测试** — `search` / `search_all`、`selection_text`、`selection_rects` 与 `hit_test` 读取文本层并返回页面空间几何；见[跨页选择与搜索](#跨页选择与搜索)。

```rust
use ofdbox_folio::{
    ir::{FixedDocument, SourceFormat},
    Document, ReaderOptions,
};

// 包装排版产物，让它复用与原生 OFD/PDF 相同的阅读器 API。
let fixed = FixedDocument::default(); // 任意已排版文档，例如 `ofdbox_folio::layout` 的输出
let mut document = Document::from_fixed(fixed, SourceFormat::Docx, ReaderOptions::default());
document.page(0)?;
```

## 加载模型

- **元数据先行（OFD/PDF）**：打开原生文档只解析元数据、页表、大纲和资源索引；`DocumentSource::load_page()` 按需解析单页。
- **DOCX 立即排版**：打开 DOCX 时解析 `word/document.xml` 为 Flow IR，用调用方提供的 `FontProvider` 排版，再把 `FixedDocument` 包成 `FixedDocumentSource`；打开后所有页面都已存在。
- **排版结果也是 Source**：`Document::from_fixed` 与 `Document::from_flow` 让排版结果复用缓存、选择、搜索与命中测试 API。
- **页面不可变**：`Page` 可以缓存并跨线程共享；OFD/PDF 不会一次性构建所有页面，DOCX 在排版后页面已全部存在。
- **资源共享**：字体和图片属于文档而不是某一页，加载其他页不会重新读取它们。
- **局部损坏**：加载失败的页面显示占位内容并保留诊断，文档其余部分仍可阅读。
- **不可信输入**：每个格式解析器都返回错误而不是 panic，并受页数、解压、图像像素和递归深度上限约束。

## 性能模型

- **页面惰性（OFD/PDF）**：首屏时间与总页数无关；内存由缓存预算和可视页决定。
- **资源分离**：字体、图片按内容共享；图片按目标缩放档位解码，缩略图不触发全分辨率解码。
- **渲染列表缓存**：页面首次绘制时生成扁平渲染指令列表，滚动和缩放复用。
- **文本层独立**：绘制顺序不等于阅读顺序；文本层保存行框、基线和字符映射，选择与搜索不重新解析页面。
- **DOCX 立即排版**：DOCX 打开时解析为 Flow IR 并完成全部排版，页数与排版耗时都在打开阶段支付，页面缓存随后缓存排版后的页面；逐页渐进布局尚未实现。

## 阅读器缓存

```rust
use ofdbox_folio::{CacheOptions, Document, OpenOptions};

let mut document = Document::open_path(
    "input.ofd",
    OpenOptions {
        cache: CacheOptions { max_bytes: 64 * 1024 * 1024 },
        ..OpenOptions::default()
    },
)?;

let page = document.page(0)?;      // 首次访问时解析并写入缓存
let page_again = document.page(0)?; // 缓存命中，返回同一个 Arc<Page>
document.prefetch(1)?;              // 预取下一页
let stats = document.cache_stats();
```

缓存按页面结构字节数计费，LRU 淘汰；`max_bytes = 0` 可禁用。字体和图片属于文档级共享资源，单独计价，因此预算约束的是页面结构而不是整个文档的占用。

## 跨页选择与搜索

```rust
use ofdbox_folio::{ir::Point, Document, OpenOptions, SearchQuery, Selection, TextPosition};

let mut document = Document::open_path("input.ofd", OpenOptions::default())?;

let hits = document.search_all(&SearchQuery {
    text: "发票".to_owned(),
    case_sensitive: false,
    whole_word: false,
})?;
for hit in &hits {
    let page = hit.range.start.page;
    // hit.rects 是页面空间中的矩形
}

let selection = Selection {
    anchor: TextPosition::new(0, 0, 2),
    focus: TextPosition::new(1, 0, 5),
};
let text = document.selection_text(&selection)?;
let rects = document.selection_rects(&selection)?;

let position = document.hit_test(0, Point::new(12.0, 18.0))?; // 页面坐标，单位毫米
```

搜索按需加载页面并写入缓存；选择文本跨页时用换行符连接，页面之间不重复解析。命中矩形与选择矩形都位于页面空间，调用方只需把它们映射到自己的视口。

## 错误隔离

- **页级失败**：单页损坏不终止文档加载；该页显示占位内容，文档继续可用。
- **保留诊断**：失败原因始终可读，调用方既能显示文档其余部分，也能报告损坏页。
- **不牵连其他页面**：一个坏页不会让缓存失效，也不会迫使其他页面重新解析。
- **契约约束**：页级失败返回错误而不是 panic；页数、解压、图像像素和递归深度的上限依然生效。

## 参见

- [架构](../../docs/architecture.md) — 门面与 IR、排版、渲染及格式适配器的关系。
- [排版契约](../ofdbox-folio-layout/CONTRACT.md) — 排版与 Reader 共用的 Flow → Fixed 契约。
- [OFD 适配器](../ofdbox-folio-ofd/README.md) — OFD 探测、读取与写出。
- [PDF 适配器](../ofdbox-folio-pdf/README.md) — PDF 解析、字体与写出。
- [DOCX 适配器](../ofdbox-folio-docx/README.md) — DOCX 读写与 Flow IR 映射。
- [渲染](../ofdbox-folio-render/README.md) — Fixed IR 的 CPU/GPU 位图渲染。
