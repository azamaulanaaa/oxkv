//! Pest-generated parser for the Lucene-style query grammar.
//!
//! The derive macro generates items without documentation: `Rule` and the
//! `Parser` impl. This module disables the `missing_docs` lint for them.
#![allow(missing_docs)]

use pest_derive::Parser;

#[derive(Parser)]
#[grammar = "query/query.pest"]
pub enum LuceneParser {}
