//! Контракт вывода для AI-агента: самодостаточный Context Anchor.
//!
//! JSON-схема (спецификация §5):
//!
//! ```json
//! {
//!   "query": "нокс",
//!   "total_hits": 1,
//!   "anchors": [
//!     {
//!       "file": "/path/to/chapter_36.md",
//!       "token": "нокс",
//!       "epsilon": 7983.94,
//!       "resonance": 28714.73,
//!       "scene": {
//!         "chapter": "Глава 36. Инертный",
//!         "temporal_metric": "Метрика: Т-23",
//!         "location": "Локация: Разлом Каньона",
//!         "subjects": ["Субъекты: Мальчик (гибрид), Соболь (Нокс)"],
//!         "enclosing_scope": "Полный текст законченной сцены..."
//!       },
//!       "k_hop_relations": [["Нокс", "вонзила_когти", "Солнечное сплетение"]]
//!     }
//!   ]
//! }
//! ```

use serde::{Deserialize, Serialize};

use crate::parser::markdown_scenes::SceneContext;

/// Один Context Anchor: самодостаточный контекст совпадения.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContextAnchor {
    pub file: String,
    pub token: String,
    pub epsilon: f64,
    pub resonance: f64,
    pub scene: SceneContext,
    pub k_hop_relations: Vec<(String, String, String)>,
}

/// Итог поиска.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchResult {
    pub query: String,
    /// Общее число совпадений (до усечения по top_n).
    pub total_hits: usize,
    pub anchors: Vec<ContextAnchor>,
}

/// Markdown-рендер результата (формат `--format md`).
pub fn render_markdown(res: &SearchResult) -> String {
    let mut out = String::new();
    out.push_str(&format!(
        "# POLER-Engine: «{}» (Найдено: {})\n\n",
        res.query, res.total_hits
    ));
    for (i, a) in res.anchors.iter().enumerate() {
        out.push_str(&format!(
            "## {}. Файл: `{}` (R={:.2}, ε={:.2})\n\n",
            i + 1,
            a.file,
            a.resonance,
            a.epsilon
        ));
        if let Some(m) = &a.scene.temporal_metric {
            let val = m.strip_prefix("Метрика: ").unwrap_or(m);
            out.push_str(&format!("- **Метрика:** {val}\n"));
        }
        if let Some(l) = &a.scene.location {
            let val = l.strip_prefix("Локация: ").unwrap_or(l);
            out.push_str(&format!("- **Локация:** {val}\n"));
        }
        for s in &a.scene.subjects {
            let val = s.strip_prefix("Субъекты: ").unwrap_or(s);
            out.push_str(&format!("- **Субъекты:** {val}\n"));
        }
        if !a.k_hop_relations.is_empty() {
            out.push_str("- **K-Hop связи:**\n");
            for (s, p, o) in &a.k_hop_relations {
                out.push_str(&format!("  - `{s}` —{p}→ `{o}`\n"));
            }
        }
        out.push_str(&format!("\n```text\n{}\n```\n---\n\n", a.scene.enclosing_scope));
    }
    out
}

/// Компактный рендер (формат `--format simple`).
pub fn render_simple(res: &SearchResult) -> String {
    let mut out = String::new();
    let total = res.anchors.len();
    for (i, a) in res.anchors.iter().enumerate() {
        let head: String = a.scene.enclosing_scope.chars().take(120).collect();
        out.push_str(&format!(
            "[{}/{}] R={:.1} ε={:.1} | {} | {}\n",
            i + 1,
            total,
            a.resonance,
            a.epsilon,
            a.file,
            head
        ));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parser::markdown_scenes::SceneContext;

    fn sample() -> SearchResult {
        let scene = SceneContext {
            chapter: "Глава 36. Инертный".into(),
            temporal_metric: Some("Метрика: Т-23".into()),
            location: Some("Локация: Разлом Каньона".into()),
            subjects: vec!["Субъекты: Мальчик (гибрид), Соболь (Нокс)".into()],
            enclosing_scope: "Нокс вонзила когти...".into(),
            metric_tag: Some("Т-23".into()),
            subject_names: vec![],
            subject_pairs: vec![],
        };
        SearchResult {
            query: "нокс".into(),
            total_hits: 3,
            anchors: vec![ContextAnchor {
                file: "/book/chapter_36.md".into(),
                token: "нокс".into(),
                epsilon: 7983.94,
                resonance: 28714.73,
                scene,
                k_hop_relations: vec![(
                    "Нокс".into(),
                    "вонзила_когти".into(),
                    "Солнечное сплетение".into(),
                )],
            }],
        }
    }

    #[test]
    fn json_contract_shape() {
        let v = serde_json::to_value(sample()).unwrap();
        assert_eq!(v["query"], "нокс");
        assert_eq!(v["total_hits"], 3);
        let a = &v["anchors"][0];
        assert_eq!(a["file"], "/book/chapter_36.md");
        assert_eq!(a["token"], "нокс");
        assert!(a["epsilon"].is_f64());
        assert!(a["resonance"].is_f64());
        assert_eq!(a["scene"]["chapter"], "Глава 36. Инертный");
        assert_eq!(a["scene"]["temporal_metric"], "Метрика: Т-23");
        assert_eq!(a["scene"]["location"], "Локация: Разлом Каньона");
        assert!(a["scene"]["subjects"].is_array());
        assert!(a["scene"]["enclosing_scope"].is_string());
        // скрытые поля не сериализуются
        assert!(a["scene"].get("metric_tag").is_none());
        assert!(a["scene"].get("subject_names").is_none());
        // k_hop_relations — массивы из трёх строк
        let rel = &a["k_hop_relations"][0];
        assert_eq!(rel.as_array().unwrap().len(), 3);
        assert_eq!(rel[0], "Нокс");
        assert_eq!(rel[1], "вонзила_когти");
        assert_eq!(rel[2], "Солнечное сплетение");
    }

    #[test]
    fn roundtrip_deserialize() {
        let json = serde_json::to_string(&sample()).unwrap();
        let back: SearchResult = serde_json::from_str(&json).unwrap();
        assert_eq!(back.query, "нокс");
        assert_eq!(back.anchors[0].k_hop_relations.len(), 1);
    }

    #[test]
    fn markdown_render_contains_sections() {
        let md = render_markdown(&sample());
        assert!(md.contains("POLER-Engine"));
        assert!(md.contains("**Метрика:** Т-23"));
        assert!(md.contains("**Локация:** Разлом Каньона"));
        assert!(md.contains("вонзила_когти"));
        assert!(md.contains("```text"));
    }

    #[test]
    fn simple_render_one_line_per_anchor() {
        let s = render_simple(&sample());
        assert_eq!(s.lines().count(), 1);
        assert!(s.contains("R="));
    }
}
