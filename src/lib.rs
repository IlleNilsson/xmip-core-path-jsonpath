#![forbid(unsafe_code)]

//! The `JSONPath` path technology — a technology of `xmip-core-path`.
//!
//! The language is RFC 9535: `$` at the root, then child segments (`.name`,
//! `['name']`, `[0]`, `[-1]`, `[*]`, `[1:3]`, `[?@.price < 10]`, `[?@.isbn]`)
//! and descendant segments (`..name`, `..[*]`). A query may select many nodes;
//! a read takes the first in document order, and a write replaces every one.
//!
//! Three things, because a path language is nothing without content to address:
//! [`JsonPathEngine`], the [`PathEngine`] for the language `jsonpath`;
//! [`JsonPathStructure`], a [`StructureReader`] over a JSON Stream; and
//! [`JsonPathRewrite`], a [`StructureWriter`] that produces a new Stream with
//! every selected value replaced, as ADR-0013 asks of anything that changes
//! content. Promote reads through the first two; demote writes through the
//! first and third; route and process read. The JSON document itself — parsed
//! once, bridged to a scalar, written back as a Stream — is the capability's
//! `json`, shared with the other JSON languages (ADR-0044); what is this
//! technology's is the query.
//!
//! A value read is a scalar — null, boolean, number, string — and an object or
//! array at the first match is refused rather than stringified, because a
//! promoted property is one value, not a document. A write into a query that
//! selects nothing is refused: demote names a place, it does not invent one.

mod eval;
mod parser;
mod query;

pub use eval::{Location, Place, at, at_mut, locate};
pub use parser::parse;
pub use query::{Comparison, Filter, Query, Segment, Selector, Step};

use path::json::{self, Document, Rewrite};
use path::{Path, PathCost, PathEngine};
use sdk::contract::{
    ContractDescriptor, ContractError, StructureReader, StructureWriter, StructuredValue,
};
use stream::Stream;
use xcore::StreamId;

/// The `jsonpath` engine. The reader speaks queries already, so the engine
/// adds no traversal of its own.
pub struct JsonPathEngine;

impl PathEngine for JsonPathEngine {
    fn language(&self) -> &'static str {
        "jsonpath"
    }

    fn read(
        &self,
        reader: &dyn StructureReader,
        path: &Path,
    ) -> Result<Option<StructuredValue>, ContractError> {
        reader.read(&path.expression)
    }

    fn write(
        &self,
        writer: &mut dyn StructureWriter,
        path: &Path,
        value: StructuredValue,
    ) -> Result<(), ContractError> {
        writer.write(&path.expression, value)
    }

    /// A descendant or filter segment can only be answered against the whole
    /// document, and JSON has no prefix a plain one can be read from either.
    fn cost(&self, _path: &Path) -> PathCost {
        PathCost::Materialized
    }
}

/// A JSON Stream, read by query.
pub struct JsonPathStructure {
    document: Document,
}

impl JsonPathStructure {
    /// Parse `stream` once; every read is a query evaluation after that.
    ///
    /// # Errors
    /// The Stream is not JSON.
    pub fn parse(stream: &Stream) -> Result<Self, ContractError> {
        Ok(Self {
            document: Document::parse(stream)?,
        })
    }
}

impl StructureReader for JsonPathStructure {
    fn contract(&self) -> &ContractDescriptor {
        &self.document.descriptor
    }

    fn read(&self, path: &str) -> Result<Option<StructuredValue>, ContractError> {
        let query = parse(path)?;
        let value = &self.document.value;
        locate(value, &query)
            .first()
            .and_then(|location| at(value, location))
            .map(|found| json::scalar(found, path))
            .transpose()
    }
}

/// A JSON Stream being rewritten into a new one.
pub struct JsonPathRewrite {
    rewrite: Rewrite,
}

impl JsonPathRewrite {
    /// Start from `stream`; the Stream `finish` produces carries `id`.
    ///
    /// # Errors
    /// The Stream is not JSON.
    pub fn of(stream: &Stream, id: StreamId) -> Result<Self, ContractError> {
        Ok(Self {
            rewrite: Rewrite::of(stream, id)?,
        })
    }
}

impl StructureWriter for JsonPathRewrite {
    fn contract(&self) -> &ContractDescriptor {
        &self.rewrite.descriptor
    }

    /// Replace the value at every node `path` selects. Nothing selected is
    /// refused. Where one selected node lies inside another, the outer
    /// replacement stands.
    fn write(&mut self, path: &str, value: StructuredValue) -> Result<(), ContractError> {
        let query = parse(path)?;
        let replacement = json::from_scalar(value)?;
        let locations = locate(&self.rewrite.value, &query);
        if locations.is_empty() {
            return Err(ContractError::new(format!(
                "{path:?} selects nothing to write into"
            )));
        }
        for location in locations.iter().rev() {
            if let Some(node) = at_mut(&mut self.rewrite.value, location) {
                *node = replacement.clone();
            }
        }
        Ok(())
    }

    fn finish(self: Box<Self>) -> Result<Stream, ContractError> {
        self.rewrite.finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use path::fixture::stream;
    use serde_json::Value;

    const STORE: &str = concat!(
        r#"{"store":{"book":["#,
        r#"{"category":"fiction","title":"Sayings","price":8.95},"#,
        r#"{"category":"fiction","title":"Moby","price":12.99,"isbn":"0-553"},"#,
        r#"{"category":"reference","title":"Lord","price":22.99,"isbn":"0-395"}],"#,
        r#""bicycle":{"color":"red","price":19.95,"stock":3}}}"#
    );

    #[test]
    fn reads_the_first_scalar_match_and_refuses_structures() {
        let structure = JsonPathStructure::parse(&stream(STORE)).expect("parses");
        let engine = JsonPathEngine;
        let read = |p: &str| engine.read(&structure, &Path::new("jsonpath", p));
        assert_eq!(
            read("$.store.bicycle.color").expect("reads"),
            Some(StructuredValue::Text("red".into()))
        );
        assert_eq!(
            read("$.store.bicycle.stock").expect("reads"),
            Some(StructuredValue::Integer(3))
        );
        assert_eq!(
            read("$.store.book[-1].price").expect("reads"),
            Some(StructuredValue::Decimal(22.99))
        );
        assert_eq!(
            read("$..isbn").expect("reads"),
            Some(StructuredValue::Text("0-553".into()))
        );
        assert_eq!(
            read("$.store.book[1:].title").expect("reads"),
            Some(StructuredValue::Text("Moby".into()))
        );
        assert_eq!(
            read("$.store.book[?@.price > 20].title").expect("reads"),
            Some(StructuredValue::Text("Lord".into()))
        );
        assert_eq!(
            read("$.store.book[*].title").expect("reads"),
            Some(StructuredValue::Text("Sayings".into()))
        );
        assert_eq!(
            read("$.store.book[?@.price > 100].title").expect("reads"),
            None
        );
        assert_eq!(read("$.nowhere").expect("reads"), None);
        assert!(read("$.store.book").is_err());
        assert!(read("$").is_err());
        assert!(read("store.book").is_err());
        assert!(read("$.store.book[?@.price >]").is_err());
        assert_eq!(
            engine.cost(&Path::new("jsonpath", "$.store")),
            PathCost::Materialized
        );
    }

    #[test]
    fn rewrites_every_match_into_a_new_stream_with_the_given_id() {
        let mut rewrite = JsonPathRewrite::of(&stream(STORE), StreamId::new(2)).expect("parses");
        let engine = JsonPathEngine;
        let mut write =
            |p: &str, v: StructuredValue| engine.write(&mut rewrite, &Path::new("jsonpath", p), v);
        write("$.store.book[*].price", StructuredValue::Integer(1)).expect("writes all");
        write("$..color", StructuredValue::Text("blue".into())).expect("writes");
        write("$.store.book[?@.isbn].isbn", StructuredValue::Null).expect("writes");
        write("$.store.bicycle", StructuredValue::Bool(false)).expect("writes a structure");
        assert!(write("$.store.nothing", StructuredValue::Integer(0)).is_err());
        assert!(write("$.store.book[9].price", StructuredValue::Integer(0)).is_err());
        assert!(write("$.store", StructuredValue::Binary(vec![1])).is_err());
        assert!(write("$[", StructuredValue::Null).is_err());
        let out = Box::new(rewrite).finish().expect("finishes");
        assert_eq!(out.id(), StreamId::new(2));
        assert_eq!(out.media_type(), Some("application/json"));
        let back: Value = serde_json::from_slice(out.bytes()).expect("json");
        assert_eq!(
            back,
            serde_json::json!({"store": {"book": [
                {"category": "fiction", "title": "Sayings", "price": 1},
                {"category": "fiction", "title": "Moby", "price": 1, "isbn": null},
                {"category": "reference", "title": "Lord", "price": 1, "isbn": null}
            ], "bicycle": false}})
        );
    }

    #[test]
    fn a_stream_that_is_not_json_is_refused_up_front() {
        assert!(JsonPathStructure::parse(&stream("{nope")).is_err());
        assert!(JsonPathRewrite::of(&stream("{nope"), StreamId::new(1)).is_err());
    }
}
