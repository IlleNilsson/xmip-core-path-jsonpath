#![forbid(unsafe_code)]

//! The `JSONPath` path technology — a technology of `xmip-core-path`.
//!
//! The language is RFC 9535: `$` at the root, then child segments (`.name`,
//! `['name']`, `[0]`, `[-1]`, `[*]`, `[1:3]`, `[?@.price < 10]`, `[?@.isbn]`)
//! and descendant segments (`..name`, `..[*]`). A query may select many nodes;
//! a read takes the first in document order, and a write replaces every one.
//!
//! [`JsonPathLanguage`] is the [`PathLanguage`] `jsonpath`: it compiles a
//! query once, and the compiled [`Query`] reads the first match from a
//! Stream's JSON and writes every match in a rewrite of it (ADR-0013).
//! Promote reads through it; demote writes through it; route and process
//! read. The JSON document itself — parsed once per Message, bridged to a
//! scalar, written back as a Stream — is the capability's `json`, shared with
//! the other JSON languages (ADR-0044); what is this technology's is the
//! query.
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

use contract::ContractError;
use path::{CompiledExpression, Content, Namespaces, PathLanguage, Rewriting, json};
use serde_json::Value;
use xcore::ScalarValue;

/// The language `jsonpath`.
pub struct JsonPathLanguage;

impl PathLanguage for JsonPathLanguage {
    fn language(&self) -> &'static str {
        "jsonpath"
    }

    fn compile(
        &self,
        expression: &str,
        namespaces: &Namespaces,
    ) -> Result<Box<dyn CompiledExpression>, ContractError> {
        namespaces.unused_by(self.language())?;
        Ok(Box::new(Compiled {
            query: parse(expression)?,
            expression: expression.to_string(),
        }))
    }
}

/// A query compiled, with the text a refusal names.
struct Compiled {
    query: Query,
    expression: String,
}

impl CompiledExpression for Compiled {
    fn read(&self, content: &Content<'_>) -> Result<Option<ScalarValue>, ContractError> {
        let document = content.form::<Value>()?;
        locate(&document, &self.query)
            .first()
            .and_then(|location| at(&document, location))
            .map(|found| json::scalar(found, &self.expression))
            .transpose()
    }

    /// Replace the value at every node the query selects. Nothing selected is
    /// refused. Where one selected node lies inside another, the outer
    /// replacement stands.
    fn write(&self, rewriting: &mut Rewriting, value: ScalarValue) -> Result<(), ContractError> {
        let replacement = json::from_scalar(value)?;
        let document = rewriting.form_mut::<Value>()?;
        let locations = locate(document, &self.query);
        if locations.is_empty() {
            return Err(ContractError::new(format!(
                "{:?} selects nothing to write into",
                self.expression
            )));
        }
        for location in locations.iter().rev() {
            if let Some(node) = at_mut(document, location) {
                *node = replacement.clone();
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use contract::fixture::stream;
    use xcore::StreamId;

    const STORE: &str = concat!(
        r#"{"store":{"book":["#,
        r#"{"category":"fiction","title":"Sayings","price":8.95},"#,
        r#"{"category":"fiction","title":"Moby","price":12.99,"isbn":"0-553"},"#,
        r#"{"category":"reference","title":"Lord","price":22.99,"isbn":"0-395"}],"#,
        r#""bicycle":{"color":"red","price":19.95,"stock":3}}}"#
    );

    fn compiled(query: &str) -> Result<Box<dyn CompiledExpression>, ContractError> {
        JsonPathLanguage.compile(query, &Namespaces::new())
    }

    #[test]
    fn reads_the_first_scalar_match_and_refuses_structures() {
        let store = stream(STORE);
        let content = Content::of(&store);
        let read = |p: &str| compiled(p)?.read(&content);
        assert_eq!(
            read("$.store.bicycle.color").expect("reads"),
            Some(ScalarValue::Text("red".into()))
        );
        assert_eq!(
            read("$.store.bicycle.stock").expect("reads"),
            Some(ScalarValue::Integer(3))
        );
        assert_eq!(
            read("$.store.book[-1].price").expect("reads"),
            Some(ScalarValue::Decimal(22.99))
        );
        assert_eq!(
            read("$..isbn").expect("reads"),
            Some(ScalarValue::Text("0-553".into()))
        );
        assert_eq!(
            read("$.store.book[1:].title").expect("reads"),
            Some(ScalarValue::Text("Moby".into()))
        );
        assert_eq!(
            read("$.store.book[?@.price > 20].title").expect("reads"),
            Some(ScalarValue::Text("Lord".into()))
        );
        assert_eq!(
            read("$.store.book[*].title").expect("reads"),
            Some(ScalarValue::Text("Sayings".into()))
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
    }

    #[test]
    fn a_path_binding_a_namespace_is_refused_for_json_has_none() {
        let bound = Namespaces::new().bind("o", "urn:example:order");
        let refused = JsonPathLanguage
            .compile("$.store", &bound)
            .map(drop)
            .expect_err("refused");
        assert_eq!(
            refused.message,
            "jsonpath has no namespaces, and the path binds o"
        );
    }

    #[test]
    fn rewrites_every_match_into_a_new_stream_with_the_given_id() {
        let mut rewriting = Rewriting::of(&stream(STORE), StreamId::new(2));
        let mut write =
            |p: &str, v: ScalarValue| compiled(p).and_then(|c| c.write(&mut rewriting, v));
        write("$.store.book[*].price", ScalarValue::Integer(1)).expect("writes all");
        write("$..color", ScalarValue::Text("blue".into())).expect("writes");
        write("$.store.book[?@.isbn].isbn", ScalarValue::Null).expect("writes");
        write("$.store.bicycle", ScalarValue::Bool(false)).expect("writes a structure");
        assert!(write("$.store.nothing", ScalarValue::Integer(0)).is_err());
        assert!(write("$.store.book[9].price", ScalarValue::Integer(0)).is_err());
        assert!(write("$.store", ScalarValue::Binary(vec![1])).is_err());
        assert!(write("$[", ScalarValue::Null).is_err());
        let out = rewriting.finish().expect("finishes");
        assert_eq!(out.id(), StreamId::new(2));
        assert_eq!(out.media_type(), Some("application/json"));
        let back: Value = serde_json::from_slice(out.load().expect("held")).expect("json");
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
    fn a_stream_that_is_not_json_is_refused() {
        let broken = stream("{nope");
        let query = compiled("$.a").expect("compiles");
        assert!(query.read(&Content::of(&broken)).is_err());
        assert!(
            query
                .write(
                    &mut Rewriting::of(&broken, StreamId::new(1)),
                    ScalarValue::Null
                )
                .is_err()
        );
    }
}
