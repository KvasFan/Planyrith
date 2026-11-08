//! Implements docs/04-document-model.md §9.1 parametric components: the
//! formula language of parameters and part rules. A formula is arithmetic
//! (`+ - * /`, parentheses, unary minus) on numbers (millimeters; a length
//! with a unit such as `18mm`, `2cm`, `0.6m`, `1in`), parameter names and
//! the functions `min`, `max`, `round`, `floor`, `ceil`, `abs`. Evaluation
//! is deterministic; parameters may use other parameters, and cycles are
//! errors.

use std::collections::BTreeMap;

/// A formula error, with what went wrong.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{0}")]
pub struct FormulaError(pub String);

fn err(text: impl Into<String>) -> FormulaError {
    FormulaError(text.into())
}

#[derive(Debug, Clone, PartialEq)]
enum Token {
    Number(f64),
    Name(String),
    Op(char),
}

fn tokens(text: &str) -> Result<Vec<Token>, FormulaError> {
    let chars: Vec<char> = text.chars().collect();
    let mut out = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if c.is_whitespace() {
            i += 1;
        } else if c.is_ascii_digit()
            || (c == '.' && chars.get(i + 1).is_some_and(char::is_ascii_digit))
        {
            let start = i;
            while i < chars.len() && (chars[i].is_ascii_digit() || chars[i] == '.') {
                i += 1;
            }
            let number: String = chars[start..i].iter().collect();
            let mut value: f64 = number
                .parse()
                .map_err(|_| err(format!("`{number}` is not a number")))?;
            // A unit right after the number.
            let unit_start = i;
            while i < chars.len() && chars[i].is_ascii_alphabetic() {
                i += 1;
            }
            let unit: String = chars[unit_start..i].iter().collect();
            value *= match unit.as_str() {
                "" | "mm" => 1.0,
                "cm" => 10.0,
                "m" => 1000.0,
                "in" => 25.4,
                "ft" => 304.8,
                other => return Err(err(format!("unknown unit `{other}`"))),
            };
            out.push(Token::Number(value));
        } else if c.is_alphabetic() || c == '_' {
            let start = i;
            while i < chars.len() && (chars[i].is_alphanumeric() || chars[i] == '_') {
                i += 1;
            }
            out.push(Token::Name(chars[start..i].iter().collect()));
        } else if "+-*/(),".contains(c) {
            out.push(Token::Op(c));
            i += 1;
        } else {
            return Err(err(format!("unexpected `{c}`")));
        }
    }
    Ok(out)
}

/// Looks names up while evaluating.
pub trait Names {
    /// The value of `name`, or why there is none.
    fn value(&mut self, name: &str) -> Result<f64, FormulaError>;
}

struct Eval<'a, N: Names> {
    tokens: &'a [Token],
    at: usize,
    names: &'a mut N,
}

impl<N: Names> Eval<'_, N> {
    fn peek(&self) -> Option<&Token> {
        self.tokens.get(self.at)
    }

    fn sum(&mut self) -> Result<f64, FormulaError> {
        let mut v = self.product()?;
        while let Some(Token::Op(op @ ('+' | '-'))) = self.peek().cloned() {
            self.at += 1;
            let r = self.product()?;
            v = if op == '+' { v + r } else { v - r };
        }
        Ok(v)
    }

    fn product(&mut self) -> Result<f64, FormulaError> {
        let mut v = self.unary()?;
        while let Some(Token::Op(op @ ('*' | '/'))) = self.peek().cloned() {
            self.at += 1;
            let r = self.unary()?;
            if op == '/' && r == 0.0 {
                return Err(err("division by zero"));
            }
            v = if op == '*' { v * r } else { v / r };
        }
        Ok(v)
    }

    fn unary(&mut self) -> Result<f64, FormulaError> {
        if self.peek() == Some(&Token::Op('-')) {
            self.at += 1;
            return Ok(-self.unary()?);
        }
        if self.peek() == Some(&Token::Op('+')) {
            self.at += 1;
        }
        self.atom()
    }

    fn atom(&mut self) -> Result<f64, FormulaError> {
        match self.peek().cloned() {
            Some(Token::Number(n)) => {
                self.at += 1;
                Ok(n)
            }
            Some(Token::Op('(')) => {
                self.at += 1;
                let v = self.sum()?;
                self.close()?;
                Ok(v)
            }
            Some(Token::Name(name)) => {
                self.at += 1;
                if self.peek() == Some(&Token::Op('(')) {
                    self.at += 1;
                    let mut args = vec![self.sum()?];
                    while self.peek() == Some(&Token::Op(',')) {
                        self.at += 1;
                        args.push(self.sum()?);
                    }
                    self.close()?;
                    return call(&name, &args);
                }
                self.names.value(&name)
            }
            Some(t) => Err(err(format!("unexpected `{t:?}`"))),
            None => Err(err("the formula ends early")),
        }
    }

    fn close(&mut self) -> Result<(), FormulaError> {
        if self.peek() == Some(&Token::Op(')')) {
            self.at += 1;
            Ok(())
        } else {
            Err(err("a `(` is not closed"))
        }
    }
}

fn call(name: &str, args: &[f64]) -> Result<f64, FormulaError> {
    let one = || {
        if args.len() == 1 {
            Ok(args[0])
        } else {
            Err(err(format!("`{name}` takes one value")))
        }
    };
    Ok(match name {
        "min" => args.iter().copied().fold(f64::INFINITY, f64::min),
        "max" => args.iter().copied().fold(f64::NEG_INFINITY, f64::max),
        "round" => one()?.round(),
        "floor" => one()?.floor(),
        "ceil" => one()?.ceil(),
        "abs" => one()?.abs(),
        other => return Err(err(format!("unknown function `{other}`"))),
    })
}

/// Evaluates `formula` with `names` for its parameters.
pub fn evaluate(formula: &str, names: &mut dyn Names) -> Result<f64, FormulaError> {
    struct Dyn<'a>(&'a mut dyn Names);
    impl Names for Dyn<'_> {
        fn value(&mut self, name: &str) -> Result<f64, FormulaError> {
            self.0.value(name)
        }
    }
    let tokens = tokens(formula)?;
    let mut names = Dyn(names);
    let mut e = Eval {
        tokens: &tokens,
        at: 0,
        names: &mut names,
    };
    let v = e.sum()?;
    if e.at != tokens.len() {
        return Err(err(format!("unexpected text in `{formula}`")));
    }
    if v.is_finite() {
        Ok(v)
    } else {
        Err(err(format!("`{formula}` is not a finite number")))
    }
}

/// Parameters as formulas, evaluated on demand (each once), cycles found.
pub struct Parameters<'a> {
    formulas: &'a BTreeMap<String, String>,
    values: BTreeMap<String, f64>,
    busy: Vec<String>,
}

impl<'a> Parameters<'a> {
    /// Parameters `formulas` (name → formula).
    pub fn new(formulas: &'a BTreeMap<String, String>) -> Self {
        Parameters {
            formulas,
            values: BTreeMap::new(),
            busy: Vec::new(),
        }
    }

    /// Every parameter's value.
    pub fn all(&mut self) -> Result<BTreeMap<String, f64>, FormulaError> {
        for name in self.formulas.keys() {
            self.value(name)?;
        }
        Ok(self.values.clone())
    }
}

impl Names for Parameters<'_> {
    fn value(&mut self, name: &str) -> Result<f64, FormulaError> {
        if let Some(v) = self.values.get(name) {
            return Ok(*v);
        }
        let formula = self
            .formulas
            .get(name)
            .ok_or_else(|| err(format!("unknown parameter `{name}`")))?;
        if self.busy.iter().any(|b| b == name) {
            return Err(err(format!("`{name}` depends on itself")));
        }
        self.busy.push(name.to_string());
        let v = evaluate(formula, self);
        self.busy.pop();
        let v = v?;
        self.values.insert(name.to_string(), v);
        Ok(v)
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    fn params(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
            .collect()
    }

    #[test]
    fn arithmetic_units_functions_and_names() {
        let p = params(&[
            ("width", "60cm"),
            ("board", "18mm"),
            ("inner", "width - 2 * board"),
            ("shelves", "max(1, floor(inner / 200))"),
        ]);
        let mut values = Parameters::new(&p);
        let all = values.all().unwrap();
        assert_eq!(all["width"], 600.0);
        assert_eq!(all["inner"], 564.0);
        assert_eq!(all["shelves"], 2.0);
        assert_eq!(evaluate("-(1 + 2) * 3 / 2", &mut values).unwrap(), -4.5);
        assert_eq!(evaluate("0.5m + 1in", &mut values).unwrap(), 525.4);
    }

    #[test]
    fn errors_say_what_is_wrong() {
        let p = params(&[("a", "b + 1"), ("b", "a * 2")]);
        let mut values = Parameters::new(&p);
        assert!(values.all().unwrap_err().0.contains("depends on itself"));
        let empty = BTreeMap::new();
        let mut none = Parameters::new(&empty);
        for (formula, why) in [
            ("1 +", "ends early"),
            ("(1 + 2", "not closed"),
            ("3 / 0", "division by zero"),
            ("2furlongs", "unknown unit"),
            ("height", "unknown parameter"),
            ("sqrt(4)", "unknown function"),
            ("1 2", "unexpected text"),
        ] {
            let e = evaluate(formula, &mut none).unwrap_err();
            assert!(e.0.contains(why), "{formula}: {e}");
        }
    }
}
