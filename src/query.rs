//! The parsed form of an RFC 9535 query: `$` followed by segments, each a
//! child or descendant step carrying one or more selectors.

use serde_json::Value;

/// A whole query, root first. No segments is `$` alone.
#[derive(Clone, Debug, PartialEq)]
pub struct Query {
    /// The segments after `$`, in order.
    pub segments: Vec<Segment>,
}

/// One segment: `.name`, `[...]` or their descendant forms `..name`, `..[...]`.
#[derive(Clone, Debug, PartialEq)]
pub enum Segment {
    /// Selects among the children of each current node.
    Child(Vec<Selector>),
    /// Selects among each current node, its children and all their
    /// descendants, in document order.
    Descendant(Vec<Selector>),
}

/// One selector inside a segment.
#[derive(Clone, Debug, PartialEq)]
pub enum Selector {
    /// A member by name: `.name`, `['name']`, `["name"]`.
    Name(String),
    /// An element by index; negative counts from the end: `[0]`, `[-1]`.
    Index(i64),
    /// Every child: `.*`, `[*]`.
    Wildcard,
    /// A run of elements: `[start:end:step]`, each part optional.
    Slice {
        /// Where the run starts; the first element when absent.
        start: Option<i64>,
        /// Where the run stops, exclusive; the end when absent.
        end: Option<i64>,
        /// The stride; one when absent, and zero selects nothing.
        step: Option<i64>,
    },
    /// The children that satisfy a test: `[?@.field == value]`, `[?@.field]`.
    Filter(Filter),
}

/// A filter's test: a singular query under `@`, existence alone or compared
/// against a literal.
#[derive(Clone, Debug, PartialEq)]
pub struct Filter {
    /// The steps from the candidate node to the value under test.
    pub path: Vec<Step>,
    /// The comparison and the literal, or none for a bare existence test.
    pub test: Option<(Comparison, Value)>,
}

/// One step of a filter's singular query.
#[derive(Clone, Debug, PartialEq)]
pub enum Step {
    /// A member by name.
    Member(String),
    /// An element by index; negative counts from the end.
    Element(i64),
}

/// The six comparison operators of RFC 9535 section 2.3.5.2.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Comparison {
    /// `==`
    Eq,
    /// `!=`
    Ne,
    /// `<`
    Lt,
    /// `<=`
    Le,
    /// `>`
    Gt,
    /// `>=`
    Ge,
}
