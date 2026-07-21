use serde::{Deserialize, Serialize};

use crate::document::{Document, DocumentId};

/// A single matched document, with its relevance score.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Hit {
    pub id: DocumentId,
    pub score: f32,
    pub document: Document,
}

/// The outcome of a [`crate::SearchBackend::search`] call.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct SearchResults {
    pub hits: Vec<Hit>,
    /// Total number of matching documents, ignoring `offset`/`limit`.
    pub total: usize,
}

impl SearchResults {
    pub fn empty() -> Self {
        Self::default()
    }
}
