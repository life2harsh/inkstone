pub fn render_mathml(source: &str) -> String {
    let source = source.trim();
    if source.is_empty() {
        return "<span class=\"math-empty\">No equation provided</span>".to_string();
    }

    let mut parser = Parser::new(source);
    let body = parser.parse_row(None);
    format!(
        "<math xmlns=\"http://www.w3.org/1998/Math/MathML\" display=\"block\" aria-label=\"{}\"><mrow>{}</mrow></math>",
        escape(source),
        body
    )
}

struct Parser {
    chars: Vec<char>,
    position: usize,
}

impl Parser {
    fn new(source: &str) -> Self {
        Self {
            chars: source.chars().collect(),
            position: 0,
        }
    }

    fn parse_row(&mut self, stop: Option<char>) -> String {
        let mut output = String::new();
        while let Some(ch) = self.peek() {
            if Some(ch) == stop {
                self.position += 1;
                break;
            }
            if ch.is_whitespace() {
                self.position += 1;
                continue;
            }

            let mut atom = self.parse_atom();
            let mut subscript = None;
            let mut superscript = None;
            loop {
                match self.peek() {
                    Some('_') => {
                        self.position += 1;
                        subscript = Some(self.parse_script());
                    }
                    Some('^') => {
                        self.position += 1;
                        superscript = Some(self.parse_script());
                    }
                    _ => break,
                }
            }
            atom = match (subscript, superscript) {
                (Some(sub), Some(sup)) => format!("<msubsup>{atom}{sub}{sup}</msubsup>"),
                (Some(sub), None) => format!("<msub>{atom}{sub}</msub>"),
                (None, Some(sup)) => format!("<msup>{atom}{sup}</msup>"),
                (None, None) => atom,
            };
            output.push_str(&atom);
        }
        output
    }

    fn parse_atom(&mut self) -> String {
        let Some(ch) = self.next() else {
            return String::new();
        };
        match ch {
            '{' => format!("<mrow>{}</mrow>", self.parse_row(Some('}'))),
            '\\' => self.parse_command(),
            '0'..='9' | '.' => {
                let mut value = String::from(ch);
                while matches!(self.peek(), Some(next) if next.is_ascii_digit() || next == '.') {
                    value.push(self.next().unwrap());
                }
                format!("<mn>{}</mn>", escape(&value))
            }
            'a'..='z' | 'A'..='Z' => format!("<mi>{ch}</mi>"),
            '(' | ')' | '[' | ']' | '=' | '+' | '-' | '*' | '/' | '|' | ',' | ':' | ';' | '<'
            | '>' => format!("<mo>{}</mo>", escape(&ch.to_string())),
            _ => format!("<mtext>{}</mtext>", escape(&ch.to_string())),
        }
    }

    fn parse_command(&mut self) -> String {
        let mut command = String::new();
        while matches!(self.peek(), Some(ch) if ch.is_ascii_alphabetic()) {
            command.push(self.next().unwrap());
        }
        match command.as_str() {
            "frac" => {
                let numerator = self.parse_script();
                let denominator = self.parse_script();
                format!("<mfrac>{numerator}{denominator}</mfrac>")
            }
            "sqrt" => format!("<msqrt>{}</msqrt>", self.parse_script()),
            "alpha" => "<mi>α</mi>".to_string(),
            "beta" => "<mi>β</mi>".to_string(),
            "gamma" => "<mi>γ</mi>".to_string(),
            "theta" => "<mi>θ</mi>".to_string(),
            "lambda" => "<mi>λ</mi>".to_string(),
            "mu" => "<mi>μ</mi>".to_string(),
            "pi" => "<mi>π</mi>".to_string(),
            "sigma" => "<mi>σ</mi>".to_string(),
            "sum" => "<mo>∑</mo>".to_string(),
            "prod" => "<mo>∏</mo>".to_string(),
            "int" => "<mo>∫</mo>".to_string(),
            "cdot" => "<mo>·</mo>".to_string(),
            "times" => "<mo>×</mo>".to_string(),
            "mid" => "<mo>|</mo>".to_string(),
            "le" => "<mo>≤</mo>".to_string(),
            "ge" => "<mo>≥</mo>".to_string(),
            "neq" => "<mo>≠</mo>".to_string(),
            _ => format!("<mtext>\\{}</mtext>", escape(&command)),
        }
    }

    fn parse_script(&mut self) -> String {
        while matches!(self.peek(), Some(ch) if ch.is_whitespace()) {
            self.position += 1;
        }
        if self.peek() == Some('{') {
            self.position += 1;
            format!("<mrow>{}</mrow>", self.parse_row(Some('}')))
        } else {
            self.parse_atom()
        }
    }

    fn peek(&self) -> Option<char> {
        self.chars.get(self.position).copied()
    }

    fn next(&mut self) -> Option<char> {
        let value = self.peek()?;
        self.position += 1;
        Some(value)
    }
}

fn escape(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renders_fraction_and_scripts_as_mathml() {
        let html = render_mathml(r"P(A|B) = \frac{P(B|A)P(A)}{P(B)} + x_1^2");
        assert!(html.contains("<math"));
        assert!(html.contains("<mfrac>"));
        assert!(html.contains("<msubsup>"));
    }

    #[test]
    fn escapes_unsupported_source() {
        let html = render_mathml(r"x < y & \unknown");
        assert!(html.contains("&lt;"));
        assert!(html.contains("&amp;"));
        assert!(html.contains("\\unknown"));
    }

    #[test]
    fn labels_empty_equations() {
        assert!(render_mathml("  ").contains("No equation provided"));
    }
}
