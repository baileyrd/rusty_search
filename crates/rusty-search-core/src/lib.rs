//! Backend-agnostic types for `rusty_search`: the standard interface that
//! makes search engines pluggable and replaceable, in the spirit of
//! SQLAlchemy's dialect system for databases.
//!
//! This crate defines *only* the shared vocabulary - [`Document`],
//! [`Schema`], [`Query`], [`SearchRequest`]/[`SearchResults`] and the
//! [`SearchBackend`] trait - and no concrete engine. Pick a backend crate
//! (`rusty-search-memory`, `rusty-search-tantivy`, ...) or implement
//! [`SearchBackend`] yourself to plug in a new one.

mod backend;
mod document;
mod error;
mod query;
mod result;
mod schema;

pub use backend::SearchBackend;
pub use document::{Document, DocumentId};
pub use error::{Result, SearchError};
pub use query::{Query, SearchRequest, Sort, SortOrder};
pub use result::{Hit, SearchResults};
pub use schema::{FieldDefinition, FieldOptions, FieldType, Schema, SchemaBuilder};
