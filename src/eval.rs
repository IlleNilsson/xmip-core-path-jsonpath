//! Evaluating a [`Query`] against a parsed document. The answer is a list of
//! locations rather than values, so that one evaluation serves both a read,
//! which wants the first, and a write, which wants every one and needs to
//! reach it mutably.

use crate::query::{Comparison, Filter, Query, Segment, Selector, Step};
use serde_json::Value;
use std::cmp::Ordering;

/// One step from a document's root towards a node.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Place {
    /// Into an object, by member name.
    Member(String),
    /// Into an array, by element position.
    Element(usize),
}

/// The steps from the root to one node the query selected.
pub type Location = Vec<Place>;

/// Every node `query` selects in `root`, in document order.
#[must_use]
pub fn locate(root: &Value, query: &Query) -> Vec<Location> {
    let mut current: Vec<(Location, &Value)> = vec![(Vec::new(), root)];
    for segment in &query.segments {
        let mut next = Vec::new();
        for (location, node) in current {
            match segment {
                Segment::Child(selectors) => apply(selectors, &location, node, &mut next),
                Segment::Descendant(selectors) => {
                    let mut all = Vec::new();
                    descend(&location, node, &mut all);
                    for (place, found) in all {
                        apply(selectors, &place, found, &mut next);
                    }
                }
            }
        }
        current = next;
    }
    current.into_iter().map(|(location, _)| location).collect()
}

/// The node at `location`, if the document still has one there.
#[must_use]
pub fn at<'a>(root: &'a Value, location: &Location) -> Option<&'a Value> {
    location.iter().try_fold(root, |node, place| match place {
        Place::Member(name) => node.get(name.as_str()),
        Place::Element(index) => node.get(*index),
    })
}

/// The node at `location`, mutably, if the document still has one there.
#[must_use]
pub fn at_mut<'a>(root: &'a mut Value, location: &Location) -> Option<&'a mut Value> {
    location.iter().try_fold(root, |node, place| match place {
        Place::Member(name) => node.get_mut(name.as_str()),
        Place::Element(index) => node.get_mut(*index),
    })
}

/// `node` first, then its children and theirs: array elements in order,
/// object members in key order. RFC 9535 leaves member order open, and the
/// parsed document keeps its members sorted, so this is the one order a
/// reader will see.
fn descend<'a>(location: &Location, node: &'a Value, out: &mut Vec<(Location, &'a Value)>) {
    out.push((location.clone(), node));
    for (place, child) in children(node) {
        let mut deeper = location.clone();
        deeper.push(place);
        descend(&deeper, child, out);
    }
}

fn children(node: &Value) -> Vec<(Place, &Value)> {
    match node {
        Value::Array(items) => items
            .iter()
            .enumerate()
            .map(|(index, item)| (Place::Element(index), item))
            .collect(),
        Value::Object(members) => members
            .iter()
            .map(|(name, member)| (Place::Member(name.clone()), member))
            .collect(),
        _ => Vec::new(),
    }
}

fn apply<'a>(
    selectors: &[Selector],
    location: &Location,
    node: &'a Value,
    out: &mut Vec<(Location, &'a Value)>,
) {
    for selector in selectors {
        for (place, child) in select(selector, node) {
            let mut deeper = location.clone();
            deeper.push(place);
            out.push((deeper, child));
        }
    }
}

fn select<'a>(selector: &Selector, node: &'a Value) -> Vec<(Place, &'a Value)> {
    match selector {
        Selector::Name(name) => node
            .as_object()
            .and_then(|members| members.get(name))
            .map(|member| vec![(Place::Member(name.clone()), member)])
            .unwrap_or_default(),
        Selector::Index(index) => node
            .as_array()
            .and_then(|items| {
                let position = normal(*index, items.len())?;
                items
                    .get(position)
                    .map(|item| vec![(Place::Element(position), item)])
            })
            .unwrap_or_default(),
        Selector::Wildcard => children(node),
        Selector::Slice { start, end, step } => node
            .as_array()
            .map(|items| slice(items, *start, *end, step.unwrap_or(1)))
            .unwrap_or_default(),
        Selector::Filter(filter) => children(node)
            .into_iter()
            .filter(|(_, child)| holds(filter, child))
            .collect(),
    }
}

/// A possibly negative index as a position; `None` when it falls before the
/// start. Past the end is left to the lookup.
fn normal(index: i64, len: usize) -> Option<usize> {
    let len = i64::try_from(len).ok()?;
    let position = if index < 0 { len + index } else { index };
    usize::try_from(position).ok()
}

/// RFC 9535 section 2.3.4.2.2, bounds normalised and clamped before stepping.
fn slice(items: &[Value], start: Option<i64>, end: Option<i64>, step: i64) -> Vec<(Place, &Value)> {
    let len = i64::try_from(items.len()).unwrap_or(i64::MAX);
    let normalise = |index: i64| if index < 0 { len + index } else { index };
    let mut out = Vec::new();
    let mut push = |position: i64| {
        if let Some(at) = usize::try_from(position)
            .ok()
            .filter(|at| *at < items.len())
        {
            out.push((Place::Element(at), &items[at]));
        }
    };
    if step > 0 {
        let lower = normalise(start.unwrap_or(0)).clamp(0, len);
        let upper = normalise(end.unwrap_or(len)).clamp(0, len);
        let mut position = lower;
        while position < upper {
            push(position);
            position += step;
        }
    } else if step < 0 {
        let upper = normalise(start.unwrap_or(len - 1)).clamp(-1, len - 1);
        let lower = normalise(end.unwrap_or(-len - 1)).clamp(-1, len - 1);
        let mut position = upper;
        while position > lower {
            push(position);
            position += step;
        }
    }
    out
}

fn holds(filter: &Filter, node: &Value) -> bool {
    let found = filter
        .path
        .iter()
        .try_fold(node, |current, step| match step {
            Step::Member(name) => current.as_object()?.get(name),
            Step::Element(index) => {
                let items = current.as_array()?;
                items.get(normal(*index, items.len())?)
            }
        });
    match &filter.test {
        None => found.is_some(),
        Some((comparison, literal)) => compare(found, literal, *comparison),
    }
}

/// RFC 9535 section 2.3.5.2.2: numbers compare as numbers, strings as
/// strings, everything else only for equality; a missing value equals nothing
/// and differs from everything.
fn compare(found: Option<&Value>, literal: &Value, comparison: Comparison) -> bool {
    let Some(found) = found else {
        return comparison == Comparison::Ne;
    };
    let order = match (found, literal) {
        (Value::Number(a), Value::Number(b)) => a
            .as_f64()
            .zip(b.as_f64())
            .and_then(|(a, b)| a.partial_cmp(&b)),
        (Value::String(a), Value::String(b)) => Some(a.cmp(b)),
        _ => None,
    };
    let equal = match order {
        Some(order) => order == Ordering::Equal,
        None => found == literal,
    };
    match comparison {
        Comparison::Eq => equal,
        Comparison::Ne => !equal,
        Comparison::Lt => order == Some(Ordering::Less),
        Comparison::Le => order.is_some_and(|order| order != Ordering::Greater),
        Comparison::Gt => order == Some(Ordering::Greater),
        Comparison::Ge => order.is_some_and(|order| order != Ordering::Less),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parser::parse;
    use serde_json::json;

    fn found(root: &Value, text: &str) -> Vec<Value> {
        locate(root, &parse(text).expect("parses"))
            .iter()
            .map(|location| at(root, location).expect("located").clone())
            .collect()
    }

    fn store() -> Value {
        json!({"store": {
            "book": [
                {"category": "fiction", "title": "Sayings", "price": 8.95},
                {"category": "fiction", "title": "Moby", "price": 12.99, "isbn": "0-553"},
                {"category": "reference", "title": "Lord", "price": 22.99, "isbn": "0-395"}
            ],
            "bicycle": {"color": "red", "price": 19.95}
        }})
    }

    #[test]
    fn selects_by_name_index_wildcard_and_descendant() {
        let doc = store();
        assert_eq!(found(&doc, "$.store.bicycle.color"), vec![json!("red")]);
        assert_eq!(
            found(&doc, "$['store']['book'][0]['title']"),
            vec![json!("Sayings")]
        );
        assert_eq!(found(&doc, "$.store.book[-1].title"), vec![json!("Lord")]);
        assert_eq!(
            found(&doc, "$.store.book[*].title"),
            vec![json!("Sayings"), json!("Moby"), json!("Lord")]
        );
        assert_eq!(
            found(&doc, "$..price"),
            vec![json!(19.95), json!(8.95), json!(12.99), json!(22.99)]
        );
        assert_eq!(found(&doc, "$..isbn"), vec![json!("0-553"), json!("0-395")]);
        assert_eq!(
            found(&doc, "$.store.book[0, -1].title"),
            vec![json!("Sayings"), json!("Lord")]
        );
        assert_eq!(found(&doc, "$.store.book[3]"), Vec::<Value>::new());
        assert_eq!(found(&doc, "$.store.book[-4]"), Vec::<Value>::new());
        assert_eq!(found(&doc, "$.store.bicycle[0]"), Vec::<Value>::new());
        assert_eq!(found(&doc, "$.nowhere.title"), Vec::<Value>::new());
    }

    #[test]
    fn slices_follow_the_rfc_including_negative_steps() {
        let doc = json!([0, 1, 2, 3, 4]);
        assert_eq!(found(&doc, "$[1:3]"), vec![json!(1), json!(2)]);
        assert_eq!(found(&doc, "$[:2]"), vec![json!(0), json!(1)]);
        assert_eq!(found(&doc, "$[3:]"), vec![json!(3), json!(4)]);
        assert_eq!(found(&doc, "$[-2:]"), vec![json!(3), json!(4)]);
        assert_eq!(found(&doc, "$[::2]"), vec![json!(0), json!(2), json!(4)]);
        assert_eq!(found(&doc, "$[::-2]"), vec![json!(4), json!(2), json!(0)]);
        assert_eq!(found(&doc, "$[3:0:-1]"), vec![json!(3), json!(2), json!(1)]);
        assert_eq!(
            found(&doc, "$[1:10]"),
            vec![json!(1), json!(2), json!(3), json!(4)]
        );
        assert_eq!(found(&doc, "$[::0]"), Vec::<Value>::new());
        assert_eq!(found(&doc, "$[3:1]"), Vec::<Value>::new());
        assert_eq!(found(&json!({"a": 1}), "$[0:1]"), Vec::<Value>::new());
    }

    #[test]
    fn filters_compare_and_test_existence() {
        let doc = store();
        assert_eq!(
            found(&doc, "$.store.book[?@.price < 10].title"),
            vec![json!("Sayings")]
        );
        assert_eq!(
            found(&doc, "$.store.book[?@.isbn].title"),
            vec![json!("Moby"), json!("Lord")]
        );
        assert_eq!(
            found(&doc, "$.store.book[?@.category == 'fiction'].title"),
            vec![json!("Sayings"), json!("Moby")]
        );
        assert_eq!(
            found(&doc, "$.store.book[?@.category != \"fiction\"].title"),
            vec![json!("Lord")]
        );
        assert_eq!(
            found(&doc, "$.store.book[?@.price >= 22.99].title"),
            vec![json!("Lord")]
        );
        assert_eq!(
            found(&doc, "$.store.book[?@.price > 22.99].title"),
            Vec::<Value>::new()
        );
        assert_eq!(
            found(&doc, "$.store.book[?@.price <= 8.95].title"),
            vec![json!("Sayings")]
        );
        assert_eq!(
            found(&doc, "$.store.book[?@.title == 8.95]"),
            Vec::<Value>::new()
        );
        assert_eq!(
            found(&doc, "$.store.book[?@.title < 8.95]"),
            Vec::<Value>::new()
        );
        assert_eq!(
            found(&doc, "$.store.book[?@.colour != null].title").len(),
            3
        );
        assert_eq!(
            found(&doc, "$.store.book[?@.colour == null].title").len(),
            0
        );
        let flags = json!([{"on": true}, {"on": false}, {"on": null}, {"on": 1}]);
        assert_eq!(found(&flags, "$[?@.on == true].on"), vec![json!(true)]);
        assert_eq!(found(&flags, "$[?@.on == null].on"), vec![json!(null)]);
        assert_eq!(found(&flags, "$[?@.on == 1].on"), vec![json!(1)]);
        assert_eq!(found(&flags, "$[?@.on == 1.0].on"), vec![json!(1)]);
        assert_eq!(
            found(&json!({"a": {"b": [{"c": 1}]}}), "$..[?@[0].c == 1]").len(),
            1
        );
    }

    #[test]
    fn a_location_reaches_the_node_mutably() {
        let mut doc = store();
        let locations = locate(&doc, &parse("$..price").expect("parses"));
        assert_eq!(locations.len(), 4);
        for location in &locations {
            *at_mut(&mut doc, location).expect("reaches") = json!(0);
        }
        assert_eq!(found(&doc, "$..price"), vec![json!(0); 4]);
        assert_eq!(at(&doc, &vec![Place::Member("x".into())]), None);
    }
}
