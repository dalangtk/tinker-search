//! 自然语言 → 搜索条件的解析引擎。
//!
//! 这里**不含任何具体词汇**：时间词、类型词、忽略词、字段词全部由插件清单提供。
//! 引擎只负责三件事：
//!   1. 在文本里做最长匹配，把词消耗掉并归到某一类（类型 / 时间 / 字段 / 忽略 / 文件夹）；
//!   2. 把相对时间（今天、上个月、最近 3 天）换算成本地时区的时间戳区间；
//!   3. 剩下的字词当关键词，交给索引内核做相关度检索。
//!
//! 这样换一个词表更全的插件、或换成调本地小模型来解析，都只是换插件的事，
//! 不需要改宿主代码。

use chrono::{Datelike, Duration, Local, Months, NaiveDate, TimeZone};
use serde::{Deserialize, Serialize};

// ============================================================================
// 插件提供的词表
// ============================================================================

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct NluCfg {
    pub locale: String,
    /// 没写「修改 / 打开」这类词时，默认按哪个时间字段过滤
    pub default_time_field: String,
    pub ignore: Vec<String>,
    pub fields: Vec<FieldRule>,
    pub times: Vec<TimeRule>,
    pub types: Vec<TypeRule>,
    pub folders: Vec<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct FieldRule {
    pub words: Vec<String>,
    /// ctime | mtime | atime
    pub field: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct TimeRule {
    pub words: Vec<String>,
    /// today / yesterday / days:-2 / this_week / last_month / recent:3d / date:2026-11-05
    pub range: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct TypeRule {
    pub words: Vec<String>,
    pub exts: Vec<String>,
}

// ============================================================================
// 解析结果
// ============================================================================

/// 一次自然语言解析的产物，前端直接拿来渲染「已理解为…」。
#[derive(Debug, Clone, Default, Serialize)]
pub struct NluQuery {
    /// 剩余关键词（已去掉时间词、类型词、语气词）
    pub keyword: String,
    pub exts: Vec<String>,
    pub dirs_only: bool,
    /// ctime | mtime | atime
    pub field: String,
    /// 0 表示不限
    pub from_ms: i64,
    pub to_ms: i64,
    /// 时间区间的人话描述，例如「今天」「最近 3 天」
    pub label: String,
    /// 0~1，用来在界面上提示「这句话我有多确定」
    pub score: f64,
    /// 给人看的理解结果，例如「类型 pdf」「时间 今天」
    pub notes: Vec<String>,
    /// 未被识别的原始片段，用于提示「这部分我没看懂」
    pub leftover: Vec<String>,
}

/// 词表里的一条规则，按最长匹配优先。
enum Action {
    Type(Vec<String>),
    Folder,
    Field(String),
    Time(String),
    Ignore,
}

struct Entry {
    chars: Vec<char>,
    action: Action,
}

// ============================================================================
// 解析入口
// ============================================================================

/// 把一句话解析成搜索条件。`now` 传进来是为了让测试可复现。
pub fn parse(text: &str, cfg: &NluCfg, now: chrono::DateTime<Local>) -> NluQuery {
    let src: Vec<char> = text.trim().to_lowercase().chars().collect();
    let mut used = vec![false; src.len()];
    let mut q = NluQuery {
        field: if cfg.default_time_field.is_empty() {
            "ctime".into()
        } else {
            cfg.default_time_field.clone()
        },
        ..Default::default()
    };

    // 1) 先让时间语法引擎扫一遍：「上个月5号」「10月1日到10月7日」「最近三个月」
    //    这类说法词表写不全，交给专门的规则解析器。认到就把字消耗掉，词表不再重复吃。
    if let Some((from, to, label)) = crate::grammar::scan(&src, &mut used, now) {
        q.from_ms = from;
        q.to_ms = to;
        q.label = label;
    }

    // 2) 词表最长匹配
    let entries = build_entries(cfg);
    let mut i = 0;
    while i < src.len() {
        if used[i] {
            i += 1;
            continue;
        }
        let mut hit = None;
        for (idx, e) in entries.iter().enumerate() {
            if e.chars.is_empty() || i + e.chars.len() > src.len() {
                continue;
            }
            if e.chars.iter().enumerate().all(|(k, c)| src[i + k] == *c) {
                hit = Some(idx);
                break;
            }
        }
        match hit {
            Some(idx) => {
                let len = entries[idx].chars.len();
                for k in i..i + len {
                    used[k] = true;
                }
                match &entries[idx].action {
                    Action::Type(exts) => {
                        for e in exts {
                            if !q.exts.iter().any(|x| x == e) {
                                q.exts.push(e.clone());
                            }
                        }
                    }
                    Action::Folder => q.dirs_only = true,
                    Action::Field(f) => q.field = f.clone(),
                    Action::Time(spec) => apply_time(&mut q, spec, now),
                    Action::Ignore => {}
                }
                i += len;
            }
            None => i += 1,
        }
    }

    // 3) 剩下的当关键词
    let (keyword, leftover) = collect_keyword(&src, &used);
    q.keyword = keyword;
    q.leftover = leftover;

    q.score = score_of(&q);
    q.notes = notes_of(&q);
    q
}

fn build_entries(cfg: &NluCfg) -> Vec<Entry> {
    let mut out: Vec<Entry> = Vec::new();
    let mut push = |words: &[String], action: Action| {
        for w in words {
            let w = w.trim().to_lowercase();
            if !w.is_empty() {
                out.push(Entry { chars: w.chars().collect(), action: clone_action(&action) });
            }
        }
    };
    for r in &cfg.types {
        push(&r.words, Action::Type(r.exts.clone()));
    }
    for r in &cfg.fields {
        push(&r.words, Action::Field(r.field.clone()));
    }
    for r in &cfg.times {
        push(&r.words, Action::Time(r.range.clone()));
    }
    push(&cfg.folders, Action::Folder);
    push(&cfg.ignore, Action::Ignore);

    // 最长匹配优先：不排序的话「文件夹」会被「文件」先吃掉
    out.sort_by(|a, b| b.chars.len().cmp(&a.chars.len()));
    out
}

fn clone_action(a: &Action) -> Action {
    match a {
        Action::Type(v) => Action::Type(v.clone()),
        Action::Folder => Action::Folder,
        Action::Field(f) => Action::Field(f.clone()),
        Action::Time(t) => Action::Time(t.clone()),
        Action::Ignore => Action::Ignore,
    }
}

/// 把未消耗的字收集成关键词。连续的字母数字算一个词，中文按单字聚合成片段。
fn collect_keyword(src: &[char], used: &[bool]) -> (String, Vec<String>) {
    let mut buf = String::new();
    for (i, c) in src.iter().enumerate() {
        if used[i] {
            // 被消耗的词在这里断开，避免「报告今天」粘成一个词
            if !buf.is_empty() {
                buf.push(' ');
            }
            continue;
        }
        if c.is_whitespace() || is_punct(*c) {
            buf.push(' ');
        } else {
            buf.push(*c);
        }
    }
    let joined = buf.split_whitespace().collect::<Vec<_>>().join(" ");
    let leftover: Vec<String> = joined
        .split_whitespace()
        .filter(|t| !t.is_empty())
        .map(|t| t.to_string())
        .collect();
    (joined, leftover)
}

fn is_punct(c: char) -> bool {
    matches!(
        c,
        '，' | '。' | '、' | '；' | '：' | '？' | '！' | '“' | '”' | '‘' | '’'
            | '（' | '）' | '《' | '》' | ',' | '.' | ';' | ':' | '?' | '!' | '"'
            | '\'' | '(' | ')' | '[' | ']' | '【' | '】' | '·' | '~'
    )
}

fn score_of(q: &NluQuery) -> f64 {
    let mut s: f64 = 0.0;
    if !q.exts.is_empty() {
        s += 0.45;
    }
    if q.from_ms > 0 || q.to_ms > 0 {
        s += 0.35;
    }
    if !q.keyword.is_empty() {
        s += 0.35;
    }
    if q.dirs_only {
        s += 0.1;
    }
    s.min(1.0)
}

fn notes_of(q: &NluQuery) -> Vec<String> {
    let mut out = Vec::new();
    if !q.exts.is_empty() {
        out.push(format!("类型 {}", q.exts.iter().take(6).cloned().collect::<Vec<_>>().join(" / ")));
    }
    if q.from_ms > 0 || q.to_ms > 0 {
        out.push(format!("{} {}", field_label(&q.field), q.label));
    }
    if q.dirs_only {
        out.push("只看文件夹".into());
    }
    if !q.keyword.is_empty() {
        out.push(format!("关键词 {}", q.keyword));
    }
    if out.is_empty() {
        out.push("没有识别出条件".into());
    }
    out
}

fn field_label(f: &str) -> &'static str {
    match f {
        "mtime" => "修改时间",
        "atime" => "访问时间",
        _ => "创建时间",
    }
}

// ============================================================================
// 时间区间
// ============================================================================

fn apply_time(q: &mut NluQuery, spec: &str, now: chrono::DateTime<Local>) {
    if let Some((from, to, label)) = resolve_range(spec, now) {
        q.from_ms = from;
        q.to_ms = to;
        q.label = label;
    }
}

pub(crate) fn day_start(d: NaiveDate) -> i64 {
    let naive = d.and_hms_opt(0, 0, 0).unwrap();
    // 夏令时切换日可能没有 00:00，退回 UTC 也不会差太多
    Local
        .from_local_datetime(&naive)
        .earliest()
        .map(|dt| dt.timestamp_millis())
        .unwrap_or_else(|| naive.and_utc().timestamp_millis())
}

fn resolve_range(spec: &str, now: chrono::DateTime<Local>) -> Option<(i64, i64, String)> {
    let today = now.date_naive();
    let spec = spec.trim();
    let (from, to, label) = match spec {
        "today" => (day_start(today), day_start(today + Duration::days(1)), "今天".to_string()),
        "yesterday" => (
            day_start(today - Duration::days(1)),
            day_start(today),
            "昨天".to_string(),
        ),
        "this_week" => {
            let mon = today - Duration::days(today.weekday().num_days_from_monday() as i64);
            (day_start(mon), day_start(mon + Duration::days(7)), "本周".to_string())
        }
        "last_week" => {
            let mon = today - Duration::days(today.weekday().num_days_from_monday() as i64);
            (
                day_start(mon - Duration::days(7)),
                day_start(mon),
                "上周".to_string(),
            )
        }
        "this_month" => {
            let first = today.with_day(1)?;
            let next = first + Months::new(1);
            (day_start(first), day_start(next), "本月".to_string())
        }
        "last_month" => {
            let first = today.with_day(1)?;
            let prev = first - Months::new(1);
            (day_start(prev), day_start(first), "上个月".to_string())
        }
        "this_year" => {
            let first = NaiveDate::from_ymd_opt(today.year(), 1, 1)?;
            let next = NaiveDate::from_ymd_opt(today.year() + 1, 1, 1)?;
            (day_start(first), day_start(next), format!("{} 年", today.year()))
        }
        "last_year" => {
            let first = NaiveDate::from_ymd_opt(today.year() - 1, 1, 1)?;
            let next = NaiveDate::from_ymd_opt(today.year(), 1, 1)?;
            (day_start(first), day_start(next), format!("{} 年", today.year() - 1))
        }
        _ => {
            if let Some(rest) = spec.strip_prefix("days:") {
                let n: i64 = rest.parse().ok()?;
                let d = today + Duration::days(n);
                (day_start(d), day_start(d + Duration::days(1)), format!("{d}"))
            } else if let Some(rest) = spec.strip_prefix("recent:") {
                let (n, unit) = split_amount(rest)?;
                let from = subtract(now, n, unit)?;
                (from.timestamp_millis(), now.timestamp_millis(), format!("最近 {}", amount_label(n, unit)))
            } else if let Some(rest) = spec.strip_prefix("date:") {
                let d = NaiveDate::parse_from_str(rest, "%Y-%m-%d").ok()?;
                (day_start(d), day_start(d + Duration::days(1)), format!("{d}"))
            } else if let Some(rest) = spec.strip_prefix("month:") {
                let d = NaiveDate::parse_from_str(&format!("{rest}-01"), "%Y-%m-%d").ok()?;
                (day_start(d), day_start(d + Months::new(1)), format!("{rest}"))
            } else {
                return None;
            }
        }
    };
    if from >= to {
        return None;
    }
    Some((from, to, label))
}

fn split_amount(s: &str) -> Option<(i64, char)> {
    let mut num = String::new();
    let mut unit = None;
    for c in s.chars() {
        if c.is_ascii_digit() {
            num.push(c);
        } else {
            unit = Some(c);
            break;
        }
    }
    let n: i64 = num.parse().ok()?;
    Some((n, unit?))
}

fn subtract(now: chrono::DateTime<Local>, n: i64, unit: char) -> Option<chrono::DateTime<Local>> {
    let dt = match unit {
        'h' => now - Duration::hours(n),
        'd' => now - Duration::days(n),
        'w' => now - Duration::weeks(n),
        'M' => now - Months::new(n as u32),
        'y' => now - Months::new((n * 12) as u32),
        _ => return None,
    };
    Some(dt)
}

fn amount_label(n: i64, unit: char) -> String {
    match unit {
        'h' => format!("{n} 小时"),
        'd' => format!("{n} 天"),
        'w' => format!("{n} 周"),
        'M' => format!("{n} 个月"),
        'y' => format!("{n} 年"),
        _ => format!("{n}"),
    }
}

// ============================================================================
// 测试：把需求里给的两个例句钉死，改词表或改引擎时不会悄悄退化
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn cfg() -> NluCfg {
        let v: serde_json::Value =
            serde_json::from_str(include_str!("../assets/plugins/tinker.nl-zh.json")).unwrap();
        serde_json::from_value(v["nlu"].clone()).unwrap()
    }

    fn at(y: i32, m: u32, d: u32, h: u32) -> chrono::DateTime<Local> {
        Local.with_ymd_and_hms(y, m, d, h, 0, 0).unwrap()
    }

    /// 「今天下载的pdf文件」→ 创建时间 = 今天，后缀 pdf，没有剩余关键词
    #[test]
    fn today_downloaded_pdf() {
        let q = parse("今天下载的pdf文件", &cfg(), at(2026, 11, 5, 10));
        assert_eq!(q.exts, vec!["pdf"]);
        assert_eq!(q.field, "ctime");
        assert_eq!(q.keyword, "");
        let day = NaiveDate::from_ymd_opt(2026, 11, 5).unwrap();
        assert_eq!(q.from_ms, day_start(day));
        assert_eq!(q.to_ms, day_start(day + Duration::days(1)));
        assert!(q.score > 0.7, "两个条件都识别到了，置信度应当高：{}", q.score);
    }

    /// 「上个月下载的word文件」→ 创建时间 = 上个月，后缀 doc/docx
    #[test]
    fn last_month_word() {
        let q = parse("上个月下载的word文件", &cfg(), at(2026, 11, 5, 10));
        assert!(q.exts.contains(&"doc".to_string()) && q.exts.contains(&"docx".to_string()));
        assert_eq!(q.field, "ctime");
        assert_eq!(q.keyword, "");
        let oct = NaiveDate::from_ymd_opt(2026, 10, 1).unwrap();
        let nov = NaiveDate::from_ymd_opt(2026, 11, 1).unwrap();
        assert_eq!((q.from_ms, q.to_ms), (day_start(oct), day_start(nov)));
    }

    /// 「最近3天改过的表格」→ 修改时间 = 最近 3 天（引擎自己认数字，词表没有这条）
    #[test]
    fn recent_three_days_modified() {
        let now = at(2026, 11, 5, 10);
        let q = parse("最近3天改过的表格", &cfg(), now);
        assert_eq!(q.field, "mtime");
        // 按自然日算：今天整天都算「最近 3 天」，所以右端是明天 0 点
        assert_eq!(q.to_ms, day_start(NaiveDate::from_ymd_opt(2026, 11, 6).unwrap()));
        assert_eq!(q.from_ms, day_start(NaiveDate::from_ymd_opt(2026, 11, 2).unwrap()));
        assert!(q.exts.contains(&"xlsx".to_string()));
    }

    /// 需求里的原句：上个月下载的 pdf
    #[test]
    fn last_month_downloaded_pdf() {
        let q = parse("上个月下载的pdf", &cfg(), at(2026, 11, 5, 10));
        assert_eq!(q.exts, vec!["pdf"]);
        assert_eq!(q.field, "ctime", "「下载」应当落到创建时间");
        assert_eq!(q.keyword, "");
        assert_eq!(q.label, "上个月");
        assert_eq!(
            (q.from_ms, q.to_ms),
            (
                day_start(NaiveDate::from_ymd_opt(2026, 10, 1).unwrap()),
                day_start(NaiveDate::from_ymd_opt(2026, 11, 1).unwrap())
            )
        );
    }

    /// 去年 8 月改过的表格：年份用中文相对词，月份是数字
    #[test]
    fn last_year_august_word() {
        let q = parse("去年8月改过的表格", &cfg(), at(2026, 11, 5, 10));
        assert_eq!(q.field, "mtime");
        assert_eq!(
            (q.from_ms, q.to_ms),
            (
                day_start(NaiveDate::from_ymd_opt(2025, 8, 1).unwrap()),
                day_start(NaiveDate::from_ymd_opt(2025, 9, 1).unwrap())
            )
        );
        assert_eq!(q.keyword, "");
    }

    /// 具体某一天：2026年10月3日
    #[test]
    fn exact_day() {
        let q = parse("2026年10月3日的pdf", &cfg(), at(2026, 11, 5, 10));
        assert_eq!(q.exts, vec!["pdf"]);
        assert_eq!(
            (q.from_ms, q.to_ms),
            (
                day_start(NaiveDate::from_ymd_opt(2026, 10, 3).unwrap()),
                day_start(NaiveDate::from_ymd_opt(2026, 10, 4).unwrap())
            )
        );
    }

    /// 区间：10月1日到10月7日
    #[test]
    fn date_span() {
        let q = parse("10月1日到10月7日的截图", &cfg(), at(2026, 11, 5, 10));
        assert!(q.exts.contains(&"png".to_string()));
        assert_eq!(
            (q.from_ms, q.to_ms),
            (
                day_start(NaiveDate::from_ymd_opt(2026, 10, 1).unwrap()),
                day_start(NaiveDate::from_ymd_opt(2026, 10, 8).unwrap())
            )
        );
        assert_eq!(q.keyword, "");
    }

    /// 上个月 5 号：月份词 + 具体日
    #[test]
    fn last_month_5th() {
        let q = parse("上个月5号拍的视频", &cfg(), at(2026, 11, 5, 10));
        assert!(q.exts.contains(&"mp4".to_string()));
        assert_eq!(
            (q.from_ms, q.to_ms),
            (
                day_start(NaiveDate::from_ymd_opt(2026, 10, 5).unwrap()),
                day_start(NaiveDate::from_ymd_opt(2026, 10, 6).unwrap())
            )
        );
        assert_eq!(q.keyword, "", "「拍的」应当被当成语气词吃掉");
    }

    /// 去年第三季度
    #[test]
    fn last_year_q3() {
        let q = parse("去年第三季度的报告", &cfg(), at(2026, 11, 5, 10));
        assert_eq!(q.keyword, "报告");
        assert_eq!(
            (q.from_ms, q.to_ms),
            (
                day_start(NaiveDate::from_ymd_opt(2025, 7, 1).unwrap()),
                day_start(NaiveDate::from_ymd_opt(2025, 10, 1).unwrap())
            )
        );
    }

    /// 最近三个月：中文数字 + 个月
    #[test]
    fn recent_three_months() {
        let q = parse("最近三个月下载的视频", &cfg(), at(2026, 11, 5, 10));
        assert!(q.exts.contains(&"mp4".to_string()));
        assert_eq!(
            (q.from_ms, q.to_ms),
            (
                day_start(NaiveDate::from_ymd_opt(2026, 8, 5).unwrap()),
                day_start(NaiveDate::from_ymd_opt(2026, 11, 6).unwrap())
            )
        );
    }

    /// 上周三：周词 + 星期几
    #[test]
    fn last_wednesday() {
        let q = parse("上周三的截图", &cfg(), at(2026, 11, 5, 10));
        assert_eq!(
            (q.from_ms, q.to_ms),
            (
                day_start(NaiveDate::from_ymd_opt(2026, 10, 28).unwrap()),
                day_start(NaiveDate::from_ymd_opt(2026, 10, 29).unwrap())
            )
        );
    }

    /// 关键词要留下：时间词、类型词、语气词去掉，剩下的才是关键词
    #[test]
    fn keyword_survives() {
        let q = parse("帮我找今天下载的季度报告pdf", &cfg(), at(2026, 11, 5, 10));
        assert_eq!(q.keyword, "季度报告");
        assert_eq!(q.exts, vec!["pdf"]);
        assert!(q.from_ms > 0);
    }

    /// 「上周的截图」：时间 + 图片类型，没写时间字段就按默认（创建时间）
    #[test]
    fn last_week_screenshots() {
        let q = parse("上周的截图", &cfg(), at(2026, 11, 5, 10));
        assert_eq!(q.field, "ctime");
        assert!(q.exts.contains(&"png".to_string()));
        // 2026-11-05 是周四，本周一 = 11-02，上周一 = 10-26
        let mon = NaiveDate::from_ymd_opt(2026, 10, 26).unwrap();
        assert_eq!(q.from_ms, day_start(mon));
    }

    /// 什么条件都没有时不能瞎猜
    #[test]
    fn nothing_recognized() {
        let q = parse("随便说点什么", &cfg(), at(2026, 11, 5, 10));
        assert_eq!(q.from_ms, 0);
        assert!(q.exts.is_empty());
        assert!(q.score < 0.4);
    }

    /// 数字不能被误当成时间：版本号、容量、数量都要留着当关键词
    #[test]
    fn numbers_are_not_dates() {
        for s in ["版本3.5的pdf", "一共5个文件", "3小时的录音", "2019"] {
            let q = parse(s, &cfg(), at(2026, 11, 5, 10));
            assert_eq!(q.from_ms, 0, "「{s}」不该被解析成时间：{}", q.label);
        }
    }

    /// 「上个月到现在」= 从左端一直到现在
    #[test]
    fn open_ended_span() {
        let q = parse("上个月到现在的pdf", &cfg(), at(2026, 11, 5, 10));
        assert_eq!(q.from_ms, day_start(NaiveDate::from_ymd_opt(2026, 10, 1).unwrap()));
        assert_eq!(q.to_ms, day_start(NaiveDate::from_ymd_opt(2026, 11, 6).unwrap()));
    }

    /// 「去年」单独出现就是整年
    #[test]
    fn last_year_alone() {
        let q = parse("去年的pdf", &cfg(), at(2026, 11, 5, 10));
        assert_eq!(
            (q.from_ms, q.to_ms),
            (
                day_start(NaiveDate::from_ymd_opt(2025, 1, 1).unwrap()),
                day_start(NaiveDate::from_ymd_opt(2026, 1, 1).unwrap())
            )
        );
    }

    /// 「3天前」是过去某一天，不是区间
    #[test]
    fn days_ago_point() {
        let q = parse("3天前的pdf", &cfg(), at(2026, 11, 5, 10));
        assert_eq!(
            (q.from_ms, q.to_ms),
            (
                day_start(NaiveDate::from_ymd_opt(2026, 11, 2).unwrap()),
                day_start(NaiveDate::from_ymd_opt(2026, 11, 3).unwrap())
            )
        );
    }

    /// 没有「最近」前缀，靠「内」也能成立：三天内 / 半个月内 / 半年内
    #[test]
    fn inner_suffix_without_prefix() {
        for (s, days) in [("三天内的word", 3), ("半个月内的word", 15)] {
            let q = parse(s, &cfg(), at(2026, 11, 5, 10));
            assert_eq!(q.keyword, "", "「{s}」不该把时间词漏进关键词");
            assert_eq!(q.from_ms, day_start(NaiveDate::from_ymd_opt(2026, 11, 5).unwrap())
                .checked_sub(days * 86_400_000)
                .unwrap());
        }
        let q = parse("半年内下载的安装包", &cfg(), at(2026, 11, 5, 10));
        assert_eq!(q.keyword, "");
        assert!(q.from_ms > 0);
        assert!(q.exts.contains(&"exe".to_string()));
    }

    /// 「去年底」：年份词 + 底，不能把「底」漏成关键词
    #[test]
    fn year_end() {
        let q = parse("去年底的合同", &cfg(), at(2026, 11, 5, 10));
        assert_eq!(q.keyword, "合同");
        assert_eq!(
            (q.from_ms, q.to_ms),
            (
                day_start(NaiveDate::from_ymd_opt(2025, 12, 1).unwrap()),
                day_start(NaiveDate::from_ymd_opt(2026, 1, 1).unwrap())
            )
        );
    }
}
