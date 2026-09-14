//! Мост `.pqw` → векторный слой (Part E, задачи 4.5.1 + 3.1).
//!
//! `PqwEmbedder` открывает нативный энкодер из контейнера `.pqw`
//! (mmap zero-copy + Sha256) и реализует трейт [`Embedder`] — точку
//! подключения, подготовленную ещё в кирпиче 1 (там был HashEmbedder,
//! здесь — настоящий нейронный forward на кернелах pqc). Дальше векторы
//! уходят в RaBitQ-субстрат/HNSW как у любого другого эмбеддера:
//! модель сменная, субстрат масштаба — неизменен.
//!
//! CLI: `poler-engine --semantic dense --model bge-m3.pqw -q "…"`.

use std::path::Path;

use super::Embedder;
use crate::pqc::encoder::EncoderModel;
use crate::pqc::hash_token_ids;

/// Эмбеддер поверх `.pqw`-энкодера (BGE-M3-класс).
pub struct PqwEmbedder {
    model: EncoderModel,
    vocab: u32,
}

impl PqwEmbedder {
    /// Открывает модель (с Sha256-верификацией весов).
    pub fn open(path: &Path) -> Result<Self, String> {
        let model = EncoderModel::open(path)?;
        let vocab = model.view().header().vocab as u32;
        Ok(Self { model, vocab })
    }
}

impl Embedder for PqwEmbedder {
    fn name(&self) -> &str {
        "pqw-native-encoder (BGE-M3-класс, int8/int4 через pqc)"
    }

    fn dim(&self) -> usize {
        self.model.hidden()
    }

    fn normalized(&self) -> bool {
        // CLS-пулинг с L2-нормализацией — косинус == IP.
        true
    }

    fn embed_batch(&mut self, texts: &[&str]) -> Result<Vec<Vec<f32>>, String> {
        texts
            .iter()
            .map(|t| {
                let ids = hash_token_ids(t, self.vocab);
                if ids.is_empty() {
                    return Err(format!("текст без UAX#29-слов: {t:?}"));
                }
                self.model.embed(&ids)
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pqc::pqw::Quant;

    fn tmp_model() -> std::path::PathBuf {
        // Небольшая синтетическая модель для контрактов моста.
        let path = std::env::temp_dir().join(format!(
            "poler-bridge-{}.pqw",
            std::process::id()
        ));
        let (builder, _) =
            crate::pqc::encoder::synth_encoder(33, 1, 32, 4, 64, 64, 48, Quant::Int8);
        builder.write_to(&path).unwrap();
        path
    }

    #[test]
    fn pqw_embedder_satisfies_contract() {
        fn dim_of(e: &mut dyn Embedder) -> usize {
            e.dim()
        }
        let path = tmp_model();
        let mut e = PqwEmbedder::open(&path).unwrap();
        assert_eq!(dim_of(&mut e), 32);
        assert!(e.normalized());
        assert!(e.name().contains("pqw"));
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn pqw_embedder_batch_matches_direct_encoder() {
        let path = tmp_model();
        let mut e = PqwEmbedder::open(&path).unwrap();
        let texts = ["нокс вонзил когти", "the vector layer"];
        let vs = e.embed_batch(&texts).unwrap();
        assert_eq!(vs.len(), 2);
        for v in &vs {
            let n = v.iter().map(|x| x * x).sum::<f32>().sqrt();
            assert!((n - 1.0).abs() < 1e-5, "норма {n}");
        }
        // Прямой вызов энкодера теми же токенами — тот же вектор.
        let ids = hash_token_ids(texts[0], 64);
        let direct = EncoderModel::open(&path).unwrap().embed(&ids).unwrap();
        assert_eq!(vs[0], direct);
        // Пустой текст — честная ошибка, не мусорный вектор.
        assert!(e.embed_batch(&["   "]).is_err());
        let _ = std::fs::remove_file(&path);
    }
}
