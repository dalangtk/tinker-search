//! 最小 SQL 执行器：在内存索引上跑 `SELECT ... FROM alltable ...`。
//!
//! 支持的语法（大小写不敏感）：
//!
//! ```sql
//! SELECT [DISTINCT] * | 字段[, 字段...] | COUNT(*) | SUM(Size) ...
//! FROM alltable
//! [WHERE 条件]                    -- = != <> > >= < <= LIKE IN BETWEEN AND OR NOT ()
//! [GROUP BY 字段]
//! [ORDER BY 字段 [ASC|DESC]]
//! [LIMIT n]
//! ```
//!
//! 字段：`Path` / `FName`(Name) / `Ext` / `Size` / `Mtime` / `Ctime` / `Atime` /
//! `Dir`(IsDir) / `Drive` / `Type`，也认中文别名（路径、文件名、后缀、大小、
//! 修改时间、创建时间、访问时间、文件夹、磁盘、类型）。
//!
//! 大小与时间不存索引（见 `store` 的说明），所以 WHERE 会被拆成两段执行：
//! 只用索引列的（路径/名字/后缀/盘/类型）先跑，把候选压下来；要 stat 的
//! （大小/时间）只对剩下的候选跑。候选数量在动手前先跟 `MAX_STAT` 比一下，
//! 超了就直接报错并说明怎么改，而不是让界面卡十分钟。
//!
//! ORDER BY 除了字段名，也接受 `COUNT(*)` 这类聚合（仅分组查询）与 SELECT 里的别名。

use crate::query::{ext_of_bytes, matches_filter, Filter};
use crate::store::Index;
use rayon::prelude::*;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

/// 单次查询允许的最大 stat 次数。
const MAX_STAT: usize = 400_000;

#[derive(Clone, Debug)]
pub enum Value {
    Text(String),
    Num(f64),
    Null,
}

pub struct Table {
    pub fields: Vec<String>,
    /// 1 = 文本，3 = 数字（与 SqlTable.vue 的类型码一致）
    pub types: Vec<u8>,
    pub rows: Vec<Vec<Value>>,
    /// 与 `rows` 一一对应的索引记录号。前端拿它去取完整文件信息
    /// （路径、图标、预览），所以 SELECT 只挑了两列也能双击打开。
    pub ids: Vec<u32>,
    /// 命中总数（分页前）
    pub total: usize,
    pub aggregate: bool,
    pub groups: Vec<Group>,
    /// 分组字段的名字（没写 GROUP BY 时为 None），前端据此区分「分组键」和聚合列
    pub group_label: Option<String>,
}

#[derive(Clone, Debug, Default)]
pub struct Group {
    pub key_text: String,
    pub key_num: i64,
    pub count: usize,
    pub sum: f64,
    pub min: f64,
    pub max: f64,
}

/* ============================== 词法 ============================== */

#[derive(Clone, Debug, PartialEq)]
enum Tok {
    Word(String),
    Str(String),
    /// 数值 + 原始文本（`100M` 这种带单位的写法要留原文）
    Num(f64, String),
    Sym(char),
    Op(&'static str),
}

fn lex(sql: &str) -> Result<Vec<Tok>, String> {
    let b: Vec<char> = sql.chars().collect();
    let mut i = 0usize;
    let mut out = Vec::new();
    while i < b.len() {
        let c = b[i];
        if c.is_whitespace() {
            i += 1;
            continue;
        }
        match c {
            '(' | ')' | ',' | '*' => {
                out.push(Tok::Sym(c));
                i += 1;
            }
            '\'' | '"' | '`' => {
                let quote = c;
                i += 1;
                let mut s = String::new();
                while i < b.len() && b[i] != quote {
                    if b[i] == '\\' && i + 1 < b.len() {
                        i += 1;
                    }
                    s.push(b[i]);
                    i += 1;
                }
                if i >= b.len() {
                    return Err("字符串没有闭合的引号".into());
                }
                i += 1;
                out.push(Tok::Str(s));
            }
            '=' | '!' | '<' | '>' => {
                let next = b.get(i + 1).copied();
                let (op, len) = match (c, next) {
                    ('=', Some('=')) => ("=", 2),
                    ('!', Some('=')) => ("!=", 2),
                    ('<', Some('>')) => ("!=", 2),
                    ('<', Some('=')) => ("<=", 2),
                    ('>', Some('=')) => (">=", 2),
                    ('=', _) => ("=", 1),
                    ('<', _) => ("<", 1),
                    ('>', _) => (">", 1),
                    _ => return Err("无法识别的运算符 !".into()),
                };
                out.push(Tok::Op(op));
                i += len;
            }
            _ if c.is_ascii_digit()
                || (c == '.' && i + 1 < b.len() && b[i + 1].is_ascii_digit())
                || (c == '-'
                    && i + 1 < b.len()
                    && b[i + 1].is_ascii_digit()
                    && matches!(
                        out.last(),
                        None | Some(Tok::Op(_)) | Some(Tok::Sym('(')) | Some(Tok::Sym(','))
                    )) =>
            {
                let start = i;
                if c == '-' {
                    i += 1;
                }
                while i < b.len() && (b[i].is_ascii_digit() || b[i] == '.') {
                    i += 1;
                }
                // 单位后缀：100M / 1.5GB / 3kb
                while i < b.len() && b[i].is_ascii_alphabetic() {
                    i += 1;
                }
                let raw: String = b[start..i].iter().collect();
                let num = lit_num(&raw).unwrap_or(0.0);
                out.push(Tok::Num(num, raw));
            }
            _ => {
                let start = i;
                while i < b.len()
                    && !b[i].is_whitespace()
                    && !matches!(b[i], '(' | ')' | ',' | '*' | '=' | '!' | '<' | '>' | '\'' | '"')
                {
                    i += 1;
                }
                if start == i {
                    return Err(format!("无法识别的字符 {c}"));
                }
                let w: String = b[start..i].iter().collect();
                out.push(Tok::Word(w));
            }
        }
    }
    Ok(out)
}

/// 数值字面量：支持 K/M/G/T 单位与 `2026-01-01[ 10:00:00]` 日期。
fn lit_num(s: &str) -> Option<f64> {
    let t = s.trim();
    if t.is_empty() {
        return None;
    }
    if t.len() >= 8 && t.chars().filter(|c| *c == '-').count() >= 2 {
        // 形如 2026-01-01 的日期
        if let Some(v) = parse_time(t) {
            return Some(v as f64);
        }
    }
    let lower = t.to_ascii_lowercase();
    let (num_part, scale) = if let Some(p) = lower.strip_suffix("tb").or_else(|| lower.strip_suffix('t')) {
        (p, 1024f64.powi(4))
    } else if let Some(p) = lower.strip_suffix("gb").or_else(|| lower.strip_suffix('g')) {
        (p, 1024f64.powi(3))
    } else if let Some(p) = lower.strip_suffix("mb").or_else(|| lower.strip_suffix('m')) {
        (p, 1024f64.powi(2))
    } else if let Some(p) = lower
        .strip_suffix("kb")
        .or_else(|| lower.strip_suffix('k'))
        .or_else(|| lower.strip_suffix('b'))
    {
        (p, 1024f64)
    } else {
        (lower.as_str(), 1.0)
    };
    num_part.trim().parse::<f64>().ok().map(|v| v * scale)
}

/// `2026-01-01` / `2026/01/01` / `2026-01-01 10:00(:00)` → Unix 秒。
fn parse_time(s: &str) -> Option<i64> {
    let norm = s.trim().replace('/', "-");
    let (date, time) = match norm.split_once(' ') {
        Some((d, t)) => (d, t),
        None => (norm.as_str(), ""),
    };
    let mut it = date.split('-');
    let y: i64 = it.next()?.parse().ok()?;
    let m: i64 = it.next()?.parse().ok()?;
    let d: i64 = it.next()?.parse().ok()?;
    if !(1..=12).contains(&m) || !(1..=31).contains(&d) {
        return None;
    }
    let days = days_from_civil(y, m, d);
    let mut secs = days * 86400;
    if !time.is_empty() {
        let mut tp = time.split(':');
        let hh: i64 = tp.next().and_then(|v| v.parse().ok()).unwrap_or(0);
        let mm: i64 = tp.next().and_then(|v| v.parse().ok()).unwrap_or(0);
        let ss: i64 = tp.next().and_then(|v| v.parse().ok()).unwrap_or(0);
        secs += hh * 3600 + mm * 60 + ss;
    }
    Some(secs)
}

/// Howard Hinnant 的 days_from_civil：公历 → 1970 起的天数。
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146097 + doe - 719468
}

/* ============================== 语法 ============================== */

#[derive(Clone, Copy, PartialEq, Debug)]
enum F {
    Path,
    Name,
    Ext,
    Size,
    Mtime,
    Ctime,
    Atime,
    Dir,
    Drive,
    Type,
}

impl F {
    fn parse(s: &str) -> Option<F> {
        let k = s.to_ascii_lowercase();
        Some(match k.as_str() {
            "path" | "fullpath" | "路径" | "完整路径" => F::Path,
            "fname" | "name" | "filename" | "文件名" | "名称" => F::Name,
            "ext" | "extension" | "后缀" | "扩展名" => F::Ext,
            "size" | "filesize" | "大小" | "文件大小" => F::Size,
            "mtime" | "modified" | "修改时间" => F::Mtime,
            "ctime" | "created" | "创建时间" => F::Ctime,
            "atime" | "accessed" | "访问时间" => F::Atime,
            "dir" | "isdir" | "文件夹" | "目录" => F::Dir,
            "drive" | "vol" | "volume" | "磁盘" | "盘符" => F::Drive,
            "type" | "类型" => F::Type,
            _ => return None,
        })
    }

    fn label(self) -> &'static str {
        match self {
            F::Path => "Path",
            F::Name => "FName",
            F::Ext => "Ext",
            F::Size => "Size",
            F::Mtime => "Mtime",
            F::Ctime => "Ctime",
            F::Atime => "Atime",
            F::Dir => "Dir",
            F::Drive => "Drive",
            F::Type => "Type",
        }
    }

    fn numeric(self) -> bool {
        matches!(self, F::Size | F::Mtime | F::Ctime | F::Atime | F::Dir)
    }
}

#[derive(Clone, Copy, PartialEq)]
enum CmpOp {
    Eq,
    Ne,
    Gt,
    Ge,
    Lt,
    Le,
}

#[derive(Clone)]
enum Expr {
    Cmp(F, CmpOp, String),
    Like(F, String, bool),
    In(F, Vec<String>, bool),
    Between(F, String, String, bool),
    IsNull(F, bool),
    And(Box<Expr>, Box<Expr>),
    Or(Box<Expr>, Box<Expr>),
    Not(Box<Expr>),
}

#[derive(Clone, Copy, PartialEq)]
enum Agg {
    Count,
    Sum,
    Avg,
    Min,
    Max,
}

fn agg_of(name: &str) -> Option<Agg> {
    Some(match name.to_ascii_lowercase().as_str() {
        "count" => Agg::Count,
        "sum" => Agg::Sum,
        "avg" | "average" => Agg::Avg,
        "min" => Agg::Min,
        "max" => Agg::Max,
        _ => return None,
    })
}

impl Agg {
    fn label(self) -> &'static str {
        match self {
            Agg::Count => "COUNT",
            Agg::Sum => "SUM",
            Agg::Avg => "AVG",
            Agg::Min => "MIN",
            Agg::Max => "MAX",
        }
    }

    /// 这个聚合要读文件大小/时间吗。COUNT 不用，所以 COUNT(*) 再快不过。
    fn needs_stat(self) -> bool {
        !matches!(self, Agg::Count)
    }
}

fn agg_label(a: Agg) -> &'static str {
    match a {
        Agg::Count => "COUNT",
        Agg::Sum => "SUM",
        Agg::Avg => "AVG",
        Agg::Min => "MIN",
        Agg::Max => "MAX",
    }
}

enum Proj {
    Field(F, String),
    Agg(Agg, Option<F>, String),
}

struct Stmt {
    proj: Vec<Proj>,
    star: bool,
    distinct: bool,
    filter: Option<Expr>,
    group: Option<F>,
    order: Option<Ord>,
    limit: Option<usize>,
}

/// ORDER BY 的排序键。分组查询要能按 `COUNT(*)` 或 SELECT 里起的别名排，
/// 所以不能只记一个字段名。
#[derive(Clone, Copy)]
struct Ord {
    key: OrdKey,
    asc: bool,
}

#[derive(Clone, Copy)]
enum OrdKey {
    Field(F),
    Agg(Agg),
}

struct Parser {
    t: Vec<Tok>,
    i: usize,
}

impl Parser {
    fn peek(&self) -> Option<&Tok> {
        self.t.get(self.i)
    }

    fn at(&self, n: usize) -> Option<&Tok> {
        self.t.get(self.i + n)
    }

    fn word(&self) -> Option<String> {
        match self.peek() {
            Some(Tok::Word(w)) => Some(w.clone()),
            _ => None,
        }
    }

    fn is_kw(&self, k: &str) -> bool {
        self.word().map(|w| w.eq_ignore_ascii_case(k)).unwrap_or(false)
    }

    fn eat_kw(&mut self, k: &str) -> bool {
        if self.is_kw(k) {
            self.i += 1;
            true
        } else {
            false
        }
    }

    fn expect_kw(&mut self, k: &str) -> Result<(), String> {
        if self.eat_kw(k) {
            Ok(())
        } else {
            Err(format!("这里应该是 {k}，实际读到「{}」", self.show()))
        }
    }

    fn show(&self) -> String {
        match self.peek() {
            Some(Tok::Word(w)) => w.clone(),
            Some(Tok::Str(s)) => format!("'{s}'"),
            Some(Tok::Num(_, r)) => r.clone(),
            Some(Tok::Sym(c)) => c.to_string(),
            Some(Tok::Op(o)) => (*o).to_string(),
            None => "语句结尾".into(),
        }
    }

    fn sym(&mut self, c: char) -> bool {
        if let Some(Tok::Sym(x)) = self.peek() {
            if *x == c {
                self.i += 1;
                return true;
            }
        }
        false
    }

    fn parse(&mut self) -> Result<Stmt, String> {
        self.expect_kw("select")?;
        let distinct = self.eat_kw("distinct");
        let mut proj: Vec<Proj> = Vec::new();
        let mut star = false;
        loop {
            if self.sym('*') {
                star = true;
            } else {
                let name = self.show();
                // 聚合函数：COUNT( * ) / SUM(Size) ...
                if agg_of(&name).is_some() && matches!(self.at(1), Some(Tok::Sym('('))) {
                    let a = agg_of(&name).unwrap();
                    self.i += 2;
                    let inner = if self.sym('*') {
                        None
                    } else {
                        let n2 = self.show();
                        let f2 = F::parse(&n2)
                            .ok_or_else(|| format!("聚合函数里的字段 {n2} 不认识"))?;
                        self.i += 1;
                        Some(f2)
                    };
                    if !self.sym(')') {
                        return Err("聚合函数缺少右括号".into());
                    }
                    let mut label = match inner {
                        Some(f) => format!("{}({})", agg_label(a), f.label()),
                        None => format!("{}(*)", agg_label(a)),
                    };
                    if self.eat_kw("as") {
                        label = self.alias()?;
                    }
                    proj.push(Proj::Agg(a, inner, label));
                } else {
                    let f = F::parse(&name).ok_or_else(|| {
                        format!(
                            "不认识的字段 {name}（可用：Path / FName / Ext / Size / Mtime / Ctime / Atime / Dir / Drive / Type）"
                        )
                    })?;
                    self.i += 1;
                    let mut alias = f.label().to_string();
                    if self.eat_kw("as") {
                        alias = self.alias()?;
                    }
                    proj.push(Proj::Field(f, alias));
                }
            }
            if !self.sym(',') {
                break;
            }
        }
        self.expect_kw("from")?;
        if self.peek().is_none() {
            return Err("FROM 后面缺少表名（本引擎固定为 alltable）".into());
        }
        self.i += 1;

        let mut filter = None;
        if self.eat_kw("where") {
            filter = Some(self.expr()?);
        }
        let mut group = None;
        if self.is_kw("group") {
            self.i += 1;
            self.expect_kw("by")?;
            let n = self.show();
            group = Some(F::parse(&n).ok_or_else(|| format!("GROUP BY 的字段 {n} 不认识"))?);
            self.i += 1;
        }
        let mut order = None;
        if self.is_kw("order") {
            self.i += 1;
            self.expect_kw("by")?;
            let key = self.order_key(&proj)?;
            let asc = !self.eat_kw("desc");
            let _ = self.eat_kw("asc");
            order = Some(Ord { key, asc });
        }
        let mut limit = None;
        if self.eat_kw("limit") {
            if let Some(Tok::Num(n, _)) = self.peek() {
                limit = Some((*n).max(0.0) as usize);
                self.i += 1;
            } else {
                return Err("LIMIT 后面应该是数字".into());
            }
        }
        if self.i < self.t.len() {
            return Err(format!(
                "语句在「{}」之后多出了内容（只支持单条 SELECT）",
                self.show()
            ));
        }
        Ok(Stmt { proj, star, distinct, filter, group, order, limit })
    }

    /// 解析 ORDER BY 的排序键：字段名、聚合函数，或 SELECT 里起的别名。
    fn order_key(&mut self, proj: &[Proj]) -> Result<OrdKey, String> {
        let raw = self.show();
        // COUNT(*) / SUM(Size) / AVG(Size) …：括号里写什么都行，
        // 分组阶段已经把各聚合值算好了。
        if let Some(a) = agg_of(&raw) {
            if matches!(self.at(1), Some(Tok::Sym('('))) {
                self.i += 2;
                let mut depth = 1;
                loop {
                    match self.peek() {
                        Some(Tok::Sym('(')) => {
                            depth += 1;
                            self.i += 1;
                        }
                        Some(Tok::Sym(')')) => {
                            depth -= 1;
                            self.i += 1;
                        }
                        Some(_) => self.i += 1,
                        None => return Err("ORDER BY 里函数的括号没闭合".into()),
                    }
                    if depth == 0 {
                        break;
                    }
                }
                return Ok(OrdKey::Agg(a));
            }
        }
        if let Some(f) = F::parse(&raw) {
            self.i += 1;
            return Ok(OrdKey::Field(f));
        }
        // 别名：SELECT Ext, COUNT(*) AS n … ORDER BY n DESC
        if let Some(p) = proj.iter().find(|p| match p {
            Proj::Field(_, a) | Proj::Agg(_, _, a) => a.eq_ignore_ascii_case(&raw),
        }) {
            self.i += 1;
            return Ok(match p {
                Proj::Field(f, _) => OrdKey::Field(*f),
                Proj::Agg(a, _, _) => OrdKey::Agg(*a),
            });
        }
        Err(format!(
            "ORDER BY 的「{raw}」不认识（可写字段名、COUNT(*) 这类聚合，或 SELECT 里起的别名）"
        ))
    }

    fn alias(&mut self) -> Result<String, String> {
        match self.peek().cloned() {
            Some(Tok::Word(w)) | Some(Tok::Str(w)) => {
                self.i += 1;
                Ok(w)
            }
            _ => Err("AS 后面应该是别名".into()),
        }
    }

    fn expr(&mut self) -> Result<Expr, String> {
        let mut left = self.and_expr()?;
        while self.eat_kw("or") {
            let right = self.and_expr()?;
            left = Expr::Or(Box::new(left), Box::new(right));
        }
        Ok(left)
    }

    fn and_expr(&mut self) -> Result<Expr, String> {
        let mut left = self.unary()?;
        while self.eat_kw("and") {
            let right = self.unary()?;
            left = Expr::And(Box::new(left), Box::new(right));
        }
        Ok(left)
    }

    fn unary(&mut self) -> Result<Expr, String> {
        if self.eat_kw("not") {
            return Ok(Expr::Not(Box::new(self.unary()?)));
        }
        if self.sym('(') {
            let e = self.expr()?;
            if !self.sym(')') {
                return Err("条件缺少右括号".into());
            }
            return Ok(e);
        }
        let name = self.show();
        let f = F::parse(&name).ok_or_else(|| format!("条件左边应该是字段，实际读到「{name}」"))?;
        self.i += 1;
        let negate = self.eat_kw("not");
        if self.eat_kw("like") {
            let v = self.value_text()?;
            return Ok(Expr::Like(f, v, negate));
        }
        if self.eat_kw("in") {
            if !self.sym('(') {
                return Err("IN 后面应该跟括号".into());
            }
            let mut list = Vec::new();
            loop {
                list.push(self.value_text()?);
                if !self.sym(',') {
                    break;
                }
            }
            if !self.sym(')') {
                return Err("IN 列表缺少右括号".into());
            }
            return Ok(Expr::In(f, list, negate));
        }
        if self.eat_kw("between") {
            let a = self.value_text()?;
            self.expect_kw("and")?;
            let b = self.value_text()?;
            return Ok(Expr::Between(f, a, b, negate));
        }
        if self.eat_kw("is") {
            let neg = self.eat_kw("not");
            self.expect_kw("null")?;
            return Ok(Expr::IsNull(f, neg));
        }
        if negate {
            return Err("NOT 后面只能跟 LIKE / IN / BETWEEN / NULL".into());
        }
        let op = match self.peek() {
            Some(Tok::Op(o)) => {
                let o = *o;
                self.i += 1;
                o
            }
            _ => return Err(format!("字段 {} 后面缺少比较运算符", f.label())),
        };
        let v = self.value_text()?;
        let cmp = match op {
            "=" => CmpOp::Eq,
            "!=" => CmpOp::Ne,
            ">" => CmpOp::Gt,
            ">=" => CmpOp::Ge,
            "<" => CmpOp::Lt,
            "<=" => CmpOp::Le,
            _ => return Err(format!("不支持的运算符 {op}")),
        };
        Ok(Expr::Cmp(f, cmp, v))
    }

    fn value_text(&mut self) -> Result<String, String> {
        match self.peek().cloned() {
            Some(Tok::Str(s)) => {
                self.i += 1;
                Ok(s)
            }
            Some(Tok::Num(_, raw)) => {
                self.i += 1;
                Ok(raw)
            }
            Some(Tok::Word(w)) => {
                self.i += 1;
                Ok(w)
            }
            _ => Err("这里应该是一个值（字符串或数字）".into()),
        }
    }
}

/* ============================== 求值 ============================== */

#[derive(Clone, Copy)]
struct Meta {
    size: i64,
    mtime: i64,
    ctime: i64,
    atime: i64,
}

struct Ctx<'a> {
    idx: &'a Index,
    id: u32,
    meta: Option<Option<Meta>>,
    stats: &'a AtomicUsize,
    capped: &'a AtomicBool,
}

impl<'a> Ctx<'a> {
    fn new(idx: &'a Index, id: u32, stats: &'a AtomicUsize, capped: &'a AtomicBool) -> Self {
        Ctx { idx, id, meta: None, stats, capped }
    }

    fn meta(&mut self) -> Option<Meta> {
        if let Some(m) = self.meta {
            return m;
        }
        if self.stats.fetch_add(1, Ordering::Relaxed) >= MAX_STAT {
            self.capped.store(true, Ordering::Relaxed);
            self.meta = Some(None);
            return None;
        }
        let path = self.idx.path_of(self.id);
        let m = std::fs::metadata(&path).ok().map(|md| Meta {
            size: md.len() as i64,
            mtime: epoch(md.modified()),
            ctime: epoch(md.created()),
            atime: epoch(md.accessed()),
        });
        self.meta = Some(m);
        m
    }

    fn text(&mut self, f: F) -> String {
        match f {
            F::Path => self.idx.path_of(self.id),
            F::Name => self.idx.name(self.id).to_string(),
            F::Ext => String::from_utf8_lossy(ext_of_bytes(self.idx.name_bytes(self.id))).into_owned(),
            F::Drive => self
                .idx
                .volumes
                .get(self.idx.vol[self.id as usize] as usize)
                .cloned()
                .unwrap_or_default(),
            F::Type => {
                if self.idx.is_dir(self.id) {
                    "文件夹".into()
                } else {
                    String::from_utf8_lossy(ext_of_bytes(self.idx.name_bytes(self.id)))
                        .to_ascii_uppercase()
                }
            }
            _ => String::new(),
        }
    }

    fn num(&mut self, f: F) -> f64 {
        match f {
            F::Dir => {
                if self.idx.is_dir(self.id) {
                    1.0
                } else {
                    0.0
                }
            }
            F::Size => self.meta().map(|m| m.size as f64).unwrap_or(-1.0),
            F::Mtime => self.meta().map(|m| m.mtime as f64).unwrap_or(-1.0),
            F::Ctime => self.meta().map(|m| m.ctime as f64).unwrap_or(-1.0),
            F::Atime => self.meta().map(|m| m.atime as f64).unwrap_or(-1.0),
            _ => 0.0,
        }
    }
}

fn str_eq(a: &str, b: &str) -> bool {
    a.eq_ignore_ascii_case(b)
}

fn like_match(text: &str, pat: &str) -> bool {
    // 通配符：% = 任意串，_ = 单字符
    let t: Vec<char> = text.chars().flat_map(|c| c.to_lowercase()).collect();
    let p: Vec<char> = pat.chars().flat_map(|c| c.to_lowercase()).collect();
    let (mut ti, mut pi) = (0usize, 0usize);
    let (mut star, mut mark) = (None::<usize>, 0usize);
    while ti < t.len() {
        if pi < p.len() && (p[pi] == '_' || p[pi] == t[ti]) {
            ti += 1;
            pi += 1;
        } else if pi < p.len() && p[pi] == '%' {
            star = Some(pi);
            mark = ti;
            pi += 1;
        } else if let Some(sp) = star {
            pi = sp + 1;
            mark += 1;
            ti = mark;
        } else {
            return false;
        }
    }
    while pi < p.len() && p[pi] == '%' {
        pi += 1;
    }
    pi == p.len()
}

/// 这个字段要不要碰磁盘。大小与时间不在索引里，读一次就得 stat 一个文件，
/// 所以它们在 WHERE 里是最贵的那部分条件。
fn stat_field(f: F) -> bool {
    matches!(f, F::Size | F::Mtime | F::Ctime | F::Atime)
}

fn expr_needs_stat(e: &Expr) -> bool {
    match e {
        Expr::Cmp(f, _, _)
        | Expr::Like(f, _, _)
        | Expr::In(f, _, _)
        | Expr::Between(f, _, _, _)
        | Expr::IsNull(f, _) => stat_field(*f),
        Expr::And(a, b) | Expr::Or(a, b) => expr_needs_stat(a) || expr_needs_stat(b),
        Expr::Not(a) => expr_needs_stat(a),
    }
}

/// 把顶层 AND 摊平成一张条件表——只有平摊之后才能把「不花钱的」先跑、
/// 「要 stat 的」留到候选已经很少的时候再跑。
fn flatten_and(e: Expr, out: &mut Vec<Expr>) {
    match e {
        Expr::And(a, b) => {
            flatten_and(*a, out);
            flatten_and(*b, out);
        }
        other => out.push(other),
    }
}

fn eval(e: &Expr, c: &mut Ctx) -> bool {
    match e {
        Expr::And(a, b) => eval(a, c) && eval(b, c),
        Expr::Or(a, b) => eval(a, c) || eval(b, c),
        Expr::Not(a) => !eval(a, c),
        Expr::Cmp(f, op, raw) => {
            if f.numeric() {
                let lhs = c.num(*f);
                let rhs = if *f == F::Dir {
                    match raw.to_ascii_lowercase().as_str() {
                        "true" | "yes" | "文件夹" | "目录" => 1.0,
                        "false" | "no" => 0.0,
                        _ => lit_num(raw).unwrap_or(f64::NAN),
                    }
                } else {
                    lit_num(raw).unwrap_or(f64::NAN)
                };
                if rhs.is_nan() || lhs < 0.0 {
                    return false;
                }
                match op {
                    CmpOp::Eq => (lhs - rhs).abs() < 0.5,
                    CmpOp::Ne => (lhs - rhs).abs() >= 0.5,
                    CmpOp::Gt => lhs > rhs,
                    CmpOp::Ge => lhs >= rhs,
                    CmpOp::Lt => lhs < rhs,
                    CmpOp::Le => lhs <= rhs,
                }
            } else {
                let lhs = c.text(*f);
                let ord = lhs.to_lowercase().cmp(&raw.to_lowercase());
                match op {
                    CmpOp::Eq => str_eq(&lhs, raw),
                    CmpOp::Ne => !str_eq(&lhs, raw),
                    CmpOp::Gt => ord == std::cmp::Ordering::Greater,
                    CmpOp::Ge => ord != std::cmp::Ordering::Less,
                    CmpOp::Lt => ord == std::cmp::Ordering::Less,
                    CmpOp::Le => ord != std::cmp::Ordering::Greater,
                }
            }
        }
        Expr::Like(f, pat, neg) => {
            let hit = like_match(&c.text(*f), pat);
            hit != *neg
        }
        Expr::In(f, list, neg) => {
            let hit = if f.numeric() {
                let lhs = c.num(*f);
                list.iter()
                    .any(|s| lit_num(s).map(|v| (v - lhs).abs() < 0.5).unwrap_or(false))
            } else {
                let lhs = c.text(*f);
                list.iter().any(|s| str_eq(&lhs, s))
            };
            hit != *neg
        }
        Expr::Between(f, a, b, neg) => {
            let hit = if f.numeric() {
                let v = c.num(*f);
                let lo = lit_num(a).unwrap_or(f64::NAN);
                let hi = lit_num(b).unwrap_or(f64::NAN);
                v >= lo && v <= hi
            } else {
                let v = c.text(*f).to_lowercase();
                v >= a.to_lowercase() && v <= b.to_lowercase()
            };
            hit != *neg
        }
        Expr::IsNull(f, neg) => {
            let null = if f.numeric() { c.num(*f) < 0.0 } else { c.text(*f).is_empty() };
            null != *neg
        }
    }
}

fn epoch(t: std::io::Result<std::time::SystemTime>) -> i64 {
    t.ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// 本地时间格式化（把时间列展示成人能看的字符串）。
fn fmt_time(secs: i64) -> String {
    if secs <= 0 {
        return String::new();
    }
    #[cfg(windows)]
    {
        if let Some(s) = win_time::format(secs) {
            return s;
        }
    }
    secs.to_string()
}

#[cfg(windows)]
mod win_time {
    #[repr(C)]
    struct FileTime {
        low: u32,
        high: u32,
    }
    #[repr(C)]
    #[derive(Default)]
    struct SystemTime {
        year: u16,
        month: u16,
        dow: u16,
        day: u16,
        hour: u16,
        minute: u16,
        second: u16,
        ms: u16,
    }
    extern "system" {
        fn FileTimeToSystemTime(lpfiletime: *const FileTime, lpsystemtime: *mut SystemTime) -> i32;
    }

    pub fn format(secs: i64) -> Option<String> {
        // FILETIME 起点 1601-01-01，单位 100ns
        let ticks = (secs as i128 + 11_644_473_600) * 10_000_000;
        if ticks <= 0 || ticks > u64::MAX as i128 {
            return None;
        }
        let t = ticks as u64;
        let ft = FileTime { low: (t & 0xFFFF_FFFF) as u32, high: (t >> 32) as u32 };
        let mut st = SystemTime::default();
        let ok = unsafe { FileTimeToSystemTime(&ft, &mut st) };
        if ok == 0 {
            return None;
        }
        Some(format!(
            "{:04}-{:02}-{:02} {:02}:{:02}",
            st.year, st.month, st.day, st.hour, st.minute
        ))
    }
}

/* ============================== 执行 ============================== */

pub fn run(idx: &Index, sql: &str, filter: &Filter, limit: usize) -> Result<Table, String> {
    let toks = lex(sql)?;
    if toks.is_empty() {
        return Err("请输入 SQL，例如 SELECT Path, Size FROM alltable WHERE Size > '100M'".into());
    }
    let mut p = Parser { t: toks, i: 0 };
    let stmt = p.parse()?;

    // WHERE 拆成两段：`pre` 只用索引里有的列（路径/名字/后缀/盘/类型），
    // `post` 才去读大小与时间。先跑 pre 把候选压下来，post 的 stat 次数自然就少了——
    // 否则一个 `Size > '1G'` 会把整块盘 stat 一遍。
    let mut pre: Vec<Expr> = Vec::new();
    let mut post: Vec<Expr> = Vec::new();
    if let Some(w) = stmt.filter.clone() {
        let mut all = Vec::new();
        flatten_and(w, &mut all);
        for one in all {
            if expr_needs_stat(&one) {
                post.push(one);
            } else {
                pre.push(one);
            }
        }
    }

    let stats = AtomicUsize::new(0);
    let capped = AtomicBool::new(false);

    // ---- 1. 扫描：先过分类条件，再过 WHERE ----
    let n = idx.len() as u32;
    const CHUNK: u32 = 32 * 1024;
    let chunks = n.div_ceil(CHUNK).max(1);
    // map + collect 保留分块顺序，省掉整体排序
    let parts: Vec<Vec<u32>> = (0..chunks)
        .into_par_iter()
        .map(|c| {
            let mut acc = Vec::new();
            let start = c * CHUNK;
            let end = ((c + 1) * CHUNK).min(n);
            let mut ctx = Ctx::new(idx, 0, &stats, &capped);
            for id in start..end {
                if !idx.is_visible(id) {
                    continue;
                }
                if !matches_filter(idx, id, filter) {
                    continue;
                }
                ctx.id = id;
                ctx.meta = None;
                if pre.iter().all(|w| eval(w, &mut ctx)) {
                    acc.push(id);
                }
            }
            acc
        })
        .collect();
    let ids: Vec<u32> = parts.into_iter().flatten().collect();

    // ---- 1b. 要 stat 的条件放到第二段。候选太多就直接把账算给用户看，
    // 而不是先埋头 stat 十秒再报错。 ----
    let ids: Vec<u32> = if post.is_empty() {
        ids
    } else {
        if ids.len() > MAX_STAT {
            return Err(format!(
                "还要读 {} 个候选的大小/时间，超过 {} 这个上限。先加文件名、后缀或路径条件把候选压下来再查。",
                ids.len(),
                MAX_STAT
            ));
        }
        let kept: Vec<Vec<u32>> = ids
            .par_chunks(8192)
            .map(|sl| {
                let mut acc = Vec::new();
                let mut ctx = Ctx::new(idx, 0, &stats, &capped);
                for &id in sl {
                    ctx.id = id;
                    ctx.meta = None;
                    if post.iter().all(|w| eval(w, &mut ctx)) {
                        acc.push(id);
                    }
                }
                acc
            })
            .collect();
        kept.into_iter().flatten().collect()
    };

    if capped.load(Ordering::Relaxed) {
        return Err(format!(
            "这条查询要读取超过 {} 个文件的大小/时间，太慢。请先加文件名、后缀或路径条件缩小范围。",
            MAX_STAT
        ));
    }

    let total = ids.len();
    let has_agg = stmt.proj.iter().any(|p| matches!(p, Proj::Agg(..)));

    // ---- 2. 聚合 ----
    if has_agg || stmt.group.is_some() {
        return run_aggregate(idx, &stmt, &ids, total, limit);
    }

    // ---- 3. 排序 ----
    let mut ids = ids;
    if let Some(o) = stmt.order {
        match o.key {
            OrdKey::Field(f) => {
                sort_ids(idx, &mut ids, f, o.asc, &stats, &capped)?;
                if capped.load(Ordering::Relaxed) {
                    return Err(format!(
                        "排序要读取超过 {} 个文件的大小/时间，太慢。请先用 WHERE 缩小范围。",
                        MAX_STAT
                    ));
                }
            }
            // 聚合排序只对分组结果有意义，这里没有分组
            OrdKey::Agg(a) => {
                return Err(format!(
                    "ORDER BY {}(…) 只能和 GROUP BY 一起用",
                    a.label()
                ))
            }
        }
    }

    // ---- 4. 投影 ----
    let (fields, types) = match stmt.star {
        true => {
            let fs = [F::Path, F::Name, F::Size, F::Mtime, F::Type];
            (
                fs.iter().map(|f| f.label().to_string()).collect::<Vec<_>>(),
                fs.iter().map(|f| if f.numeric() { 3u8 } else { 1 }).collect::<Vec<_>>(),
            )
        }
        false => {
            let mut fields = Vec::new();
            let mut types = Vec::new();
            for p in &stmt.proj {
                match p {
                    Proj::Field(f, alias) => {
                        fields.push(alias.clone());
                        types.push(if f.numeric() { 3u8 } else { 1 });
                    }
                    Proj::Agg(_, _, alias) => {
                        fields.push(alias.clone());
                        types.push(3);
                    }
                }
            }
            (fields, types)
        }
    };

    let take = stmt.limit.unwrap_or(limit).min(ids.len());
    let mut rows: Vec<Vec<Value>> = Vec::with_capacity(take.min(4096));
    let mut row_ids: Vec<u32> = Vec::with_capacity(take.min(4096));
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut ctx = Ctx::new(idx, 0, &stats, &capped);
    for &id in ids.iter().take(take) {
        ctx.id = id;
        ctx.meta = None;
        let row = project_row(&mut ctx, &stmt);
        if stmt.distinct {
            let key = row
                .iter()
                .map(|v| match v {
                    Value::Text(s) => s.clone(),
                    Value::Num(x) => format!("{x:.0}"),
                    Value::Null => String::new(),
                })
                .collect::<Vec<_>>()
                .join("\u{1}");
            if !seen.insert(key) {
                continue;
            }
        }
        rows.push(row);
        row_ids.push(id);
    }

    Ok(Table {
        fields,
        types,
        rows,
        ids: row_ids,
        total,
        aggregate: false,
        groups: Vec::new(),
        group_label: None,
    })
}

fn project_row(ctx: &mut Ctx, stmt: &Stmt) -> Vec<Value> {
    let mut out = Vec::with_capacity(if stmt.star { 5 } else { stmt.proj.len() });
    if stmt.star {
        for f in [F::Path, F::Name, F::Size, F::Mtime, F::Type] {
            out.push(field_value(ctx, f));
        }
        return out;
    }
    for p in &stmt.proj {
        match p {
            Proj::Field(f, _) => out.push(field_value(ctx, *f)),
            Proj::Agg(..) => out.push(Value::Null),
        }
    }
    out
}

fn field_value(ctx: &mut Ctx, f: F) -> Value {
    match f {
        F::Size => {
            let v = ctx.num(F::Size);
            if v < 0.0 {
                Value::Null
            } else {
                Value::Num(v)
            }
        }
        F::Mtime | F::Ctime | F::Atime => {
            let v = ctx.num(f);
            if v <= 0.0 {
                Value::Null
            } else {
                Value::Text(fmt_time(v as i64))
            }
        }
        F::Dir => Value::Num(ctx.num(F::Dir)),
        _ => Value::Text(ctx.text(f)),
    }
}

fn sort_ids(
    idx: &Index,
    ids: &mut Vec<u32>,
    f: F,
    asc: bool,
    stats: &AtomicUsize,
    capped: &AtomicBool,
) -> Result<(), String> {
    use std::cmp::Ordering as O;
    let _ = asc;
    match f {
        F::Size | F::Mtime | F::Ctime | F::Atime => {
            let mut keys: Vec<(u32, f64)> = ids
                .par_iter()
                .map(|id| {
                    let mut ctx = Ctx::new(idx, *id, stats, capped);
                    (*id, ctx.num(f))
                })
                .collect();
            keys.par_sort_by(|a, b| {
                let o = a.1.partial_cmp(&b.1).unwrap_or(O::Equal);
                if asc {
                    o
                } else {
                    o.reverse()
                }
            });
            for (i, (id, _)) in keys.into_iter().enumerate() {
                ids[i] = id;
            }
        }
        F::Path => {
            if ids.len() > 300_000 {
                return Err("结果太多，按路径排序会很慢。请先用 WHERE 缩小范围，或按 FName 排序。".into());
            }
            let mut keys: Vec<(u32, String)> = ids
                .iter()
                .map(|id| (*id, idx.path_of(*id).to_lowercase()))
                .collect();
            keys.sort_unstable_by(|a, b| if asc { a.1.cmp(&b.1) } else { b.1.cmp(&a.1) });
            for (i, (id, _)) in keys.into_iter().enumerate() {
                ids[i] = id;
            }
        }
        _ => {
            ids.sort_unstable_by(|a, b| {
                let o = match f {
                    F::Name => cmp_name(idx, *a, *b),
                    F::Ext => cmp_bytes(ext_of_bytes(idx.name_bytes(*a)), ext_of_bytes(idx.name_bytes(*b))),
                    F::Drive => idx.vol[*a as usize].cmp(&idx.vol[*b as usize]),
                    F::Dir => idx.is_dir(*a).cmp(&idx.is_dir(*b)),
                    F::Type => cmp_bytes(ext_of_bytes(idx.name_bytes(*a)), ext_of_bytes(idx.name_bytes(*b))),
                    _ => O::Equal,
                };
                if asc {
                    o
                } else {
                    o.reverse()
                }
            });
        }
    }
    Ok(())
}

fn cmp_name(idx: &Index, a: u32, b: u32) -> std::cmp::Ordering {
    cmp_bytes(idx.name_bytes(a), idx.name_bytes(b))
}

fn cmp_bytes(a: &[u8], b: &[u8]) -> std::cmp::Ordering {
    let n = a.len().min(b.len());
    for i in 0..n {
        let x = a[i].to_ascii_lowercase();
        let y = b[i].to_ascii_lowercase();
        if x != y {
            return x.cmp(&y);
        }
    }
    a.len().cmp(&b.len())
}

fn run_aggregate(
    idx: &Index,
    stmt: &Stmt,
    ids: &[u32],
    total: usize,
    limit: usize,
) -> Result<Table, String> {
    let group_field = stmt.group;

    // 要累计哪些聚合：SELECT 里的，加上 ORDER BY 里点名但没出现在 SELECT 的
    let mut aggs: Vec<(Agg, F)> = Vec::new();
    for p in &stmt.proj {
        if let Proj::Agg(a, Some(f), _) = p {
            if a.needs_stat() && !aggs.iter().any(|(x, y)| x == a && y == f) {
                aggs.push((*a, *f));
            }
        }
    }
    if let Some(Ord { key: OrdKey::Agg(a), .. }) = stmt.order {
        if a.needs_stat() {
            let f = match stmt.proj.iter().find_map(|p| match p {
                Proj::Agg(x, Some(f), _) if *x == a => Some(*f),
                _ => None,
            }) {
                Some(f) => Some(f),
                // SELECT 里没有这个聚合，就按大小算
                None => Some(F::Size),
            };
            if let Some(f) = f {
                if !aggs.iter().any(|(x, y)| *x == a && *y == f) {
                    aggs.push((a, f));
                }
            }
        }
    }

    let mut order: Vec<String> = Vec::new();
    let mut map: std::collections::HashMap<String, Group> = std::collections::HashMap::new();
    let stats = AtomicUsize::new(0);
    let capped = AtomicBool::new(false);

    for &id in ids {
        let mut ctx = Ctx::new(idx, id, &stats, &capped);
        let key = match group_field {
            Some(f) => {
                if f.numeric() {
                    format!("{}", ctx.num(f) as i64)
                } else {
                    ctx.text(f)
                }
            }
            None => "全部".to_string(),
        };
        if !map.contains_key(&key) {
            order.push(key.clone());
            map.insert(
                key.clone(),
                Group {
                    key_text: key.clone(),
                    key_num: key.parse().unwrap_or(0),
                    min: f64::MAX,
                    max: f64::MIN,
                    ..Default::default()
                },
            );
        }
        let g = map.get_mut(&key).unwrap();
        g.count += 1;
        for (a, f) in &aggs {
            let v = ctx.num(*f);
            if v < 0.0 {
                continue;
            }
            match a {
                Agg::Count => {}
                Agg::Sum | Agg::Avg => g.sum += v,
                Agg::Min => g.min = g.min.min(v),
                Agg::Max => g.max = g.max.max(v),
            }
        }
    }

    let mut groups: Vec<Group> = order
        .into_iter()
        .filter_map(|k| map.remove(&k))
        .map(|mut g| {
            if g.min == f64::MAX {
                g.min = 0.0;
            }
            if g.max == f64::MIN {
                g.max = 0.0;
            }
            g
        })
        .collect();
    // 排序：默认「数量多的在前」，写了 ORDER BY 就按它排
    match stmt.order {
        None => groups.sort_by_key(|g| std::cmp::Reverse(g.count)),
        // 按分组键本身排：文本键（后缀、盘符）按字典序，数字键（Dir/Type）按数值
        Some(Ord { key: OrdKey::Field(_), asc }) => {
            if group_field.map(|f| !f.numeric()).unwrap_or(true) {
                groups.sort_by(|a, b| {
                    let o = a.key_text.cmp(&b.key_text);
                    if asc {
                        o
                    } else {
                        o.reverse()
                    }
                });
            } else {
                groups.sort_by(|a, b| {
                    let o = a.key_num.cmp(&b.key_num);
                    if asc {
                        o
                    } else {
                        o.reverse()
                    }
                });
            }
        }
        Some(o) => {
            let val = |g: &Group| -> f64 {
                match o.key {
                    OrdKey::Agg(Agg::Count) => g.count as f64,
                    OrdKey::Agg(Agg::Sum) => g.sum,
                    OrdKey::Agg(Agg::Avg) => {
                        if g.count > 0 {
                            g.sum / g.count as f64
                        } else {
                            0.0
                        }
                    }
                    OrdKey::Agg(Agg::Min) => g.min,
                    OrdKey::Agg(Agg::Max) => g.max,
                    OrdKey::Field(_) => g.key_num as f64,
                }
            };
            groups.sort_by(|a, b| {
                let ord = val(a).partial_cmp(&val(b)).unwrap_or(std::cmp::Ordering::Equal);
                if o.asc {
                    ord
                } else {
                    ord.reverse()
                }
            });
        }
    }
    // LIMIT 对分组同样有效：TOP 10 就该只有 10 组
    groups.truncate(stmt.limit.unwrap_or(limit).max(1));

    let mut fields: Vec<String> = Vec::new();
    let mut types: Vec<u8> = Vec::new();
    if let Some(f) = group_field {
        fields.push(f.label().to_string());
        types.push(if f.numeric() { 3 } else { 1 });
    }
    for p in &stmt.proj {
        if let Proj::Agg(_, _, alias) = p {
            fields.push(alias.clone());
            types.push(3);
        }
    }

    Ok(Table {
        fields,
        types,
        rows: Vec::new(),
        ids: Vec::new(),
        total,
        aggregate: true,
        group_label: group_field.map(|f| f.label().to_string()),
        groups,
    })
}
