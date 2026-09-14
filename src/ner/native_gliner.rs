//! Нативный GLiNER — span-классификация сущностей (Part E, Фаза 7.1).
//!
//! GLiNER = BERT-энкодер (нативный, из `pqc::encoder`) + span-голова:
//!
//! ```text
//! span(i..=j) → репрезентация [start_tok ‖ end_tok ‖ width_emb(width)]
//!            → линейный классификатор [num_labels, 3·hidden]
//!            → sigmoid → (label, score) для каждого span
//! ```
//!
//! Никакого `gline-rs`/ONNX — веса лежат в том же `.pqw` контейнере
//! (`model_type = SpanNer`), энкодерная спина общая с BGE-M3. Найденные
//! сущности готовы к экспорту в K-Hop граф знаний (AIDDE-якоря) —
//! голова отдаёт структуру (текст, метку, скор, границы), интерпретация
//! остаётся потребителю (принцип «инструмент, не ИИ»).
//!
//! Тензоры головы:
//!
//! ```text
//! span_width_emb  [max_width, hidden]   int8/int4
//! span_proj_w     [num_labels, 3·hidden] int8/int4
//! __labels__      raw (utf-8, '\n'-разделённые метки)
//! ```

use std::path::Path;

use crate::pqc::encoder::{synth_encoder, EncoderModel, EncoderOut, SynthRng};
use crate::pqc::pqw::{ModelType, PqwBuilder, Quant};
use crate::pqc::tensor;

// ---------------------------------------------------------------------------
// Сущность
// ---------------------------------------------------------------------------

/// Найденная сущность: текст, метка, уверенность, границы (в словах).
#[derive(Debug, Clone)]
pub struct Entity {
    /// Текст span (слова i..=j, склеенные пробелом).
    pub text: String,
    /// Метка классификатора (PERSON / LOCATION / OBJECT / …).
    pub label: String,
    /// Уверенность (sigmoid, 0..1).
    pub score: f32,
    /// Начало (индекс слова, включительно).
    pub start: usize,
    /// Конец (индекс слова, НЕ включительно).
    pub end: usize,
}

// ---------------------------------------------------------------------------
// Модель
// ---------------------------------------------------------------------------

/// GLiNER поверх mmap-весов `.pqw` (SpanNer-контейнер).
pub struct GlinerModel {
    encoder: EncoderModel,
    labels: Vec<String>,
    max_width: usize,
}

impl GlinerModel {
    /// Открывает модель (энкодер + span-голова, Sha256-верификация).
    pub fn open(path: &Path) -> Result<Self, String> {
        let encoder = EncoderModel::open(path)?;
        let view = encoder.view();
        if view.header().model_type != ModelType::SpanNer {
            return Err("модель не SpanNer-класса — --ner gliner требует .pqw с span-головой".into());
        }
        let labels_tensor = view.require("__labels__")?;
        let labels_raw = labels_tensor.raw_str()?;
        let labels: Vec<String> = labels_raw
            .split('\n')
            .filter(|s| !s.is_empty())
            .map(|s| s.to_string())
            .collect();
        if labels.is_empty() {
            return Err("список меток пуст".into());
        }
        let max_width = view.require("span_width_emb")?.rows();
        Ok(Self {
            encoder,
            labels,
            max_width,
        })
    }

    /// Метки классификатора.
    pub fn labels(&self) -> &[String] {
        &self.labels
    }

    /// Размер словаря энкодерной спины (для токенизации запроса).
    pub fn vocab(&self) -> usize {
        self.encoder.view().header().vocab
    }

    /// Скоры span (i..=j): sigmoid-вероятности по всем меткам.
    ///
    /// Репрезентация: `[h_i ‖ h_j ‖ width_emb[j−i]]` → proj-матвектор.
    pub fn span_scores(&self, out: &EncoderOut, i: usize, j: usize) -> Result<Vec<f32>, String> {
        let h = self.encoder.hidden();
        let seq = out.states.len() / h;
        if i >= seq || j >= seq || j < i {
            return Err(format!("span ({i}..={j}) вне последовательности {seq}"));
        }
        let width = j - i;
        if width >= self.max_width {
            return Err(format!("ширина span {width} ≥ max_width {}", self.max_width));
        }
        let view = self.encoder.view();
        let width_emb = view.require("span_width_emb")?;
        let proj = view.require("span_proj_w")?;
        let mut rep = vec![0f32; 3 * h];
        rep[..h].copy_from_slice(&out.states[i * h..(i + 1) * h]);
        rep[h..2 * h].copy_from_slice(&out.states[j * h..(j + 1) * h]);
        width_emb.gather_row(width, &mut rep[2 * h..])?;
        let mut logits = vec![0f32; proj.rows()];
        proj.matvec(&rep, &mut logits)?;
        for v in logits.iter_mut() {
            *v = 1.0 / (1.0 + (-*v).exp());
        }
        Ok(logits)
    }

    /// Извлекает сущности: слова + их токен-идентификаторы (порядок 1:1).
    ///
    /// Порог `threshold` на sigmoid-скор; span шире `max_width`
    /// пропускаются. Слова нужны для текста span (токены — модели).
    pub fn extract(
        &self,
        words: &[&str],
        ids: &[u32],
        threshold: f32,
    ) -> Result<Vec<Entity>, String> {
        if words.len() != ids.len() {
            return Err(format!(
                "слов {} != токенов {} — склейте UAX#29-токенизацию",
                words.len(),
                ids.len()
            ));
        }
        let out = self.encoder.forward(ids)?;
        let h = self.encoder.hidden();
        let seq = out.states.len() / h;
        let mut entities = Vec::new();
        for i in 0..seq {
            let j_max = seq.min(i + self.max_width);
            for j in i..j_max {
                let scores = self.span_scores(&out, i, j)?;
                for (l, &p) in scores.iter().enumerate() {
                    if p >= threshold {
                        entities.push(Entity {
                            text: words[i..=j].join(" "),
                            label: self.labels[l].clone(),
                            score: p,
                            start: i,
                            end: j + 1,
                        });
                    }
                }
            }
        }
        Ok(entities)
    }
}

// ---------------------------------------------------------------------------
// Синтетическая модель (тесты + --pqw-selftest)
// ---------------------------------------------------------------------------

/// fp32-эталон span-головы (дифференциальные тесты).
pub struct Fp32Gliner {
    pub encoder: crate::pqc::encoder::Fp32Encoder,
    pub width: Vec<f32>,
    pub proj: Vec<f32>,
    pub labels: Vec<String>,
    pub hidden: usize,
}

/// Генерирует синтетический GLiNER: энкодер + span-голова.
#[allow(clippy::too_many_arguments)]
pub fn synth_gliner(
    seed: u64,
    layers: usize,
    hidden: usize,
    heads: usize,
    intermediate: usize,
    vocab: usize,
    max_pos: usize,
    labels: &[&str],
    quant: Quant,
) -> (PqwBuilder, Fp32Gliner) {
    // Энкодерная спина (ModelType меняется на SpanNer после сборки).
    let (mut b, encoder) = synth_encoder(seed, layers, hidden, heads, intermediate, vocab, max_pos, quant);
    b.model_type = ModelType::SpanNer;
    // Свежий ГПСЧ для головы (детерминизм: сид с солью).
    let mut rng = SynthRng(seed ^ 0xC0FF_EE00_1234_5678);
    let max_width = 16usize;
    let n_labels = labels.len();

    let mut w = vec![0f32; max_width * hidden];
    rng.fill(&mut w, -0.2, 0.2);
    match quant {
        Quant::Int8 => {
            let (q, s) = tensor::quant_i8_per_row(&w, max_width, hidden);
            b.add_i8("span_width_emb", vec![max_width, hidden], &q, &s);
        }
        Quant::Int4 => {
            let (q, s) = tensor::quant_i4_per_row(&w, max_width, hidden);
            b.add_i4("span_width_emb", vec![max_width, hidden], &q, &s);
        }
        Quant::F32 => {
            b.add_f32("span_width_emb", vec![max_width, hidden], &w);
        }
    }

    let mut proj = vec![0f32; n_labels * 3 * hidden];
    rng.fill(&mut proj, -0.15, 0.15);
    match quant {
        Quant::Int8 => {
            let (q, s) = tensor::quant_i8_per_row(&proj, n_labels, 3 * hidden);
            b.add_i8("span_proj_w", vec![n_labels, 3 * hidden], &q, &s);
        }
        Quant::Int4 => {
            let (q, s) = tensor::quant_i4_per_row(&proj, n_labels, 3 * hidden);
            b.add_i4("span_proj_w", vec![n_labels, 3 * hidden], &q, &s);
        }
        Quant::F32 => {
            b.add_f32("span_proj_w", vec![n_labels, 3 * hidden], &proj);
        }
    }
    b.add_raw("__labels__", labels.join("\n").as_bytes());

    (
        b,
        Fp32Gliner {
            encoder,
            width: w,
            proj,
            labels: labels.iter().map(|s| s.to_string()).collect(),
            hidden,
        },
    )
}

// ---------------------------------------------------------------------------
// Тесты
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pqc::encoder::reference_forward;

    fn cosine(a: &[f32], b: &[f32]) -> f32 {
        let ip = a.iter().zip(b).map(|(x, y)| x * y).sum::<f32>();
        let na = a.iter().map(|v| v * v).sum::<f32>().sqrt();
        let nb = b.iter().map(|v| v * v).sum::<f32>().sqrt();
        ip / (na * nb)
    }

    #[test]
    fn gliner_extracts_wellformed_entities() {
        let path = std::env::temp_dir().join(format!(
            "poler-gliner-struct-{}.pqw",
            std::process::id()
        ));
        let (builder, _) = synth_gliner(
            3,
            1,
            32,
            4,
            64,
            64,
            48,
            &["PERSON", "LOCATION", "OBJECT"],
            Quant::Int8,
        );
        builder.write_to(&path).unwrap();
        let model = GlinerModel::open(&path).unwrap();
        let words = ["Вэнс", "прибыл", "на", "Архисферу", "вечером"];
        let ids: Vec<u32> = words.iter().map(|w| w.len() as u32 % 64).collect();
        let ents = model.extract(&words, &ids, 0.5).unwrap();
        for e in &ents {
            assert!(!e.text.is_empty());
            assert!(model.labels().contains(&e.label), "метка {}", e.label);
            assert!(e.start < 5 && e.end <= 5 && e.start < e.end);
            assert!((0.5..=1.0).contains(&e.score));
            // Текст span == склейка слов.
            assert_eq!(e.text, words[e.start..e.end].join(" "));
        }
        // Синтетические веса дают случайные скоры — но структура обязана
        // быть согласованной; сущности хотя бы иногда появляются.
        assert!(!ents.is_empty(), "случайные веса всё же должны что-то дать");
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn gliner_span_scores_match_fp32_reference() {
        let path = std::env::temp_dir().join(format!(
            "poler-gliner-diff-{}.pqw",
            std::process::id()
        ));
        let (builder, fp32) = synth_gliner(
            5,
            1,
            32,
            4,
            64,
            64,
            48,
            &["A", "B"],
            Quant::Int8,
        );
        builder.write_to(&path).unwrap();
        let model = GlinerModel::open(&path).unwrap();
        let words = ["альфа", "бета", "гамма"];
        let ids: Vec<u32> = vec![10, 20, 30];
        let out = model.encoder.forward(&ids).unwrap();
        let native = model.span_scores(&out, 0, 2).unwrap();

        // Эталон: fp32-энкодер + наивная голова.
        let states = reference_forward(&fp32.encoder, &ids);
        let h = fp32.hidden;
        let mut rep = vec![0f32; 3 * h];
        rep[..h].copy_from_slice(&states[..h]);
        rep[h..2 * h].copy_from_slice(&states[2 * h..3 * h]);
        let width = 2;
        rep[2 * h..].copy_from_slice(&fp32.width[width * h..(width + 1) * h]);
        let reference: Vec<f32> = (0..2)
            .map(|l| {
                let mut lg = 0f32;
                for (a, b) in fp32.proj[l * 3 * h..(l + 1) * 3 * h].iter().zip(&rep) {
                    lg += a * b;
                }
                1.0 / (1.0 + (-lg).exp())
            })
            .collect();
        let c = cosine(&native, &reference);
        assert!(c > 0.999, "косинус span-скоров = {c}");
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn gliner_rejects_wrong_model_type() {
        // Чистый энкодер не должен открываться как GLiNER.
        let path = std::env::temp_dir().join(format!(
            "poler-gliner-wrong-{}.pqw",
            std::process::id()
        ));
        let (builder, _) = crate::pqc::encoder::synth_encoder(
            1,
            1,
            16,
            2,
            32,
            32,
            32,
            Quant::Int8,
        );
        builder.write_to(&path).unwrap();
        assert!(GlinerModel::open(&path).is_err());
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn gliner_word_token_count_mismatch_rejected() {
        let path = std::env::temp_dir().join(format!(
            "poler-gliner-mm-{}.pqw",
            std::process::id()
        ));
        let (builder, _) = synth_gliner(2, 1, 16, 2, 32, 32, 32, &["X"], Quant::Int8);
        builder.write_to(&path).unwrap();
        let model = GlinerModel::open(&path).unwrap();
        assert!(model.extract(&["a", "b"], &[1], 0.5).is_err());
        let _ = std::fs::remove_file(&path);
    }
}
