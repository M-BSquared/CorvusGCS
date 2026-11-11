//! The error text of Python's `json.loads`, for the one place an operator reads
//! it: a firmware container that is not JSON. [`check`] walks the document the
//! way CPython's scanner does and names the first thing wrong with it.

/// `None` when `text` is one JSON document by Python's rules (which accept
/// `NaN` and `Infinity`), else `Expecting value: line 1 column 2 (char 1)` and
/// its kin.
pub fn check(text: &str) -> Option<String> {
    let chars: Vec<char> = text.chars().collect();
    let mut s = Scan { c: &chars };
    let start = s.ws(0);
    let end = match s.value(start) {
        Ok(end) => end,
        Err((msg, at)) => return Some(s.message(msg, at)),
    };
    let end = s.ws(end);
    if end != chars.len() {
        return Some(s.message("Extra data", end));
    }
    None
}

type Fail = (&'static str, usize);

struct Scan<'a> {
    c: &'a [char],
}

impl Scan<'_> {
    fn message(&self, msg: &str, pos: usize) -> String {
        let before = &self.c[..pos.min(self.c.len())];
        let line = before.iter().filter(|c| **c == '\n').count() + 1;
        let col = match before.iter().rposition(|c| *c == '\n') {
            Some(i) => pos - i,
            None => pos + 1,
        };
        format!("{msg}: line {line} column {col} (char {pos})")
    }

    fn at(&self, i: usize) -> Option<char> {
        self.c.get(i).copied()
    }

    fn ws(&self, mut i: usize) -> usize {
        while matches!(self.at(i), Some(' ' | '\t' | '\n' | '\r')) {
            i += 1;
        }
        i
    }

    fn starts(&self, i: usize, word: &str) -> bool {
        word.chars().enumerate().all(|(k, w)| self.at(i + k) == Some(w))
    }

    fn value(&mut self, i: usize) -> Result<usize, Fail> {
        match self.at(i) {
            None => Err(("Expecting value", i)),
            Some('"') => self.string(i + 1),
            Some('{') => self.object(i + 1),
            Some('[') => self.array(i + 1),
            Some('n') if self.starts(i, "null") => Ok(i + 4),
            Some('t') if self.starts(i, "true") => Ok(i + 4),
            Some('f') if self.starts(i, "false") => Ok(i + 5),
            Some('N') if self.starts(i, "NaN") => Ok(i + 3),
            Some('I') if self.starts(i, "Infinity") => Ok(i + 8),
            Some('-') if self.starts(i, "-Infinity") => Ok(i + 9),
            Some(c) if c == '-' || c.is_ascii_digit() => self.number(i),
            Some(_) => Err(("Expecting value", i)),
        }
    }

    fn number(&self, start: usize) -> Result<usize, Fail> {
        let mut i = start;
        if self.at(i) == Some('-') {
            i += 1;
        }
        match self.at(i) {
            Some('0') => i += 1,
            Some(c) if c.is_ascii_digit() => {
                while self.at(i).is_some_and(|c| c.is_ascii_digit()) {
                    i += 1;
                }
            }
            _ => return Err(("Expecting value", start)),
        }
        if self.at(i) == Some('.') && self.at(i + 1).is_some_and(|c| c.is_ascii_digit()) {
            i += 1;
            while self.at(i).is_some_and(|c| c.is_ascii_digit()) {
                i += 1;
            }
        }
        if matches!(self.at(i), Some('e' | 'E')) {
            let mut j = i + 1;
            if matches!(self.at(j), Some('+' | '-')) {
                j += 1;
            }
            if self.at(j).is_some_and(|c| c.is_ascii_digit()) {
                while self.at(j).is_some_and(|c| c.is_ascii_digit()) {
                    j += 1;
                }
                i = j;
            }
        }
        Ok(i)
    }

    /// `i` is the first character after the opening quote.
    fn string(&self, mut i: usize) -> Result<usize, Fail> {
        let begin = i - 1;
        loop {
            match self.at(i) {
                None => return Err(("Unterminated string starting at", begin)),
                Some('"') => return Ok(i + 1),
                Some(c) if (c as u32) < 0x20 => return Err(("Invalid control character at", i)),
                Some('\\') => {
                    let Some(e) = self.at(i + 1) else {
                        return Err(("Unterminated string starting at", begin));
                    };
                    if e == 'u' {
                        let ok = (0..4).all(|k| self.at(i + 2 + k).is_some_and(|h| h.is_ascii_hexdigit()));
                        if !ok {
                            return Err(("Invalid \\uXXXX escape", i + 1));
                        }
                        i += 6;
                    } else if matches!(e, '"' | '\\' | '/' | 'b' | 'f' | 'n' | 'r' | 't') {
                        i += 2;
                    } else {
                        return Err(("Invalid \\escape", i));
                    }
                }
                Some(_) => i += 1,
            }
        }
    }

    fn object(&mut self, i: usize) -> Result<usize, Fail> {
        let mut i = self.ws(i);
        if self.at(i) == Some('}') {
            return Ok(i + 1);
        }
        loop {
            if self.at(i) != Some('"') {
                return Err(("Expecting property name enclosed in double quotes", i));
            }
            i = self.string(i + 1)?;
            i = self.ws(i);
            if self.at(i) != Some(':') {
                return Err(("Expecting ':' delimiter", i));
            }
            i = self.ws(i + 1);
            i = self.value(i)?;
            i = self.ws(i);
            match self.at(i) {
                Some('}') => return Ok(i + 1),
                Some(',') => i = self.ws(i + 1),
                _ => return Err(("Expecting ',' delimiter", i)),
            }
        }
    }

    fn array(&mut self, i: usize) -> Result<usize, Fail> {
        let mut i = self.ws(i);
        if self.at(i) == Some(']') {
            return Ok(i + 1);
        }
        loop {
            i = self.value(i)?;
            i = self.ws(i);
            match self.at(i) {
                Some(']') => return Ok(i + 1),
                Some(',') => i = self.ws(i + 1),
                _ => return Err(("Expecting ',' delimiter", i)),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::check;

    #[test]
    fn the_messages_are_pythons() {
        assert_eq!(
            check("{not json").unwrap(),
            "Expecting property name enclosed in double quotes: line 1 column 2 (char 1)"
        );
        assert_eq!(
            check("{\"a\" 1}").unwrap(),
            "Expecting ':' delimiter: line 1 column 6 (char 5)"
        );
        assert_eq!(
            check("{\"a\": 1 \"b\"}").unwrap(),
            "Expecting ',' delimiter: line 1 column 9 (char 8)"
        );
        assert_eq!(check("{\"a\": ").unwrap(), "Expecting value: line 1 column 7 (char 6)");
        assert_eq!(check("{\"a\": 1} x").unwrap(), "Extra data: line 1 column 10 (char 9)");
        assert_eq!(
            check("{\"a\": \"x").unwrap(),
            "Unterminated string starting at: line 1 column 7 (char 6)"
        );
        assert_eq!(
            check("{\"a\":\n[1,]}").unwrap(),
            "Expecting value: line 2 column 4 (char 9)"
        );
        assert_eq!(
            check("{\"a\": \"\\q\"}").unwrap(),
            "Invalid \\escape: line 1 column 8 (char 7)"
        );
        assert!(check("{\"a\": [1, 2.5e3, -0, NaN, \"\\u00e4\", null, true]}").is_none());
    }
}
