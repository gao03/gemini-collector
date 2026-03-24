# Gemini 深度研究（Deep Research）文章解析实现

## 📋 问题描述

Gemini 的深度研究功能会生成长篇 Markdown 格式的研究报告，但现有代码没有解析这些文章内容。用户提供了截图和示例数据（`deep-demo.txt`），需要实现解析功能。

## 🔍 数据结构分析

### batchexecute 响应格式

深度研究的响应通过 `hNvQHb` RPC 返回，格式如下：

```
)]}'
\n
<字符数>\n
[["wrb.fr","hNvQHb","<inner_json_string>"]]
```

### 内层 JSON 结构

解析后的 `inner_json` 结构：

```
inner[0]  // 消息列表
  [0]     // 第一条消息（包含深度研究文章）
    [0]   // 消息 ID: ["c_xxx", "r_xxx"]
    [3]   // 内容块
      [0] // 候选列表
        [0] // r0 - 响应对象
          [0]  // response_id: "rc_xxx"
          [1]  // text: ["我已经完成了研究。你可以提出后续问题..."]
          [30] // 深度研究文章列表 ⭐
            [0] // 第一篇文章
              [0] // composite_id: "c_xxx_uuid"
              [1] // uuid: "uuid-string"
              [2] // title: "文章标题"
              [3] // doc_uuid: "doc-uuid-string"
              [4] // article_markdown: "# 标题\n\n内容..." ⭐
              [5] // 其他元数据（引用源等）
              ...
```

### 关键字段说明

| 字段 | 位置 | 类型 | 说明 |
|------|------|------|------|
| `composite_id` | `entry[0]` | String | 组合ID（对话ID + 文档UUID） |
| `uuid` | `entry[1]` | String | 文章唯一标识符 |
| `title` | `entry[2]` | String | 文章标题 |
| `doc_uuid` | `entry[3]` | String | 文档UUID |
| `article_markdown` | `entry[4]` | String | 完整的 Markdown 文章内容 |

## ✅ 实现方案

### 1. 新增数据结构

在 `turn_parser.rs` 中添加：

```rust
/// 深度研究文章条目
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeepResearchArticle {
    pub composite_id: String,
    pub uuid: String,
    pub title: String,
    pub doc_uuid: String,
    pub article_markdown: String,
}
```

### 2. 扩展 AssistantContent

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AssistantContent {
    pub text: String,
    pub thinking: String,
    pub model: String,
    pub files: Vec<MediaFile>,
    pub music_meta: Option<MusicMeta>,
    pub gen_meta: Option<GenMeta>,
    pub deep_research_articles: Vec<DeepResearchArticle>,  // ⭐ 新增
}
```

### 3. 添加提取函数

```rust
/// 从 ai_data 中提取深度研究文章（来自 r0[30]）
fn extract_deep_research_articles(ai_data: &Value) -> Vec<DeepResearchArticle> {
    let mut articles = Vec::new();

    if !ai_data.is_array() || vlen(ai_data) <= 30 {
        return articles;
    }

    let entries = match vget(ai_data, 30) {
        Some(v) if v.is_array() => v.as_array().unwrap(),
        _ => return articles,
    };

    for entry in entries {
        let entry_arr = match entry.as_array() {
            Some(a) if a.len() >= 5 => a,
            _ => continue,
        };

        let composite_id = match entry_arr[0].as_str() {
            Some(s) => s.to_string(),
            None => continue,
        };
        let uuid = match entry_arr[1].as_str() {
            Some(s) => s.to_string(),
            None => continue,
        };
        let title = match entry_arr[2].as_str() {
            Some(s) => s.to_string(),
            None => continue,
        };
        let doc_uuid = match entry_arr[3].as_str() {
            Some(s) => s.to_string(),
            None => continue,
        };
        let article_markdown = match entry_arr[4].as_str() {
            Some(s) => s.to_string(),
            None => continue,
        };

        articles.push(DeepResearchArticle {
            composite_id,
            uuid,
            title,
            doc_uuid,
            article_markdown,
        });
    }

    articles
}
```

### 4. 集成到 parse_turn

在 `parse_turn` 函数的 AI 数据处理部分添加：

```rust
// Deep research articles from ai_data[30]
let articles = extract_deep_research_articles(ai);
if !articles.is_empty() {
    result.assistant.deep_research_articles = articles;
}
```

## 🧪 测试验证

### Python 测试脚本

创建了 `test_deep_research.py` 验证解析逻辑：

```bash
$ python3 test_deep_research.py
📖 解析 batchexecute 响应...
✅ 成功解析，包含 2 条消息

📊 ai_data 长度: 31
📊 ai_data[30] 存在: True

🎉 找到 1 篇深度研究文章

============================================================
文章 1
============================================================
📌 标题: 资深AI Agent程序员技能要求
🆔 UUID: 9ec62dd2-a80f-4b0e-b0af-bc0b026a01bd
📄 文档UUID: 2fb767c2-2287-4473-a696-4ba509eb9a31
📝 文章长度: 10187 字符
```

### Rust 单元测试

添加了两个测试用例：

1. `test_extract_deep_research_articles` - 测试正常提取
2. `test_extract_deep_research_articles_empty` - 测试边界情况

## 📦 输出格式

解析后的 JSON 输出示例：

```json
{
  "turn_id": "r_xxx",
  "timestamp": 1770609008,
  "user": {
    "text": "开始研究",
    "files": []
  },
  "assistant": {
    "text": "我已经完成了研究。你可以提出后续问题或者要求进行改动。",
    "model": "3 Pro",
    "files": [],
    "deep_research_articles": [
      {
        "composite_id": "c_d11ee17e727f3dd5_2fb767c2-2287-4473-a696-4ba509eb9a31",
        "uuid": "9ec62dd2-a80f-4b0e-b0af-bc0b026a01bd",
        "title": "资深AI Agent程序员技能要求",
        "doc_uuid": "2fb767c2-2287-4473-a696-4ba509eb9a31",
        "article_markdown": "# 2026年资深AI Agent工程师核心胜任力与技能图谱深度研究报告\n\n## 1. 行业背景与岗位演进..."
      }
    ]
  }
}
```

## 🔄 与现有代码的集成

### 修改的文件

1. **`src-tauri/src/turn_parser.rs`**
   - 新增 `DeepResearchArticle` 结构体
   - 扩展 `AssistantContent` 添加 `deep_research_articles` 字段
   - 新增 `extract_deep_research_articles` 函数
   - 在 `parse_turn` 中集成提取逻辑
   - 添加单元测试

### 向后兼容性

- 新增字段为 `Vec<DeepResearchArticle>`，默认为空列表
- 不影响现有的普通对话解析
- 仅当 `ai_data[30]` 存在时才提取文章

## 🎯 使用场景

1. **导出深度研究报告**
   ```bash
   gemini-collector export --format json
   ```

2. **保存为 Markdown 文件**
   ```rust
   for article in &turn.assistant.deep_research_articles {
       let filename = format!("{}.md", article.doc_uuid);
       std::fs::write(filename, &article.article_markdown)?;
   }
   ```

3. **搜索和索引**
   ```rust
   // 可以对 article_markdown 进行全文搜索
   if article.article_markdown.contains("AI Agent") {
       println!("找到相关文章: {}", article.title);
   }
   ```

## ⚠️ 注意事项

1. **字符编码**：文章内容是 UTF-8 编码的 Markdown，可能包含多字节字符
2. **文章长度**：示例文章长度约 10KB，实际可能更长
3. **多篇文章**：`ai_data[30]` 是数组，理论上可包含多篇文章
4. **占位符 URL**：文章中的 `http://googleusercontent.com/immersive_entry_chip/0` 是占位符，不是实际链接

## 🚀 后续优化

1. **元数据提取**：`entry[5]` 包含引用源等元数据，可进一步解析
2. **目录结构**：`entry[8]` 包含文章目录（TOC），可用于导航
3. **引用链接**：解析文章中的 `[1]`, `[2]` 等引用标记
4. **图片/附件**：检查是否有嵌入的媒体资源

## 📚 参考

- 原始数据：`deep-demo.txt`
- Python 测试：`test_deep_research.py`
- 相关代码：`src-tauri/src/turn_parser.rs`
- 协议文档：`src-tauri/src/protocol.rs`
