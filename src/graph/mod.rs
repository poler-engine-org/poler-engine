//! Граф сущностей (Entity-Relation Graph) с K-hop обходом;
//! коннектом FLYCSR1 (мозг мухи FlyWire v783) как матрица A.

pub mod connectome;
pub mod entity_graph;
pub mod flyops;

pub use connectome::{Connectome, ConnectomeNodes, Edge, InEdges, KHopReport, SignFilter};
pub use entity_graph::{CodeSymbolRef, EntityGraph, IdentityPolicy, KnowledgeNode, RelationEdge};
pub use flyops::{
    CommonPartner, Direction, MotifReport, PageRankReport, PathHop, PathReport, PropagateReport,
    PropagateStep, RotorPair,
};
