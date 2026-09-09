//! Parsing an RFC 9535 query text into a [`Query`]. A hand-written
//! descent over the text: the grammar is small and a parser that names its
//! own errors is worth more than a generated one here.

use crate::query::{Comparison, Filter, Query, Segment, Selector, Step};
use contract::ContractError;
use serde_json::Value;

/// Parse `text` as a query.
///
/// # Errors
/// The text does not start with `$`, or a segment, selector, string, number
/// or filter in it is malformed; the message names the offset.
pub fn parse(text: &str) -> Result<Query, ContractError> {
    let mut parser = Parser { text, at: 0 };
    parser.expect('$')?;
    let mut segments = Vec::new();
    while !parser.done() {
        segments.push(parser.segment()?);
    }
    Ok(Query { segments })
}

struct Parser<'a> {
    text: &'a str,
    at: usize,
}

impl Parser<'_> {
    fn done(&self) -> bool {
        self.at >= self.text.len()
    }

    fn rest(&self) -> &str {
        &self.text[self.at..]
    }

    fn peek(&self) -> Option<char> {
        self.rest().chars().next()
    }

    fn take(&mut self, prefix: &str) -> bool {
        let found = self.rest().starts_with(prefix);
        if found {
            self.at += prefix.len();
        }
        found
    }

    fn expect(&mut self, symbol: char) -> Result<(), ContractError> {
        if self.take(symbol.encode_utf8(&mut [0; 4])) {
            Ok(())
        } else {
            Err(self.refuse(&format!("expected {symbol:?}")))
        }
    }

    fn skip_space(&mut self) {
        while matches!(self.peek(), Some(' ' | '\t' | '\n' | '\r')) {
            self.at += 1;
        }
    }

    fn refuse(&self, why: &str) -> ContractError {
        ContractError {
            message: format!("query {:?} at {}: {why}", self.text, self.at),
        }
    }

    fn segment(&mut self) -> Result<Segment, ContractError> {
        if self.take("..") {
            let selectors = if self.peek() == Some('[') {
                self.bracketed()?
            } else {
                self.shorthand()?
            };
            Ok(Segment::Descendant(selectors))
        } else if self.take(".") {
            Ok(Segment::Child(self.shorthand()?))
        } else if self.peek() == Some('[') {
            Ok(Segment::Child(self.bracketed()?))
        } else {
            Err(self.refuse("expected a segment"))
        }
    }

    fn shorthand(&mut self) -> Result<Vec<Selector>, ContractError> {
        if self.take("*") {
            return Ok(vec![Selector::Wildcard]);
        }
        Ok(vec![Selector::Name(self.name()?)])
    }

    fn name(&mut self) -> Result<String, ContractError> {
        let start = self.at;
        while let Some(symbol) = self.peek() {
            if symbol.is_alphanumeric() || symbol == '_' || !symbol.is_ascii() {
                self.at += symbol.len_utf8();
            } else {
                break;
            }
        }
        if self.at == start {
            return Err(self.refuse("expected a name"));
        }
        Ok(self.text[start..self.at].to_string())
    }

    fn bracketed(&mut self) -> Result<Vec<Selector>, ContractError> {
        self.expect('[')?;
        let mut selectors = Vec::new();
        loop {
            self.skip_space();
            selectors.push(self.selector()?);
            self.skip_space();
            if self.take(",") {
                continue;
            }
            self.expect(']')?;
            return Ok(selectors);
        }
    }

    fn selector(&mut self) -> Result<Selector, ContractError> {
        match self.peek() {
            Some('\'' | '"') => Ok(Selector::Name(self.quoted()?)),
            Some('*') => {
                self.at += 1;
                Ok(Selector::Wildcard)
            }
            Some('?') => {
                self.at += 1;
                Ok(Selector::Filter(self.filter()?))
            }
            Some(symbol) if symbol == '-' || symbol == ':' || symbol.is_ascii_digit() => {
                self.index_or_slice()
            }
            _ => Err(self.refuse("expected a selector")),
        }
    }

    fn integer(&mut self) -> Result<Option<i64>, ContractError> {
        let start = self.at;
        self.take("-");
        while self.peek().is_some_and(|symbol| symbol.is_ascii_digit()) {
            self.at += 1;
        }
        let token = &self.text[start..self.at];
        if token.is_empty() {
            return Ok(None);
        }
        token
            .parse()
            .map(Some)
            .map_err(|_| self.refuse(&format!("{token} is not an integer")))
    }

    fn index_or_slice(&mut self) -> Result<Selector, ContractError> {
        let start = self.integer()?;
        self.skip_space();
        if !self.take(":") {
            return start
                .map(Selector::Index)
                .ok_or_else(|| self.refuse("expected an index"));
        }
        self.skip_space();
        let end = self.integer()?;
        self.skip_space();
        let step = if self.take(":") {
            self.skip_space();
            self.integer()?
        } else {
            None
        };
        Ok(Selector::Slice { start, end, step })
    }

    fn quoted(&mut self) -> Result<String, ContractError> {
        let quote = self
            .peek()
            .ok_or_else(|| self.refuse("expected a string"))?;
        self.at += 1;
        let mut out = String::new();
        loop {
            let Some(symbol) = self.peek() else {
                return Err(self.refuse("a string is never closed"));
            };
            self.at += symbol.len_utf8();
            if symbol == quote {
                return Ok(out);
            }
            if symbol != '\\' {
                out.push(symbol);
                continue;
            }
            let Some(escaped) = self.peek() else {
                return Err(self.refuse("a string is never closed"));
            };
            self.at += escaped.len_utf8();
            out.push(match escaped {
                'n' => '\n',
                't' => '\t',
                'r' => '\r',
                'b' => '\u{8}',
                'f' => '\u{c}',
                '/' | '\\' | '\'' | '"' => escaped,
                _ => return Err(self.refuse(&format!("unknown escape \\{escaped}"))),
            });
        }
    }

    fn filter(&mut self) -> Result<Filter, ContractError> {
        self.skip_space();
        self.expect('@')?;
        let mut path = Vec::new();
        loop {
            if self.take(".") {
                path.push(Step::Member(self.name()?));
            } else if self.take("[") {
                self.skip_space();
                let step = match self.peek() {
                    Some('\'' | '"') => Step::Member(self.quoted()?),
                    _ => Step::Element(
                        self.integer()?
                            .ok_or_else(|| self.refuse("expected a name or index"))?,
                    ),
                };
                self.skip_space();
                self.expect(']')?;
                path.push(step);
            } else {
                break;
            }
        }
        self.skip_space();
        let test = match self.comparison() {
            Some(comparison) => {
                self.skip_space();
                Some((comparison, self.literal()?))
            }
            None => None,
        };
        Ok(Filter { path, test })
    }

    fn comparison(&mut self) -> Option<Comparison> {
        let operators = [
            ("==", Comparison::Eq),
            ("!=", Comparison::Ne),
            ("<=", Comparison::Le),
            (">=", Comparison::Ge),
            ("<", Comparison::Lt),
            (">", Comparison::Gt),
        ];
        operators
            .into_iter()
            .find(|(token, _)| self.take(token))
            .map(|(_, comparison)| comparison)
    }

    fn literal(&mut self) -> Result<Value, ContractError> {
        if self.take("null") {
            return Ok(Value::Null);
        }
        if self.take("true") {
            return Ok(Value::Bool(true));
        }
        if self.take("false") {
            return Ok(Value::Bool(false));
        }
        if matches!(self.peek(), Some('\'' | '"')) {
            return Ok(Value::String(self.quoted()?));
        }
        let start = self.at;
        while self
            .peek()
            .is_some_and(|symbol| symbol.is_ascii_digit() || "-+.eE".contains(symbol))
        {
            self.at += 1;
        }
        let token = &self.text[start..self.at];
        serde_json::from_str(token)
            .ok()
            .filter(Value::is_number)
            .ok_or_else(|| self.refuse("expected a literal"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn segments(text: &str) -> Vec<Segment> {
        parse(text).expect("parses").segments
    }

    #[test]
    fn parses_child_and_descendant_segments() {
        assert_eq!(segments("$"), vec![]);
        assert_eq!(
            segments("$.store.book[0]['title']"),
            vec![
                Segment::Child(vec![Selector::Name("store".into())]),
                Segment::Child(vec![Selector::Name("book".into())]),
                Segment::Child(vec![Selector::Index(0)]),
                Segment::Child(vec![Selector::Name("title".into())]),
            ]
        );
        assert_eq!(
            segments("$..price"),
            vec![Segment::Descendant(vec![Selector::Name("price".into())])]
        );
        assert_eq!(
            segments("$..[*].*"),
            vec![
                Segment::Descendant(vec![Selector::Wildcard]),
                Segment::Child(vec![Selector::Wildcard]),
            ]
        );
        assert_eq!(
            segments(r#"$[-1, "a\"b", 'c\'d']"#),
            vec![Segment::Child(vec![
                Selector::Index(-1),
                Selector::Name("a\"b".into()),
                Selector::Name("c'd".into()),
            ])]
        );
    }

    #[test]
    fn parses_slices_in_every_shape() {
        let slice = |start, end, step| Segment::Child(vec![Selector::Slice { start, end, step }]);
        assert_eq!(segments("$[1:3]"), vec![slice(Some(1), Some(3), None)]);
        assert_eq!(segments("$[:2]"), vec![slice(None, Some(2), None)]);
        assert_eq!(segments("$[::-1]"), vec![slice(None, None, Some(-1))]);
        assert_eq!(segments("$[ -2 : ]"), vec![slice(Some(-2), None, None)]);
    }

    #[test]
    fn parses_filters_with_and_without_a_comparison() {
        let filter = |path, test| Segment::Child(vec![Selector::Filter(Filter { path, test })]);
        assert_eq!(
            segments("$[?@.price < 10]"),
            vec![filter(
                vec![Step::Member("price".into())],
                Some((Comparison::Lt, json!(10)))
            )]
        );
        assert_eq!(
            segments("$[?@.isbn]"),
            vec![filter(vec![Step::Member("isbn".into())], None)]
        );
        assert_eq!(
            segments("$[?@['a'][0].b == 'x']"),
            vec![filter(
                vec![
                    Step::Member("a".into()),
                    Step::Element(0),
                    Step::Member("b".into())
                ],
                Some((Comparison::Eq, json!("x")))
            )]
        );
        for (text, comparison, literal) in [
            ("$[?@.a!=null]", Comparison::Ne, json!(null)),
            ("$[?@.a >= true]", Comparison::Ge, json!(true)),
            ("$[?@.a <= false]", Comparison::Le, json!(false)),
            ("$[?@.a > -1.5e2]", Comparison::Gt, json!(-150.0)),
        ] {
            assert_eq!(
                segments(text),
                vec![filter(
                    vec![Step::Member("a".into())],
                    Some((comparison, literal))
                )],
                "{text}"
            );
        }
    }

    #[test]
    fn refuses_what_is_not_a_query() {
        for bad in [
            "",
            "store",
            "$.",
            "$[",
            "$[]",
            "$['a",
            "$[?]",
            "$[?@.a ==]",
            "$[?a == 1]",
            "$.a b",
            "$[1,]",
            "$['a\\q']",
            "$[?@.a == nope]",
            "$[?@[]]",
            "$..",
        ] {
            assert!(parse(bad).is_err(), "{bad:?} should be refused");
        }
    }
}
