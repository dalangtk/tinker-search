//! 可选的外部模型解析（`provider = "http"`）。
//!
//! 关键取舍：**不让模型算日期**。
//! 小模型（0.5B~3B）做「上个月是哪段时间」「跨年怎么办」这类日期算术极不可靠，
//! 但做「把这句话里的时间词、类型词、关键词分开」很稳。
//! 所以这里让模型只输出 JSON 里的**原文片段**：
//!
//! ```json
//! { "time": "上个月", "field": "mtime", "exts": ["doc", "docx"], "keyword": "报告" }
//! ```
//!
//! 拿回来之后 `time` 再交回本地规则引擎换算成时间戳区间——模型换哪个都不会算错日期。
//! 端点只要是 OpenAI 兼容的 `/v1/chat/completions` 即可：
//! Ollama、LM Studio、llama.cpp server、各类在线 API 都能直接填。

use crate::nlu::{NluCfg, NluQuery};
use crate::plugins::EngineCfg;
use serde::Deserialize;

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct ModelOut {
    time: String,
    field: String,
    exts: Vec<String>,
    keyword: String,
}

/// 调一次模型。任何环节失败都返回 None，调用方会静默回落到规则引擎。
pub fn parse_http(text: &str, engine: &EngineCfg, cfg: &NluCfg) -> Option<NluQuery> {
    let body = request_body(text, engine, cfg);
    let out = std::process::Command::new("curl")
        .args([
            "-sS",
            "--max-time",
            "8",
            "-X",
            "POST",
            &engine.endpoint,
            "-H",
            "Content-Type: application/json",
            "-d",
            &body,
        ])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let reply = String::from_utf8(out.stdout).ok()?;
    let content = extract_content(&reply)?;
    let json = extract_json(&content)?;
    let parsed: ModelOut = serde_json::from_str(&json).ok()?;

    // 时间片段交回本地规则换算：模型只认字，不算数
    let now = chrono::Local::now();
    let mut q = if parsed.time.trim().is_empty() {
        NluQuery {
            field: cfg.default_time_field.clone(),
            ..Default::default()
        }
    } else {
        crate::nlu::parse(&parsed.time, cfg, now)
    };

    // 模型给的后缀要用插件词表里存在的，避免它编出 `docxx` 这种
    let known = known_exts(cfg);
    let mut exts: Vec<String> = Vec::new();
    for e in parsed.exts {
        let e = e.trim().trim_start_matches('.').to_ascii_lowercase();
        if known.iter().any(|k| *k == e) && !exts.contains(&e) {
            exts.push(e);
        }
    }
    if !exts.is_empty() {
        q.exts = exts;
    }
    if !parsed.field.is_empty() && matches!(parsed.field.as_str(), "ctime" | "mtime" | "atime") {
        q.field = parsed.field.clone();
    }

    // 关键词：去掉模型顺手抄进去的时间/类型词残留
    let kw = parsed.keyword.trim().to_string();
    q.keyword = kw;
    q.score = 0.9;
    q.notes = vec![
        format!("模型解析（{}）", if engine.model.is_empty() { "http" } else { &engine.model }),
        if q.exts.is_empty() { String::new() } else { format!("类型 {}", q.exts.join(" / ")) },
        if q.from_ms > 0 { format!("{} {}", field_label(&q.field), q.label) } else { String::new() },
        if q.keyword.is_empty() { String::new() } else { format!("关键词 {}", q.keyword) },
    ]
    .into_iter()
    .filter(|s| !s.is_empty())
    .collect();
    Some(q)
}

fn field_label(f: &str) -> &'static str {
    match f {
        "mtime" => "修改时间",
        "atime" => "访问时间",
        _ => "创建时间",
    }
}

fn known_exts(cfg: &NluCfg) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for r in &cfg.types {
        for e in &r.exts {
            let e = e.to_ascii_lowercase();
            if !out.contains(&e) {
                out.push(e);
            }
        }
    }
    out
}

fn terms(words: &[String]) -> String {
    words.iter().filter(|w| !w.is_empty()).cloned().collect::<Vec<_>>().join("、")
}

fn request_body(text: &str, engine: &EngineCfg, cfg: &NluCfg) -> String {
    let mut time_words: Vec<String> = Vec::new();
    for r in &cfg.times {
        time_words.extend(r.words.iter().cloned());
    }

    let system = format!(
        "你是文件搜索的条件抽取器。把用户的中文/英文请求拆成四部分，只输出一个 JSON 对象，不要解释。\n\
         字段：\n\
         - time：原文里的时间短语，必须是原话片段（如「上个月」「最近3天」）；没有就空字符串。可用的说法：{}\n\
         - field：按哪个时间过滤，只能是 ctime（下载/创建/保存）或 mtime（修改/编辑）或 atime（打开/访问），拿不准就 ctime\n\
         - exts：文件后缀数组，不带点、小写，如 [\"pdf\"]、[\"doc\",\"docx\"]；不确定就空数组\n\
         - keyword：去掉时间词、类型词、语气词之后剩下的搜索关键词，没有就空字符串\n\
         例：\"今天下载的pdf文件\" -> {{\"time\":\"今天\",\"field\":\"ctime\",\"exts\":[\"pdf\"],\"keyword\":\"\"}}\n\
         例：\"上个月改过的项目报告 word\" -> {{\"time\":\"上个月\",\"field\":\"mtime\",\"exts\":[\"doc\",\"docx\"],\"keyword\":\"项目报告\"}}",
        terms(&time_words)
    );

    let payload = serde_json::json!({
        "model": engine.model,
        "temperature": 0,
        "stream": false,
        "messages": [
            { "role": "system", "content": system },
            { "role": "user", "content": text }
        ]
    });
    payload.to_string()
}

/// OpenAI 兼容响应：`choices[0].message.content`
fn extract_content(reply: &str) -> Option<String> {
    let v: serde_json::Value = serde_json::from_str(reply).ok()?;
    v.pointer("/choices/0/message/content")
        .and_then(|c| c.as_str())
        .map(|s| s.to_string())
}

/// 模型常把 JSON 包在 ```json 里，或前后加解释，这里只取第一个完整对象。
fn extract_json(s: &str) -> Option<String> {
    let start = s.find('{')?;
    let end = s.rfind('}')?;
    if end <= start {
        return None;
    }
    Some(s[start..=end].to_string())
}
