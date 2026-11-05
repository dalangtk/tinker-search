//! 中文时间表达的规则解析器（零依赖，微秒级）。
//!
//! 取向和 Python 的 fst-time-nlu 一致：不做模型，用「词法状态推进 + 单位换算」
//! 把一段中文说法映射成半开区间 `[start, end)`。差别只在于语言：
//! 这里是 Rust，直接编进 exe，不需要带一个 Python 运行时。
//!
//! 分工：
//!   - 本模块只认**时间**，不认文件类型、不认时间字段；
//!   - `nlu.rs` 的词表负责类型词、字段词（下载/修改/打开过）、语气词，以及
//!     词表里写死的几条 `range` 规格；
//!   - 两边都命中时，本模块先跑，先认出来的说法会把字消耗掉，词表不再重复吃。
//!
//! 覆盖的句式（按匹配优先级）：
//!   - 区间：`10月1日到10月7日`、`去年8月至今年3月`、`上个月到现在`
//!   - 绝对：`2026年10月5日`、`2025-10-05`、`10月5号`、`2025年10月`、`10月`
//!   - 周期：`今天`/`昨天`/`大前天`、`本周`/`上周三`/`上周末`、`这个月`/
//!     `上个月`/`上上个月`/`上个月5号`/`上个月底`、`本季度`/`去年第三季度`、
//!     `上半年`/`去年下半年`、`今年`/`前年`、`第一季度`
//!   - 数量：`最近3天`、`过去两周`、`三个月内`、`半年前`、`前两天`、`这三天`
//!
//! 中文数字、阿拉伯数字、全角数字都认（输入法打出来的「３天」也常见）。

use chrono::{Datelike, Duration, Local, Months, NaiveDate};

/// 一次时间匹配：消耗了 `len` 个字符，对应 `[start, end)` 这个日期区间。
#[derive(Debug, Clone)]
pub(crate) struct Side {
    pub len: usize,
    pub start: NaiveDate,
    pub end: NaiveDate,
    pub label: String,
}

/// 从左到右扫一遍，认第一个时间说法。认到就把对应字符标记为已消耗，
/// 剩下的字才是关键词。
pub(crate) fn scan(
    src: &[char],
    used: &mut [bool],
    now: chrono::DateTime<Local>,
) -> Option<(i64, i64, String)> {
    let today = now.date_naive();
    let mut i = 0;
    while i < src.len() {
        if used[i] {
            i += 1;
            continue;
        }
        if let Some(side) = at(src, i, today) {
            let end = (i + side.len).min(src.len());
            for k in i..end {
                used[k] = true;
            }
            return Some((
                crate::nlu::day_start(side.start),
                crate::nlu::day_start(side.end),
                side.label,
            ));
        }
        i += 1;
    }
    None
}

fn at(src: &[char], i: usize, today: NaiveDate) -> Option<Side> {
    // 区间必须先试：「10月1日到10月7日」里左端本身也是个合法日期
    span_at(src, i, today).or_else(|| side_at(src, i, today))
}

fn side_at(src: &[char], i: usize, today: NaiveDate) -> Option<Side> {
    absolute_at(src, i, today)
        .or_else(|| period_at(src, i, today))
        .or_else(|| duration_at(src, i, today))
}

// ============================================================================
// 区间：「A 到 B」「A 至 B」「A ~ B」
// ============================================================================

fn span_at(src: &[char], i: usize, today: NaiveDate) -> Option<Side> {
    let left = side_at(src, i, today)?;
    let mut j = i + left.len;
    let conn = if starts(src, j, "到")
        || starts(src, j, "至")
        || starts(src, j, "~")
        || starts(src, j, "～")
        || starts(src, j, "—")
    {
        1 // src 是字符数组，连接词都是单字符
    } else {
        return None;
    };
    j += conn;
    // 右端写「现在 / 至今 / 今天」时，含义就是「一直到今天」
    if starts(src, j, "现在") || starts(src, j, "至今") || starts(src, j, "今天") {
        return Some(Side {
            len: j + 2 - i,
            start: left.start,
            end: today + Duration::days(1),
            label: format!("{}至今", left.label),
        });
    }

    let right = side_at(src, j, today)?;
    if right.end <= left.start {
        return None;
    }
    Some(Side {
        len: j + right.len - i,
        start: left.start,
        end: right.end,
        label: format!("{}—{}", left.label, right.label),
    })
}

// ============================================================================
// 绝对时间：年月日、年-月-日、几月几号
// ============================================================================

fn absolute_at(src: &[char], i: usize, today: NaiveDate) -> Option<Side> {
    // 1) 先看年份是不是用「今年 / 去年 / 前年」表达的
    let mut j = i;
    let mut year: Option<i32> = None;
    for (w, d) in [("大前年", -3), ("前年", -2), ("去年", -1), ("上年", -1), ("今年", 0)] {
        if starts(src, i, w) {
            year = Some(today.year() + d);
            j = i + w.chars().count();
            break;
        }
    }
    if let Some(y) = year {
        if let Some(s) = month_day_at(src, j, today, y, true, i) {
            return Some(s);
        }
        if let Some(s) = quarter_at(src, j, y, i) {
            return Some(s);
        }
        if let Some(s) = half_at(src, j, y, i) {
            return Some(s);
        }
        // 只说了「去年」，交给 period_at 当年份区间处理
        return None;
    }

    // 2) 2025-10-05 / 2025/10/5
    if let Some(s) = iso_at(src, i) {
        return Some(s);
    }

    // 3) 2026年10月5日
    if let Some((n, nl)) = digits(src, i) {
        if (1900..=2200).contains(&n) && starts(src, i + nl, "年") {
            let y = n as i32;
            let k = i + nl + 1;
            if let Some(s) = month_day_at(src, k, today, y, true, i) {
                return Some(s);
            }
            if let Some(s) = quarter_at(src, k, y, i) {
                return Some(s);
            }
            if let Some(s) = half_at(src, k, y, i) {
                return Some(s);
            }
            let start = NaiveDate::from_ymd_opt(y, 1, 1)?;
            return Some(Side {
                len: k - i,
                start,
                end: NaiveDate::from_ymd_opt(y + 1, 1, 1)?,
                label: format!("{y} 年"),
            });
        }
    }

    // 4) 10月5日 / 10月（没写年份，按最近一次出现理解）
    month_day_at(src, i, today, today.year(), false, i)
}

fn iso_at(src: &[char], i: usize) -> Option<Side> {
    let (y, yl) = digits(src, i)?;
    if !(1900..=2200).contains(&y) {
        return None;
    }
    let sep = *src.get(i + yl)?;
    if sep != '-' && sep != '/' && sep != '.' {
        return None;
    }
    let (m, ml) = digits(src, i + yl + 1)?;
    if !(1..=12).contains(&m) {
        return None;
    }
    let after_m = i + yl + 1 + ml;
    let mut k = after_m;
    let mut day = None;
    if src.get(k) == Some(&'-') || src.get(k) == Some(&'/') {
        if let Some((d, dl)) = digits(src, k + 1) {
            if (1..=31).contains(&d) {
                day = Some(d as u32);
                k = k + 1 + dl;
            }
        }
    }
    let start = NaiveDate::from_ymd_opt(y as i32, m as u32, day.unwrap_or(1))?;
    let (end, label) = match day {
        Some(_) => (start + Duration::days(1), format!("{}-{m:02}-{:02}", y, start.day())),
        None => (start + Months::new(1), format!("{y} 年 {m} 月")),
    };
    Some(Side { len: if day.is_some() { k - i } else { after_m - i }, start, end, label })
}

/// `[M]月[D日|D号][份]`。`explicit` 为假时，落在未来的月份按去年算
/// （用户 11 月说「10 月」是刚过去的 10 月，说「12 月」只可能是去年的）。
fn month_day_at(
    src: &[char],
    j: usize,
    today: NaiveDate,
    year: i32,
    explicit: bool,
    origin: usize,
) -> Option<Side> {
    let (m, ml) = number(src, j)?;
    if !(1..=12).contains(&m) {
        return None;
    }
    let mut k = j + ml;
    if starts(src, k, "月份") {
        k += 2;
    } else if starts(src, k, "月") {
        k += 1;
    } else {
        return None;
    }

    let mut day: Option<u32> = None;
    let mut dend = k;
    if let Some((d, dl)) = number(src, k) {
        if (1..=31).contains(&d) && (starts(src, k + dl, "日") || starts(src, k + dl, "号")) {
            day = Some(d as u32);
            dend = k + dl + 1;
        }
    }

    let mut y = year;
    if !explicit {
        let probe = NaiveDate::from_ymd_opt(y, m as u32, day.unwrap_or(1))?;
        if probe > today {
            y -= 1;
        }
    }
    let start = NaiveDate::from_ymd_opt(y, m as u32, day.unwrap_or(1))?;
    let (end, label) = match day {
        Some(d) => (start + Duration::days(1), format!("{y} 年 {m} 月 {d} 日")),
        None => (start + Months::new(1), format!("{y} 年 {m} 月")),
    };
    Some(Side { len: dend - origin, start, end, label })
}

/// `[第]N季度`，调用方已经给出年份
fn quarter_at(src: &[char], j: usize, year: i32, origin: usize) -> Option<Side> {
    let mut k = j;
    if starts(src, k, "第") {
        k += 1;
    }
    let (q, ql) = number(src, k)?;
    if !(1..=4).contains(&q) || !starts(src, k + ql, "季度") {
        return None;
    }
    k = k + ql + 2;
    let start = NaiveDate::from_ymd_opt(year, ((q - 1) * 3 + 1) as u32, 1)?;
    Some(Side {
        len: k - origin,
        start,
        end: start + Months::new(3),
        label: format!("{year} 年第 {q} 季度"),
    })
}

/// `上半年 / 下半年`，调用方已经给出年份
fn half_at(src: &[char], j: usize, year: i32, origin: usize) -> Option<Side> {
    let second = if starts(src, j, "上半年") {
        false
    } else if starts(src, j, "下半年") {
        true
    } else {
        return None;
    };
    let start = NaiveDate::from_ymd_opt(year, if second { 7 } else { 1 }, 1)?;
    Some(Side {
        len: j + 3 - origin,
        start,
        end: start + Months::new(6),
        label: format!("{year} 年{}半年", if second { "下" } else { "上" }),
    })
}

// ============================================================================
// 周期词：今天 / 上周三 / 上个月5号 / 上季度 / 上半年 / 去年
// ============================================================================

pub(crate) fn period_at(src: &[char], i: usize, today: NaiveDate) -> Option<Side> {
    // 1) 天
    if let Some((w, off)) = best(src, i, DAYS) {
        return Some(day_range(today + Duration::days(off), i, w.chars().count(), w));
    }

    // 2) 周（含「上周三」「上周末」）
    if let Some((w, off)) = best(src, i, WEEKS) {
        let wl = w.chars().count();
        let base = week_start(today) + Duration::weeks(off);
        let k = i + wl;
        // 省掉「周 / 星期 / 礼拜」直接跟「三」也认（上周三）
        let mut skip = 0;
        if starts(src, k, "星期") || starts(src, k, "礼拜") {
            skip = 2;
        } else if starts(src, k, "周") {
            skip = 1;
        }
        if let Some(c) = src.get(k + skip) {
            if let Some(day) = weekday_of(*c) {
                let d = base + Duration::days(day);
                return Some(day_range(d, i, wl + skip + 1, &format!("{w}{}", *c)));
            }
            if *c == '末' {
                return Some(Side {
                    len: wl + skip + 1,
                    start: base + Duration::days(5),
                    end: base + Duration::days(7),
                    label: format!("{w}末"),
                });
            }
        }
        return Some(Side {
            len: wl,
            start: base,
            end: base + Duration::days(7),
            label: w.to_string(),
        });
    }

    // 3) 月（含「上个月5号」「上个月底」「上个月中旬」）
    if let Some((w, off)) = best(src, i, MONTHS) {
        let mut k = i + w.chars().count();
        if starts(src, k, "份") {
            k += 1;
        }
        let base = month_start_shift(today, off as i32);
        let end = base + Months::new(1);

        // 修饰：上旬 / 中旬 / 下旬 / 初 / 中 / 底 / 末
        for (pat, a, b) in [
            ("上旬", 0u32, 10u32),
            ("中旬", 10, 20),
            ("下旬", 20, 32),
            ("初", 0, 10),
            ("中", 10, 20),
            ("底", 25, 32),
            ("末", 20, 32),
        ] {
            if starts(src, k, pat) {
                let s = base + Duration::days(a as i64);
                let e = (base + Duration::days(b as i64)).min(end);
                return Some(Side {
                    len: k + pat.chars().count() - i,
                    start: s,
                    end: e.max(s + Duration::days(1)),
                    label: format!("{w}{pat}"),
                });
            }
        }
        // 具体某天：上个月5号
        if let Some((d, dl)) = number(src, k) {
            if (1..=31).contains(&d) && (starts(src, k + dl, "日") || starts(src, k + dl, "号")) {
                let day = NaiveDate::from_ymd_opt(base.year(), base.month(), d as u32)?;
                return Some(Side {
                    len: k + dl + 1 - i,
                    start: day,
                    end: day + Duration::days(1),
                    label: format!("{w}{d}号"),
                });
            }
        }
        return Some(Side { len: k - i, start: base, end, label: w.to_string() });
    }

    // 4) 季度
    if let Some((w, off)) = best(src, i, QUARTERS) {
        let (start, end) = quarter_range(today, off)?;
        return Some(Side {
            len: w.chars().count(),
            start,
            end,
            label: w.to_string(),
        });
    }

    // 5) 半年
    if let Some((w, second)) = HALVES.iter().find(|(w, _)| starts(src, i, w)).copied() {
        let y = today.year();
        let start = NaiveDate::from_ymd_opt(y, if second { 7 } else { 1 }, 1)?;
        return Some(Side {
            len: w.chars().count(),
            start,
            end: start + Months::new(6),
            label: w.to_string(),
        });
    }

    // 6) 年（含「去年底」「今年初」）
    if let Some((w, off)) = best(src, i, YEARS) {
        let wl = w.chars().count();
        let y = today.year() + off as i32;
        for (pat, m) in [("底", 12u32), ("末", 12), ("初", 1)] {
            if starts(src, i + wl, pat) {
                let start = NaiveDate::from_ymd_opt(y, m, 1)?;
                return Some(Side {
                    len: wl + pat.chars().count(),
                    start,
                    end: start + Months::new(1),
                    label: format!("{w}{pat}"),
                });
            }
        }
        let start = NaiveDate::from_ymd_opt(y, 1, 1)?;
        return Some(Side {
            len: wl,
            start,
            end: NaiveDate::from_ymd_opt(y + 1, 1, 1)?,
            label: w.to_string(),
        });
    }

    // 7) 光说「年底 / 年初」：落在未来就按去年算
    for (w, m) in [("年底", 12u32), ("年末", 12), ("年初", 1)] {
        if starts(src, i, w) {
            let mut y = today.year();
            let probe = NaiveDate::from_ymd_opt(y, m, 1)?;
            if probe > today {
                y -= 1;
            }
            let start = NaiveDate::from_ymd_opt(y, m, 1)?;
            return Some(Side {
                len: w.chars().count(),
                start,
                end: start + Months::new(1),
                label: format!("{w}（{y} 年 {m} 月）"),
            });
        }
    }

    None
}

const DAYS: &[(&str, i64)] = &[
    ("大前天", -3),
    ("大后天", 3),
    ("今天", 0),
    ("今日", 0),
    ("当天", 0),
    ("昨天", -1),
    ("昨日", -1),
    ("前天", -2),
    ("明天", 1),
    ("后天", 2),
];

const WEEKS: &[(&str, i64)] = &[
    ("上上星期", -2),
    ("上上个星期", -2),
    ("上上礼拜", -2),
    ("上上周", -2),
    ("上个星期", -1),
    ("上个礼拜", -1),
    ("上星期", -1),
    ("上礼拜", -1),
    ("上周", -1),
    ("这个星期", 0),
    ("这星期", 0),
    ("本星期", 0),
    ("这礼拜", 0),
    ("本礼拜", 0),
    ("这周", 0),
    ("本周", 0),
    ("下个星期", 1),
    ("下星期", 1),
    ("下礼拜", 1),
    ("下周", 1),
];

const MONTHS: &[(&str, i64)] = &[
    ("上上个月", -2),
    ("上上月份", -2),
    ("上上月", -2),
    ("上个月", -1),
    ("上月份", -1),
    ("上月", -1),
    ("这个月", 0),
    ("这月份", 0),
    ("这月", 0),
    ("本月", 0),
    ("当月", 0),
    ("下个月", 1),
    ("下月份", 1),
    ("下月", 1),
];

const QUARTERS: &[(&str, i64)] = &[
    ("上上个季度", -2),
    ("上上季度", -2),
    ("上个季度", -1),
    ("上季度", -1),
    ("这个季度", 0),
    ("这季度", 0),
    ("本季度", 0),
    ("下个季度", 1),
    ("下季度", 1),
];

const HALVES: &[(&str, bool)] = &[("上半年", false), ("下半年", true)];

const YEARS: &[(&str, i64)] = &[
    ("大前年", -3),
    ("前年", -2),
    ("去年", -1),
    ("上年", -1),
    ("今年", 0),
    ("本年", 0),
    ("明年", 1),
    ("后年", 2),
];

fn best<'a>(src: &[char], i: usize, list: &'a [(&'a str, i64)]) -> Option<(&'a str, i64)> {
    let mut hit: Option<(&str, i64)> = None;
    for (w, v) in list {
        if starts(src, i, w) {
            if hit.map_or(true, |(hw, _)| w.chars().count() > hw.chars().count()) {
                hit = Some((*w, *v));
            }
        }
    }
    hit
}

fn weekday_of(c: char) -> Option<i64> {
    Some(match c {
        '一' => 0,
        '二' => 1,
        '三' => 2,
        '四' => 3,
        '五' => 4,
        '六' => 5,
        '日' | '天' => 6,
        _ => return None,
    })
}

fn day_range(d: NaiveDate, _i: usize, len: usize, label: &str) -> Side {
    Side { len, start: d, end: d + Duration::days(1), label: label.to_string() }
}

fn week_start(d: NaiveDate) -> NaiveDate {
    d - Duration::days(d.weekday().num_days_from_monday() as i64)
}

fn month_start_shift(today: NaiveDate, off: i32) -> NaiveDate {
    let first = NaiveDate::from_ymd_opt(today.year(), today.month(), 1).unwrap_or(today);
    if off == 0 {
        first
    } else if off > 0 {
        first + Months::new(off as u32)
    } else {
        first - Months::new((-off) as u32)
    }
}

fn quarter_range(today: NaiveDate, off: i64) -> Option<(NaiveDate, NaiveDate)> {
    let q_now = ((today.month() - 1) / 3) as i64; // 0..3
    let target = q_now + off;
    let year = today.year() + (target.div_euclid(4)) as i32;
    let q = target.rem_euclid(4) as u32;
    let start = NaiveDate::from_ymd_opt(year, q * 3 + 1, 1)?;
    Some((start, start + Months::new(3)))
}

// ============================================================================
// 数量词：最近3天 / 三个月内 / 半年前 / 这三天
// ============================================================================

fn duration_at(src: &[char], i: usize, today: NaiveDate) -> Option<Side> {
    let mut j = i;
    let mut prefixed = false;
    for w in ["最近", "过去", "近", "前", "这"] {
        if starts(src, i, w) {
            j = i + w.chars().count();
            prefixed = true;
            break;
        }
    }

    // 半：半个月 / 半年
    let (num, unit, mut k) = if starts(src, j, "半个月") {
        (15i64, Unit::Day, j + 3)
    } else if starts(src, j, "半年") {
        (6, Unit::Month, j + 2)
    } else {
        let (n, nl) = number(src, j)?;
        let (u, ul) = match_unit(src, j + nl)?;
        (n, u, j + nl + ul)
    };
    if num <= 0 || num > 3650 {
        return None;
    }

    // 后缀：前 / 内 / 后
    let (post, plen) = if starts(src, k, "以前") {
        (Post::Before, 2)
    } else if starts(src, k, "之前") {
        (Post::Before, 2)
    } else if starts(src, k, "前") {
        (Post::Before, 1)
    } else if starts(src, k, "以内") || starts(src, k, "之内") {
        (Post::Since, 2)
    } else if starts(src, k, "内") || starts(src, k, "里") || starts(src, k, "间") {
        (Post::Since, 1)
    } else if starts(src, k, "以后") || starts(src, k, "之后") {
        (Post::After, 2)
    } else if starts(src, k, "后") {
        (Post::After, 1)
    } else {
        (Post::None, 0)
    };
    k += plen;

    match post {
        // 「3 天前」是过去某个点，不是区间
        Post::Before => {
            if prefixed {
                return None;
            }
            point(today, num, unit, i, k)
        }
        // 「3 天后」在文件搜索里没有意义
        Post::After => None,
        // 「3 天内」和「最近 3 天」都成立；光秃秃的「3 天」不算时间
        Post::Since | Post::None => {
            if post == Post::None && !prefixed {
                return None;
            }
            let start = back(today, num, unit)?;
            Some(Side {
                len: k - i,
                start,
                end: today + Duration::days(1),
                label: format!("最近 {}", amount(num, unit)),
            })
        }
    }
}

#[derive(Clone, Copy, PartialEq)]
enum Post {
    Before,
    Since,
    After,
    None,
}

#[derive(Clone, Copy, PartialEq)]
enum Unit {
    Day,
    Week,
    Month,
    Quarter,
    Year,
}

/// 从 `today` 往前退 n 个单位，用于「最近 N 天」的起点
fn back(today: NaiveDate, n: i64, unit: Unit) -> Option<NaiveDate> {
    Some(match unit {
        Unit::Day => today - Duration::days(n),
        Unit::Week => today - Duration::weeks(n),
        Unit::Month => today - Months::new(n as u32),
        Unit::Quarter => today - Months::new((n * 3) as u32),
        Unit::Year => today - Months::new((n * 12) as u32),
    })
}

/// 「3 天前」落到具体区间：天 / 周 / 月 / 季度 / 年 各自取自然边界
fn point(today: NaiveDate, n: i64, unit: Unit, origin: usize, consumed: usize) -> Option<Side> {
    let label = format!("{}前", amount(n, unit));
    let (start, end) = match unit {
        Unit::Day => {
            let d = today - Duration::days(n);
            (d, d + Duration::days(1))
        }
        Unit::Week => {
            let d = week_start(today) - Duration::weeks(n);
            (d, d + Duration::days(7))
        }
        Unit::Month => {
            let d = month_start_shift(today, -(n as i32));
            (d, d + Months::new(1))
        }
        Unit::Quarter => {
            let (s, e) = quarter_range(today, -n)?;
            (s, e)
        }
        Unit::Year => {
            let y = today.year() - n as i32;
            (
                NaiveDate::from_ymd_opt(y, 1, 1)?,
                NaiveDate::from_ymd_opt(y + 1, 1, 1)?,
            )
        }
    };
    Some(Side { len: consumed - origin, start, end, label })
}

fn amount(n: i64, unit: Unit) -> String {
    let u = match unit {
        Unit::Day => "天",
        Unit::Week => "周",
        Unit::Month => "个月",
        Unit::Quarter => "个季度",
        Unit::Year => "年",
    };
    format!("{n} {u}")
}

fn match_unit(src: &[char], i: usize) -> Option<(Unit, usize)> {
    for (pat, u) in [
        ("个季度", Unit::Quarter),
        ("个星期", Unit::Week),
        ("个礼拜", Unit::Week),
        ("个月", Unit::Month),
        ("星期", Unit::Week),
        ("礼拜", Unit::Week),
    ] {
        if starts(src, i, pat) {
            return Some((u, pat.chars().count()));
        }
    }
    match src.get(i)? {
        '天' | '日' => Some((Unit::Day, 1)),
        '周' => Some((Unit::Week, 1)),
        '月' => Some((Unit::Month, 1)),
        '年' => Some((Unit::Year, 1)),
        '季' => Some((Unit::Quarter, 1)),
        _ => None,
    }
}

// ============================================================================
// 数字
// ============================================================================

fn digits(src: &[char], i: usize) -> Option<(i64, usize)> {
    let mut j = i;
    let mut s = String::new();
    while j < src.len() {
        let c = src[j];
        if c.is_ascii_digit() {
            s.push(c);
        } else if ('０'..='９').contains(&c) {
            s.push(char::from(b'0' + (c as u32 - '０' as u32) as u8));
        } else {
            break;
        }
        j += 1;
    }
    if s.is_empty() {
        return None;
    }
    Some((s.parse().ok()?, j - i))
}

/// 阿拉伯数字或中文数字（一 / 两 / 十 / 十五 / 二十三）
fn number(src: &[char], i: usize) -> Option<(i64, usize)> {
    if let Some(v) = digits(src, i) {
        return Some(v);
    }
    cn_number(src, i)
}

fn cn_digit(c: char) -> Option<i64> {
    Some(match c {
        '零' | '〇' => 0,
        '一' => 1,
        '二' | '两' => 2,
        '三' => 3,
        '四' => 4,
        '五' => 5,
        '六' => 6,
        '七' => 7,
        '八' => 8,
        '九' => 9,
        _ => return None,
    })
}

fn cn_number(src: &[char], i: usize) -> Option<(i64, usize)> {
    let mut j = i;
    let mut val = 0i64;
    let mut got = false;

    if src.get(j) == Some(&'十') {
        val = 10;
        j += 1;
        got = true;
        if let Some(u) = src.get(j).and_then(|c| cn_digit(*c)) {
            if u > 0 && u < 10 {
                val += u;
                j += 1;
            }
        }
    } else if let Some(d) = src.get(j).and_then(|c| cn_digit(*c)) {
        if d > 0 && src.get(j + 1) == Some(&'十') {
            val = d * 10;
            j += 2;
            got = true;
            if let Some(u) = src.get(j).and_then(|c| cn_digit(*c)) {
                if u > 0 && u < 10 {
                    val += u;
                    j += 1;
                }
            }
        } else {
            val = d;
            j += 1;
            got = true;
        }
    }
    if got {
        Some((val, j - i))
    } else {
        None
    }
}

// ============================================================================

fn starts(src: &[char], i: usize, pat: &str) -> bool {
    let mut j = i;
    for c in pat.chars() {
        if src.get(j) != Some(&c) {
            return false;
        }
        j += 1;
    }
    true
}
