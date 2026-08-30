//! Граф сущностей (Entity-Relation Graph) с K-hop обходом.

pub mod entity_graph;

pub use entity_graph::{CodeSymbolRef, EntityGraph, IdentityPolicy, KnowledgeNode, RelationEdge};
