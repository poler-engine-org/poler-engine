//! DiGraph на базе petgraph: сущности, предикатные рёбра, K-hop поиск.
//!
//! Формула обхода (спецификация §3.В):
//!
//! ```text
//! SubGraph(E0, k) = { (u, predicate, v) | dist(E0, u) < k ∧ (u →predicate v) ∈ G }
//! ```
//!
//! Особенности реализации:
//!
//! * обход BFS **в обе стороны** (outgoing + incoming) — связи «кто ссылается
//!   на сущность» не менее важны, чем «на кого ссылается сущность»;
//! * узлы сливаются case-insensitive, отображаемое имя повышается до
//!   варианта с заглавной буквы;
//! * каждый узел несёт temporal-слой (`Т-23`, …); при заданном фильтре
//!   рёбра между узлами чужих слоёв отбрасываются (Temporal Metric Tagging);
//! * тройки дедуплицируются, вывод ограничен `max_relations`.

use petgraph::graph::{DiGraph, NodeIndex};
use petgraph::visit::EdgeRef;
use petgraph::Direction;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet, VecDeque};

/// Узел знаний: сущность с temporal-слоем.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KnowledgeNode {
    pub name: String,
    pub node_type: String,
    pub temporal_layer: Option<String>,
}

/// Ребро знаний: предикат + вес.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RelationEdge {
    pub predicate: String,
    pub weight: f64,
}

/// Граф сущностей со словарём узлов (lowercase key).
pub struct EntityGraph {
    graph: DiGraph<KnowledgeNode, RelationEdge>,
    lookup: HashMap<String, NodeIndex>,
}

impl EntityGraph {
    pub fn new() -> Self {
        Self {
            graph: DiGraph::new(),
            lookup: HashMap::new(),
        }
    }

    pub fn node_count(&self) -> usize {
        self.graph.node_count()
    }

    pub fn edge_count(&self) -> usize {
        self.graph.edge_count()
    }

    fn get_or_create(&mut self, name: &str, temporal: Option<&str>) -> NodeIndex {
        let key = name.to_lowercase();
        if let Some(&idx) = self.lookup.get(&key) {
            // повышаем отображаемое имя до варианта с заглавной
            let cur = self.graph[idx].name.clone();
            let new_better = name.chars().next().map(|c| c.is_uppercase()).unwrap_or(false)
                && cur.chars().next().map(|c| c.is_lowercase()).unwrap_or(false);
            if new_better {
                self.graph[idx].name = name.to_string();
            }
            if self.graph[idx].temporal_layer.is_none() {
                self.graph[idx].temporal_layer = temporal.map(String::from);
            }
            return idx;
        }
        let idx = self.graph.add_node(KnowledgeNode {
            name: name.to_string(),
            node_type: "Entity".to_string(),
            temporal_layer: temporal.map(String::from),
        });
        self.lookup.insert(key, idx);
        idx
    }

    /// Добавляет тройку `(subject, predicate, object)` с temporal-слоем.
    pub fn add_triple(&mut self, subject: &str, predicate: &str, object: &str, temporal: Option<&str>, weight: f64) {
        if subject.is_empty() || object.is_empty() || predicate.is_empty() {
            return;
        }
        let u = self.get_or_create(subject, temporal);
        let v = self.get_or_create(object, temporal);
        self.graph.add_edge(
            u,
            v,
            RelationEdge {
                predicate: predicate.to_string(),
                weight,
            },
        );
    }

    /// Case-insensitive поиск узла.
    pub fn find_node(&self, name: &str) -> Option<NodeIndex> {
        self.lookup.get(&name.to_lowercase()).copied()
    }

    fn temporal_ok(&self, a: NodeIndex, b: NodeIndex, filter: Option<&str>) -> bool {
        match filter {
            None => true,
            Some(f) => {
                let la = &self.graph[a].temporal_layer;
                let lb = &self.graph[b].temporal_layer;
                let ok_a = la.as_deref().map_or(true, |x| x == f);
                let ok_b = lb.as_deref().map_or(true, |x| x == f);
                ok_a && ok_b
            }
        }
    }

    /// Извлекает связный K-hop подграф вокруг сущности.
    ///
    /// Возвращает тройки `(subject, predicate, object)`, достижимые из `E0`
    /// за `< k` шагов BFS (обход в обе стороны), отфильтрованные по
    /// temporal-слою и ограниченные `max_relations`.
    pub fn extract_k_hop(
        &self,
        entity: &str,
        k: usize,
        temporal_filter: Option<&str>,
        max_relations: usize,
    ) -> Vec<(String, String, String)> {
        let mut out: Vec<(String, String, String)> = Vec::new();
        let Some(start) = self.find_node(entity) else {
            return out;
        };

        let mut visited: HashSet<NodeIndex> = HashSet::new();
        visited.insert(start);
        let mut queue: VecDeque<(NodeIndex, usize)> = VecDeque::new();
        queue.push_back((start, 0));
        let mut seen_triples: HashSet<(String, String, String)> = HashSet::new();

        while let Some((node, depth)) = queue.pop_front() {
            if depth >= k || out.len() >= max_relations {
                continue;
            }
            // исходящие рёбра: (node ->pred target)
            for edge in self.graph.edges_directed(node, Direction::Outgoing) {
                let target = edge.target();
                if self.temporal_ok(node, target, temporal_filter) {
                    let t = (
                        self.graph[node].name.clone(),
                        edge.weight().predicate.clone(),
                        self.graph[target].name.clone(),
                    );
                    if seen_triples.insert((t.0.to_lowercase(), t.1.clone(), t.2.to_lowercase())) {
                        out.push(t);
                        if out.len() >= max_relations {
                            break;
                        }
                    }
                }
                if !visited.contains(&target) {
                    visited.insert(target);
                    queue.push_back((target, depth + 1));
                }
            }
            // входящие рёбра: (source ->pred node)
            for edge in self.graph.edges_directed(node, Direction::Incoming) {
                let source = edge.source();
                if self.temporal_ok(source, node, temporal_filter) {
                    let t = (
                        self.graph[source].name.clone(),
                        edge.weight().predicate.clone(),
                        self.graph[node].name.clone(),
                    );
                    if seen_triples.insert((t.0.to_lowercase(), t.1.clone(), t.2.to_lowercase())) {
                        out.push(t);
                        if out.len() >= max_relations {
                            break;
                        }
                    }
                }
                if !visited.contains(&source) {
                    visited.insert(source);
                    queue.push_back((source, depth + 1));
                }
            }
        }
        out
    }
}

impl Default for EntityGraph {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn k_hop_bfs_depth_two() {
        let mut g = EntityGraph::new();
        g.add_triple("Нокс", "вонзила_когти", "Солнечное сплетение", None, 1.0);
        g.add_triple("Нокс", "взаимодействует_с", "Шунт", None, 1.0);
        g.add_triple("Шунт", "сбрасывает_тепло", "1300°C", None, 1.0);
        g.add_triple("Шунт", "появляется_в", "Глава 36", None, 1.0);

        let rel = g.extract_k_hop("Нокс", 2, None, 64);
        let flat: Vec<String> = rel.iter().map(|(s, p, o)| format!("{s}|{p}|{o}")).collect();
        // глубина 0: прямые рёбра Нокс
        assert!(flat.iter().any(|s| s == "Нокс|вонзила_когти|Солнечное сплетение"), "{flat:?}");
        // глубина 1: рёбра Шунта
        assert!(flat.iter().any(|s| s == "Шунт|сбрасывает_тепло|1300°C"), "{flat:?}");
    }

    #[test]
    fn k_hop_incoming_edges_traversed() {
        let mut g = EntityGraph::new();
        g.add_triple("Соболь", "по_имени", "Нокс", None, 1.0);
        let rel = g.extract_k_hop("Нокс", 1, None, 64);
        assert_eq!(rel.len(), 1);
        assert_eq!(rel[0], ("Соболь".into(), "по_имени".into(), "Нокс".into()));
    }

    #[test]
    fn case_insensitive_lookup_and_display_upgrade() {
        let mut g = EntityGraph::new();
        g.add_triple("нокс", "действует", "шунт", None, 1.0);
        // повторное добавление с заглавной повышает отображаемое имя
        g.add_triple("Нокс", "взаимодействует_с", "Шунт", None, 1.0);
        let rel = g.extract_k_hop("НОКС", 1, None, 64);
        assert!(rel.iter().any(|(s, _, _)| s == "Нокс"), "{rel:?}");
    }

    #[test]
    fn temporal_filter_drops_foreign_layers() {
        let mut g = EntityGraph::new();
        g.add_triple("Нокс", "появляется_в", "Глава 36", Some("Т-23"), 1.0);
        g.add_triple("Нокс", "появляется_в", "Глава 9", Some("Т-5"), 1.0);
        g.add_triple("Нокс", "живёт", "Каньон", None, 1.0);

        let all = g.extract_k_hop("Нокс", 1, None, 64);
        assert_eq!(all.len(), 3);

        let filtered = g.extract_k_hop("Нокс", 1, Some("Т-23"), 64);
        let flat: Vec<String> = filtered.iter().map(|(s, p, o)| format!("{s}|{p}|{o}")).collect();
        assert!(flat.iter().any(|s| s.contains("Глава 36")), "{flat:?}");
        assert!(flat.iter().any(|s| s.contains("Каньон")), "{flat:?}"); // слой неизвестен — проходим
        assert!(!flat.iter().any(|s| s.contains("Глава 9")), "{flat:?}");
    }

    #[test]
    fn max_relations_cap() {
        let mut g = EntityGraph::new();
        for i in 0..20 {
            g.add_triple("Нокс", "рёбра", &format!("obj_{i}"), None, 1.0);
        }
        let rel = g.extract_k_hop("Нокс", 1, None, 5);
        assert_eq!(rel.len(), 5);
    }

    #[test]
    fn missing_entity_returns_empty() {
        let mut g = EntityGraph::new();
        g.add_triple("a", "b", "c", None, 1.0);
        assert!(g.extract_k_hop("нет такой", 2, None, 10).is_empty());
    }

    #[test]
    fn empty_triple_ignored() {
        let mut g = EntityGraph::new();
        g.add_triple("", "p", "o", None, 1.0);
        g.add_triple("s", "", "o", None, 1.0);
        assert_eq!(g.node_count(), 0);
    }
}
