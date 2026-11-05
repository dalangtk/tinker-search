//! 内置语义模型：bge-small-zh-v1.5（ONNX Runtime，int8 量化）。
//!
//! 这里要先把能力边界说清楚，否则很容易用错：
//!
//! **它不能解析「今天下载的 pdf」这类结构化条件。**
//! bge 是 embedding 模型，不是生成模型——它只会把一句话压成一个 512 维向量，
//! 拿不出「时间=今天、后缀=pdf」这样的字段。那部分仍然由 `nlu.rs` 的规则引擎做。
//!
//! **它能做的是语义相似度**：
//! - 语义重排：把「预算」和「2026年度预算表.xlsx」这类字面不重合但意思相关的结果提上来；
//! - 意图判断：判断一句话更像结构化查询还是口语描述，决定走哪条解析路径。
//!
//! 工程上的取舍：
//! - 模型随安装包分发（23 MB），ORT 静态链接进 exe，不依赖外部 DLL；
//! - 懒加载：没人用语义功能就不会占内存、不会拖慢启动；
//! - 批量推理：短文本按 32 条一批，单条成本从 1.16 ms 降到 0.60 ms；
//! - 任何环节失败都静默降级，绝不阻塞搜索。

use once_cell::sync::OnceCell;
use ort::session::Session;
use ort::value::Tensor;
use std::path::{Path, PathBuf};
use tokenizers::Tokenizer;

/// 模型目录名
const MODEL_DIR: &str = "bge-small-zh-v1.5";
/// 一次前向的条数。32 条在速度与激活内存之间比较平衡。
const BATCH: usize = 32;
/// 截断长度。文件名很少超过 64 字，128 已足够。
const MAX_TOK: usize = 128;

pub struct Engine {
    sess: Session,
    tk: Tokenizer,
    dim: usize,
}

/// 进程内唯一实例。`None` 表示还没加载或加载失败。
static ENGINE: OnceCell<parking_lot::Mutex<Slot>> = OnceCell::new();

#[derive(Default)]
struct Slot {
    engine: Option<Engine>,
    err: Option<String>,
    load_ms: u128,
}

/// 对外暴露的状态，给设置页显示。
#[derive(serde::Serialize, Clone, Default)]
pub struct Status {
    /// 模型文件是否随包分发到位
    pub present: bool,
    /// 是否已经加载进内存
    pub loaded: bool,
    /// 向量维度
    pub dim: usize,
    /// 模型文件大小（MB）
    pub size_mb: f64,
    /// 加载耗时（毫秒）
    pub load_ms: u128,
    /// 模型目录
    pub dir: String,
    /// 是否用的用户自备模型（覆盖内置）
    pub custom: bool,
    /// 加载失败时的原因
    pub error: String,
}

fn slot() -> &'static parking_lot::Mutex<Slot> {
    ENGINE.get_or_init(|| parking_lot::Mutex::new(Slot::default()))
}

/// 定位模型目录。查找顺序：
///
/// 1. `{数据目录}/models/bge-small-zh-v1.5/` —— 用户自备模型，优先级最高
///    （换模型只要把 `model.onnx` + `tokenizer.json` 丢进去，重启即生效）
/// 2. `{资源目录}/assets/models/bge-small-zh-v1.5/` —— 安装包自带
/// 3. 源码树 —— 开发模式
fn model_dir(app_dir: Option<&Path>, user_dir: Option<&Path>) -> Option<PathBuf> {
    if let Some(u) = user_dir {
        let p = u.join(MODEL_DIR);
        if p.join("model.onnx").exists() {
            return Some(p);
        }
    }
    if let Some(d) = app_dir {
        let p = d.join("assets").join("models").join(MODEL_DIR);
        if p.join("model.onnx").exists() {
            return Some(p);
        }
    }
    // 开发模式：资源还没被复制过去时直接读源码树
    let dev = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("assets")
        .join("models")
        .join(MODEL_DIR);
    if dev.join("model.onnx").exists() {
        return Some(dev);
    }
    None
}

impl Engine {
    fn load(dir: &Path) -> Result<Engine, String> {
        let env = ort::init()
            .with_name("tinker-semantic")
            .build()
            .map_err(|e| format!("ORT 初始化失败：{e}"))?;
        let sess = Session::builder(&env)
            .map_err(|e| format!("会话创建失败：{e}"))?
            .with_intra_threads(4)
            .map_err(|e| format!("线程配置失败：{e}"))?
            .commit_from_file(dir.join("model.onnx"))
            .map_err(|e| format!("模型加载失败：{e}"))?;
        let tk = Tokenizer::from_file(dir.join("tokenizer.json"))
            .map_err(|e| format!("分词器加载失败：{e}"))?;
        Ok(Engine { sess, tk, dim: 512 })
    }

    /// 批量编码，返回 L2 归一化后的向量（点积即余弦相似度）。
    pub fn encode(&mut self, texts: &[String]) -> Result<Vec<Vec<f32>>, String> {
        let mut out = Vec::with_capacity(texts.len());
        for chunk in texts.chunks(BATCH) {
            let encs = self
                .tk
                .encode_batch(chunk.to_vec(), true)
                .map_err(|e| format!("分词失败：{e}"))?;
            let maxlen = encs
                .iter()
                .map(|e| e.get_ids().len().min(MAX_TOK))
                .max()
                .unwrap_or(0)
                .max(1);
            let bs = encs.len();

            let mut ids = vec![0i64; bs * maxlen];
            let mut mask = vec![0i64; bs * maxlen];
            for (i, e) in encs.iter().enumerate() {
                let n = e.get_ids().len().min(maxlen);
                for j in 0..n {
                    ids[i * maxlen + j] = e.get_ids()[j] as i64;
                    mask[i * maxlen + j] = e.get_attention_mask()[j] as i64;
                }
            }
            // token_type_ids 必须全 0：BERT 只有句对任务才用 1
            let types = vec![0i64; bs * maxlen];

            let res = self
                .sess
                .run(ort::inputs![
                    "input_ids" => Tensor::from_array((vec![bs, maxlen], ids)).map_err(|e| e.to_string())?,
                    "attention_mask" => Tensor::from_array((vec![bs, maxlen], mask)).map_err(|e| e.to_string())?,
                    "token_type_ids" => Tensor::from_array((vec![bs, maxlen], types)).map_err(|e| e.to_string())?,
                ])
                .map_err(|e| format!("推理失败：{e}"))?;
            let (shape, data) = res[0]
                .try_extract_tensor::<f32>()
                .map_err(|e| format!("输出解析失败：{e}"))?;
            let hidden = shape[2] as usize;
            // 换成别的 embedding 模型时维度可能不是 512，这里跟着实际输出走
            if hidden != self.dim {
                self.dim = hidden;
            }

            for i in 0..bs {
                // 取 [CLS] 作句向量，bge 系列即如此训练
                let base = i * maxlen * hidden;
                let mut v = data[base..base + hidden].to_vec();
                let norm = v.iter().map(|x| x * x).sum::<f32>().sqrt();
                if norm > 1e-6 {
                    for x in v.iter_mut() {
                        *x /= norm;
                    }
                }
                out.push(v);
            }
        }
        Ok(out)
    }
}

/// 确保模型已加载；失败会把原因记进 slot，后续调用直接返回错误，不反复重试。
fn ensure(app_dir: Option<&Path>, user_dir: Option<&Path>) -> Result<(), String> {
    let mut s = slot().lock();
    if s.engine.is_some() {
        return Ok(());
    }
    if let Some(e) = &s.err {
        return Err(e.clone());
    }
    let Some(dir) = model_dir(app_dir, user_dir) else {
        let e = "未找到语义模型文件".to_string();
        s.err = Some(e.clone());
        return Err(e);
    };
    let t = std::time::Instant::now();
    match Engine::load(&dir) {
        Ok(eng) => {
            s.load_ms = t.elapsed().as_millis();
            s.engine = Some(eng);
            Ok(())
        }
        Err(e) => {
            s.err = Some(e.clone());
            Err(e)
        }
    }
}

pub fn status(app_dir: Option<&Path>, user_dir: Option<&Path>) -> Status {
    let dir = model_dir(app_dir, user_dir);
    let size = dir
        .as_ref()
        .and_then(|d| std::fs::metadata(d.join("model.onnx")).ok())
        .map(|m| m.len() as f64 / 1048576.0)
        .unwrap_or(0.0);
    let custom = dir
        .as_ref()
        .zip(user_dir)
        .map(|(d, u)| d.starts_with(u))
        .unwrap_or(false);
    let s = slot().lock();
    Status {
        present: dir.is_some(),
        loaded: s.engine.is_some(),
        dim: s.engine.as_ref().map(|e| e.dim).unwrap_or(0),
        size_mb: (size * 10.0).round() / 10.0,
        load_ms: s.load_ms,
        dir: dir
            .map(|d| d.to_string_lossy().to_string())
            .unwrap_or_default(),
        custom,
        error: s.err.clone().unwrap_or_default(),
    }
}

/// 主动加载（设置页「预加载」按钮用）。
pub fn warm(app_dir: Option<&Path>, user_dir: Option<&Path>) -> Result<Status, String> {
    ensure(app_dir, user_dir)?;
    Ok(status(app_dir, user_dir))
}

/// 单条编码，失败返回 None（调用方静默降级）。
#[allow(dead_code)]
pub fn embed(app_dir: Option<&Path>, user_dir: Option<&Path>, text: &str) -> Option<Vec<f32>> {
    if ensure(app_dir, user_dir).is_err() {
        return None;
    }
    let mut s = slot().lock();
    let eng = s.engine.as_mut()?;
    eng.encode(&[text.to_string()]).ok()?.pop()
}

/// 语义相关度打分：返回与 `docs` 等长的余弦相似度。
///
/// `query` 为空、模型不可用、或 `docs` 过短时返回 `None`，调用方保持原排序。
pub fn scores(
    app_dir: Option<&Path>,
    user_dir: Option<&Path>,
    query: &str,
    docs: &[String],
) -> Option<Vec<f32>> {
    if docs.is_empty() || query.trim().is_empty() {
        return None;
    }
    if ensure(app_dir, user_dir).is_err() {
        return None;
    }
    let mut s = slot().lock();
    let eng = s.engine.as_mut()?;

    let mut all = Vec::with_capacity(docs.len() + 1);
    all.push(query.to_string());
    all.extend(docs.iter().cloned());
    let mut vs = eng.encode(&all).ok()?;
    if vs.len() != all.len() {
        return None;
    }
    let q = vs.remove(0);
    Some(vs.iter().map(|v| dot(&q, v)).collect())
}

/// 把语义分与原排序融合后给出新的顺序。
///
/// `base` 是原有顺序（索引下表，0 最相关），返回按融合分降序的索引。
pub fn rerank(base: &[usize], sem: &[f32], weight: f32) -> Vec<usize> {
    let n = base.len();
    if n == 0 || sem.len() != n {
        return base.to_vec();
    }
    // 语义分做 min-max 归一化，避免整体偏高/偏低影响权重
    let (mut lo, mut hi) = (f32::MAX, f32::MIN);
    for &s in sem {
        lo = lo.min(s);
        hi = hi.max(s);
    }
    let span = (hi - lo).max(1e-6);

    let mut scored: Vec<(f32, usize)> = base
        .iter()
        .enumerate()
        .map(|(rank, &idx)| {
            let lex = 1.0 - (rank as f32 / n as f32);
            let sn = (sem[rank] - lo) / span;
            (weight * sn + (1.0 - weight) * lex, idx)
        })
        .collect();
    scored.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
    scored.into_iter().map(|(_, i)| i).collect()
}

fn dot(a: &[f32], b: &[f32]) -> f32 {
    a.iter().zip(b).map(|(x, y)| x * y).sum()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 融合函数只依赖纯数据，单独测它，不依赖模型文件
    #[test]
    fn rerank_keeps_order_when_weight_zero() {
        let base = vec![0, 1, 2, 3];
        let sem = vec![0.1, 0.9, 0.2, 0.3];
        let out = rerank(&base, &sem, 0.0);
        assert_eq!(out, base, "权重为 0 时应完全保留原顺序");
    }

    #[test]
    fn rerank_promotes_semantic_hit() {
        let base = vec![0, 1, 2];
        let sem = vec![0.2, 0.25, 0.95];
        let out = rerank(&base, &sem, 0.8);
        assert_eq!(out[0], 2, "语义分最高的应当被提到第一位");
    }

    #[test]
    fn rerank_handles_empty_and_mismatch() {
        assert!(rerank(&[], &[], 0.5).is_empty());
        assert_eq!(rerank(&[1, 2], &[0.1], 0.5), vec![1, 2], "长度不匹配时原样返回");
    }

    #[test]
    fn dot_of_normalized_is_cosine() {
        let a = [0.6f32, 0.8];
        let b = [1.0f32, 0.0];
        assert!((dot(&a, &b) - 0.6).abs() < 1e-6);
    }

    /// 安装后的目录布局：`{安装目录}/assets/models/bge-small-zh-v1.5/`
    #[test]
    fn resolves_resource_layout() {
        let root = std::env::temp_dir().join(format!("tinker-sem-res-{}", std::process::id()));
        let dir = root.join("assets").join("models").join(MODEL_DIR);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("model.onnx"), b"stub").unwrap();
        let got = model_dir(Some(&root), None).expect("应当能定位到资源目录里的模型");
        assert_eq!(got, dir);
        let _ = std::fs::remove_dir_all(&root);
    }

    /// 用户自备模型优先于内置模型
    #[test]
    fn user_model_overrides_bundled() {
        let root = std::env::temp_dir().join(format!("tinker-sem-user-{}", std::process::id()));
        let app = root.join("app");
        let user = root.join("data").join("models");
        for d in [
            app.join("assets").join("models").join(MODEL_DIR),
            user.join(MODEL_DIR),
        ] {
            std::fs::create_dir_all(&d).unwrap();
            std::fs::write(d.join("model.onnx"), b"stub").unwrap();
        }
        let got = model_dir(Some(&app), Some(&user)).expect("应当能找到模型");
        assert!(
            got.starts_with(user.join(MODEL_DIR)),
            "用户目录应当优先，实际 {got:?}"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    /// 真模型可用时，验证「预算」确实把预算表排在前面。
    /// 模型文件缺失时直接跳过，避免在没有资源的机器上红。
    #[test]
    fn model_ranks_semantically() {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("assets")
            .join("models")
            .join(MODEL_DIR);
        if !dir.join("model.onnx").exists() {
            return;
        }
        let docs = vec![
            "2026年度预算表.xlsx".to_string(),
            "周会纪要_0901.md".to_string(),
            "猫咪照片.jpg".to_string(),
        ];
        let s = scores(None, None, "预算", &docs).expect("模型应当可用");
        assert_eq!(s.len(), 3);
        assert!(
            s[0] > s[1] && s[0] > s[2],
            "预算表应当最相关，实际 {s:?}"
        );
        // 语义分做重排后，第一位不应改变
        let order = rerank(&[0, 1, 2], &s, 0.45);
        assert_eq!(order[0], 0);
    }
}
