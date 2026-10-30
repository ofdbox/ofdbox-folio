[English](reader.md) | 简体中文

# Reader

阅读器是打开 OFD、PDF 与 DOCX 文档的入口，让页面保持惰性加载、可缓存、可选择；完整项目介绍见 [README](../README.zh-CN.md)。

## 加载模型

- **元数据先行（OFD/PDF）**：打开原生文档只解析元数据、页表、大纲和资源索引；`DocumentSource::load_page()` 按需解析单页。
- **DOCX 立即排版**：打开 DOCX 时解析 `word/document.xml` 为 Flow IR，用调用方提供的 `FontProvider` 排版，再把 `FixedDocument` 包成 `FixedDocumentSource`；打开后所有页面都已存在。
- **排版结果也是 Source**：`Document::from_fixed` 与 `Document::from_flow` 让排版结果复用缓存、选择、搜索与命中测试 API。
- **页面不可变**：`Page` 可以缓存并跨线程共享；OFD/PDF 不会一次性构建所有页面，DOCX 在排版后页面已全部存在。
- **资源共享**：字体和图片属于文档而不是某一页，加载其他页不会重新读取它们。
- **局部损坏**：加载失败的页面显示占位内容并保留诊断，文档其余部分仍可阅读。
- **不可信输入**：每个格式解析器都返回错误而不是 panic，并受页数、解压、图像像素和递归深度上限约束。

## 阅读器性能模型

- **页面惰性（OFD/PDF）**：首屏时间与总页数无关；内存由缓存预算和可视页决定。
- **资源分离**：字体、图片按内容共享；图片按目标缩放档位解码，缩略图不触发全分辨率解码。
- **渲染列表缓存**：页面首次绘制时生成扁平渲染指令列表，滚动和缩放复用。
- **文本层独立**：绘制顺序不等于阅读顺序；文本层保存行框、基线和字符映射，选择/搜索不重新解析页面。
- **DOCX 立即排版**：DOCX 打开时解析为 Flow IR 并完成全部排版，页数与排版耗时都在打开阶段支付，页面缓存随后缓存排版后的页面；逐页渐进布局尚未实现。
- **错误隔离**：页级错误不终止文档加载。

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

let page = document.page(0)?;      // parsed on first access and written to the cache
let page_again = document.page(0)?; // cache hit, returns the same Arc<Page>
document.prefetch(1)?;              // prefetch the next page
let stats = document.cache_stats();
```

缓存按页面结构字节数计费，LRU 淘汰；`max_bytes = 0` 可禁用。字体和图片属于文档级共享资源，单独计价，因此预算约束的是页面结构而不是整个文档的占用。

## 跨页选择与搜索

```rust
use ofdbox_folio::{Document, OpenOptions, SearchQuery, Selection, TextPosition};

let mut document = Document::open_path("input.ofd", OpenOptions::default())?;

let hits = document.search_all(&SearchQuery {
    text: "发票".to_owned(),
    case_sensitive: false,
    whole_word: false,
})?;
for hit in &hits {
    let page = hit.range.start.page;
    // hit.rects are rectangles in page space
}

let selection = Selection {
    anchor: TextPosition::new(0, 0, 2),
    focus: TextPosition::new(1, 0, 5),
};
let text = document.selection_text(&selection)?;
let rects = document.selection_rects(&selection)?;
```

搜索按页按需加载并进入缓存；选择文本跨页时用换行符连接，页面之间不重复解析。命中矩形与选择矩形都位于页面空间，调用方只需把它们映射到自己的视口。

## 错误隔离

- **页级失败**：单页损坏不终止文档加载；该页显示占位内容，文档继续可用。
- **保留诊断**：失败原因始终可读，调用方既能显示文档其余部分，也能报告损坏页。
- **不牵连其他页面**：一个坏页不会让缓存失效，也不会迫使其他页面重新解析。
- **契约约束**：页级失败返回错误而不是 panic；页数、解压、图像像素和递归深度的上限依然生效。

## 测试与验收

- 阅读器内存随总页数保持有界；翻页复用字体、图片和渲染缓存。
- 文本选择、复制、搜索不重新解析页面。
- 单页损坏不影响其他页。

## 快速验证

阅读器相关示例可以端到端检查加载模型：

```bash
cargo run -p ofdbox-folio --example ofd_to_png -- input.ofd output.png 0 96
cargo run -p ofdbox-folio --example ofd_search -- input.ofd query
```

`ofd_to_png` 以 96 DPI 渲染第 0 页，`ofd_search` 用上面同一套搜索 API 执行跨页查询。
