//! Matchmaking attribute filter: a tiny SQL-like boolean expression evaluated against a ticket's
//! `attrs` (`rank >= 10 AND rank <= 20 AND region != 'cn'`). Comparisons on numbers, strings and
//! booleans; `AND`, `OR`, `NOT`, parentheses. Bounded: 256 chars, 16 comparisons.

use serde_json::{Map, Value};

pub const MAX_LEN: usize = 256;
pub const MAX_TERMS: usize = 16;

#[derive(Debug, Clone, PartialEq)]
pub enum Filter {
    Cmp(String, Op, Lit),
    And(Box<Filter>, Box<Filter>),
    Or(Box<Filter>, Box<Filter>),
    Not(Box<Filter>),
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Op {
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Lit {
    Num(f64),
    Str(String),
    Bool(bool),
}

#[derive(Debug, Clone, PartialEq)]
enum Tok {
    Ident(String),
    Num(f64),
    Str(String),
    Op(Op),
    And,
    Or,
    Not,
    LParen,
    RParen,
}

fn lex(src: &str) -> Result<Vec<Tok>, String> {
    let mut out = Vec::new();
    let cs: Vec<char> = src.chars().collect();
    let mut i = 0;
    while i < cs.len() {
        let c = cs[i];
        match c {
            ' ' | '\t' | '\n' | '\r' => i += 1,
            '(' => {
                out.push(Tok::LParen);
                i += 1;
            }
            ')' => {
                out.push(Tok::RParen);
                i += 1;
            }
            '=' => {
                out.push(Tok::Op(Op::Eq));
                i += 1;
                if i < cs.len() && cs[i] == '=' {
                    i += 1;
                }
            }
            '!' if i + 1 < cs.len() && cs[i + 1] == '=' => {
                out.push(Tok::Op(Op::Ne));
                i += 2;
            }
            '<' => {
                if i + 1 < cs.len() && cs[i + 1] == '>' {
                    out.push(Tok::Op(Op::Ne));
                    i += 2;
                } else if i + 1 < cs.len() && cs[i + 1] == '=' {
                    out.push(Tok::Op(Op::Le));
                    i += 2;
                } else {
                    out.push(Tok::Op(Op::Lt));
                    i += 1;
                }
            }
            '>' => {
                if i + 1 < cs.len() && cs[i + 1] == '=' {
                    out.push(Tok::Op(Op::Ge));
                    i += 2;
                } else {
                    out.push(Tok::Op(Op::Gt));
                    i += 1;
                }
            }
            '\'' | '"' => {
                let q = c;
                i += 1;
                let start = i;
                while i < cs.len() && cs[i] != q {
                    i += 1;
                }
                if i >= cs.len() {
                    return Err("unterminated string".into());
                }
                out.push(Tok::Str(cs[start..i].iter().collect()));
                i += 1;
            }
            c if c.is_ascii_digit() || c == '-' => {
                let start = i;
                i += 1;
                while i < cs.len() && (cs[i].is_ascii_digit() || cs[i] == '.') {
                    i += 1;
                }
                let s: String = cs[start..i].iter().collect();
                out.push(Tok::Num(
                    s.parse().map_err(|_| format!("bad number '{s}'"))?,
                ));
            }
            c if c.is_ascii_alphabetic() || c == '_' => {
                let start = i;
                while i < cs.len()
                    && (cs[i].is_ascii_alphanumeric() || cs[i] == '_' || cs[i] == '.')
                {
                    i += 1;
                }
                let s: String = cs[start..i].iter().collect();
                out.push(match s.to_ascii_uppercase().as_str() {
                    "AND" => Tok::And,
                    "OR" => Tok::Or,
                    "NOT" => Tok::Not,
                    "TRUE" => Tok::Ident("true".into()),
                    "FALSE" => Tok::Ident("false".into()),
                    _ => Tok::Ident(s),
                });
            }
            other => return Err(format!("unexpected character '{other}'")),
        }
    }
    Ok(out)
}

struct Parser {
    toks: Vec<Tok>,
    pos: usize,
    terms: usize,
}

impl Parser {
    fn peek(&self) -> Option<&Tok> {
        self.toks.get(self.pos)
    }
    fn next(&mut self) -> Option<Tok> {
        let t = self.toks.get(self.pos).cloned();
        self.pos += 1;
        t
    }
    fn or(&mut self) -> Result<Filter, String> {
        let mut left = self.and()?;
        while self.peek() == Some(&Tok::Or) {
            self.pos += 1;
            let right = self.and()?;
            left = Filter::Or(Box::new(left), Box::new(right));
        }
        Ok(left)
    }
    fn and(&mut self) -> Result<Filter, String> {
        let mut left = self.unary()?;
        while self.peek() == Some(&Tok::And) {
            self.pos += 1;
            let right = self.unary()?;
            left = Filter::And(Box::new(left), Box::new(right));
        }
        Ok(left)
    }
    fn unary(&mut self) -> Result<Filter, String> {
        match self.peek() {
            Some(Tok::Not) => {
                self.pos += 1;
                Ok(Filter::Not(Box::new(self.unary()?)))
            }
            Some(Tok::LParen) => {
                self.pos += 1;
                let inner = self.or()?;
                match self.next() {
                    Some(Tok::RParen) => Ok(inner),
                    _ => Err("expected ')'".into()),
                }
            }
            _ => self.cmp(),
        }
    }
    fn cmp(&mut self) -> Result<Filter, String> {
        let key = match self.next() {
            Some(Tok::Ident(k)) if k != "true" && k != "false" => k,
            other => return Err(format!("expected attribute name, got {other:?}")),
        };
        let op = match self.next() {
            Some(Tok::Op(op)) => op,
            other => return Err(format!("expected comparison after '{key}', got {other:?}")),
        };
        let lit = match self.next() {
            Some(Tok::Num(n)) => Lit::Num(n),
            Some(Tok::Str(s)) => Lit::Str(s),
            Some(Tok::Ident(b)) if b == "true" => Lit::Bool(true),
            Some(Tok::Ident(b)) if b == "false" => Lit::Bool(false),
            other => return Err(format!("expected value after '{key}', got {other:?}")),
        };
        self.terms += 1;
        if self.terms > MAX_TERMS {
            return Err(format!("more than {MAX_TERMS} comparisons"));
        }
        Ok(Filter::Cmp(key, op, lit))
    }
}

impl Filter {
    pub fn parse(src: &str) -> Result<Filter, String> {
        if src.trim().is_empty() {
            return Err("empty filter".into());
        }
        if src.len() > MAX_LEN {
            return Err(format!("filter longer than {MAX_LEN} chars"));
        }
        let toks = lex(src)?;
        let mut p = Parser {
            toks,
            pos: 0,
            terms: 0,
        };
        let f = p.or()?;
        if p.pos != p.toks.len() {
            return Err(format!("unexpected token {:?}", p.toks[p.pos]));
        }
        Ok(f)
    }

    /// A missing attribute fails every comparison (including `!=`), like SQL NULL.
    pub fn matches(&self, attrs: &Map<String, Value>) -> bool {
        match self {
            Filter::And(a, b) => a.matches(attrs) && b.matches(attrs),
            Filter::Or(a, b) => a.matches(attrs) || b.matches(attrs),
            Filter::Not(a) => !a.matches(attrs),
            Filter::Cmp(key, op, lit) => {
                let Some(v) = attrs.get(key) else {
                    return false;
                };
                let ord = match (v, lit) {
                    (Value::Number(n), Lit::Num(l)) => n.as_f64().map(|n| n.total_cmp(l)),
                    (Value::String(s), Lit::Str(l)) => Some(s.as_str().cmp(l.as_str())),
                    (Value::Bool(b), Lit::Bool(l)) => Some(b.cmp(l)),
                    _ => None,
                };
                let Some(ord) = ord else {
                    return false;
                };
                match op {
                    Op::Eq => ord.is_eq(),
                    Op::Ne => ord.is_ne(),
                    Op::Lt => ord.is_lt(),
                    Op::Le => ord.is_le(),
                    Op::Gt => ord.is_gt(),
                    Op::Ge => ord.is_ge(),
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn attrs(v: Value) -> Map<String, Value> {
        v.as_object().cloned().unwrap()
    }

    #[test]
    fn ranges_strings_bools() {
        let f =
            Filter::parse("rank >= 10 AND rank <= 20 AND map = 'dust' AND ranked = true").unwrap();
        assert!(f.matches(&attrs(json!({"rank": 15, "map": "dust", "ranked": true}))));
        assert!(!f.matches(&attrs(json!({"rank": 25, "map": "dust", "ranked": true}))));
        assert!(!f.matches(&attrs(json!({"rank": 15, "map": "nuke", "ranked": true}))));
        assert!(!f.matches(&attrs(json!({"rank": 15, "map": "dust"}))));
    }

    #[test]
    fn precedence_parens_not() {
        let f = Filter::parse("a = 1 OR b = 2 AND c = 3").unwrap();
        assert!(f.matches(&attrs(json!({"a": 1}))));
        assert!(!f.matches(&attrs(json!({"b": 2}))));
        assert!(f.matches(&attrs(json!({"b": 2, "c": 3}))));
        let g = Filter::parse("(a = 1 OR b = 2) AND NOT c <> 3").unwrap();
        assert!(g.matches(&attrs(json!({"a": 1, "c": 3}))));
        assert!(!g.matches(&attrs(json!({"a": 1, "c": 4}))));
    }

    #[test]
    fn missing_attribute_never_matches() {
        let f = Filter::parse("lang != 'en'").unwrap();
        assert!(!f.matches(&attrs(json!({}))));
        assert!(f.matches(&attrs(json!({"lang": "de"}))));
    }

    #[test]
    fn type_mismatch_is_false() {
        let f = Filter::parse("rank > 5").unwrap();
        assert!(!f.matches(&attrs(json!({"rank": "high"}))));
    }

    #[test]
    fn rejects_garbage_and_limits() {
        for bad in [
            "",
            "rank >",
            "rank 5",
            "(rank = 1",
            "rank = 1 )",
            "rank = 'x",
            "x ~ 1",
            "1 = 1",
        ] {
            assert!(Filter::parse(bad).is_err(), "{bad}");
        }
        let long = "a".repeat(MAX_LEN + 1);
        assert!(Filter::parse(&long).is_err());
        let many = (0..MAX_TERMS + 1)
            .map(|i| format!("k{i} = {i}"))
            .collect::<Vec<_>>()
            .join(" AND ");
        assert!(Filter::parse(&many).is_err());
        let ok = (0..MAX_TERMS)
            .map(|i| format!("k{i}={i}"))
            .collect::<Vec<_>>()
            .join(" AND ");
        assert!(Filter::parse(&ok).is_ok());
    }
}
