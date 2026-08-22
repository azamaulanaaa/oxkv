//! Pest-generated parser for the Lucene-style query grammar.
//!
//! The derive macro generates undocumented items (`Rule`, `Parser` impl),
//! so the `missing_docs` lint is disabled for this module.
#![allow(missing_docs)]

use pest_derive::Parser;

#[derive(Parser)]
#[grammar = "query/query.pest"]
pub enum LuceneParser {}
