#!/usr/bin/env python3
"""测试深度研究文章解析"""

import json
import sys

def parse_batchexecute_response(resp_text):
    """模拟 Rust 的解析逻辑"""
    # 跳过 )]}'  头
    body = resp_text
    if body.startswith(')'):
        nl_pos = body.find('\n')
        body = body[nl_pos+1:]
        while body and body[0] in ']\n\r}\' ':
            nl_pos = body.find('\n')
            if nl_pos == -1:
                break
            body = body[nl_pos+1:]

    # 找到长度行
    lines = body.split('\n', 1)
    if len(lines) < 2:
        return None

    length_str = lines[0].strip()
    try:
        chunk_len = int(length_str)
    except:
        return None

    chunk = lines[1][:chunk_len]

    # 解析 JSON
    decoder = json.JSONDecoder(strict=False)
    try:
        obj, _ = decoder.raw_decode(chunk, 0)
        if not isinstance(obj, list) or len(obj) == 0:
            return None

        # obj[0] 应该是 ["wrb.fr", "hNvQHb", "<inner_json>"]
        if obj[0][0] != "wrb.fr":
            return None

        inner_str = obj[0][2]
        inner = json.loads(inner_str, strict=False)
        return inner
    except Exception as e:
        print(f"解析错误: {e}", file=sys.stderr)
        return None

def extract_deep_research_articles(ai_data):
    """从 ai_data[30] 提取深度研究文章"""
    if not isinstance(ai_data, list) or len(ai_data) <= 30:
        return []

    entries = ai_data[30]
    if not isinstance(entries, list):
        return []

    articles = []
    for entry in entries:
        if not isinstance(entry, list) or len(entry) < 5:
            continue

        composite_id = entry[0] if isinstance(entry[0], str) else None
        uuid = entry[1] if isinstance(entry[1], str) else None
        title = entry[2] if isinstance(entry[2], str) else None
        doc_uuid = entry[3] if isinstance(entry[3], str) else None
        article_markdown = entry[4] if isinstance(entry[4], str) else None

        if not all([composite_id, uuid, title, doc_uuid, article_markdown]):
            continue

        articles.append({
            'composite_id': composite_id,
            'uuid': uuid,
            'title': title,
            'doc_uuid': doc_uuid,
            'article_markdown': article_markdown,
        })

    return articles

def main():
    with open('deep-demo.txt', 'r') as f:
        resp_text = f.read()

    print("📖 解析 batchexecute 响应...")
    inner = parse_batchexecute_response(resp_text)

    if not inner:
        print("❌ 解析失败")
        return 1

    print(f"✅ 成功解析，包含 {len(inner[0])} 条消息")

    # 第一条消息
    msg1 = inner[0][0]
    ai_data = msg1[3][0][0]  # r0

    print(f"\n📊 ai_data 长度: {len(ai_data)}")
    print(f"📊 ai_data[30] 存在: {len(ai_data) > 30 and ai_data[30] is not None}")

    articles = extract_deep_research_articles(ai_data)

    if not articles:
        print("\n⚠️  未找到深度研究文章")
        return 1

    print(f"\n🎉 找到 {len(articles)} 篇深度研究文章\n")

    for i, article in enumerate(articles, 1):
        print(f"{'='*60}")
        print(f"文章 {i}")
        print(f"{'='*60}")
        print(f"📌 标题: {article['title']}")
        print(f"🆔 UUID: {article['uuid']}")
        print(f"📄 文档UUID: {article['doc_uuid']}")
        print(f"📝 文章长度: {len(article['article_markdown'])} 字符")
        print(f"\n文章开头 (前300字符):")
        print(article['article_markdown'][:300])
        print(f"\n文章结尾 (后200字符):")
        print(article['article_markdown'][-200:])
        print()

    return 0

if __name__ == '__main__':
    sys.exit(main())
