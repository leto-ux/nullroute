use anyhow::{bail, Result};

/// Represents a parsed SPA-JSON value.
/// Note that `Object` uses `Vec<(String, SpaValue)>` rather than a map so that duplicate keys
/// inside a single `{ ... }` block (e.g. multiple `node.name = "!..."` in PipeWire match rules)
/// are preserved in order.
#[derive(Debug, Clone, PartialEq)]
pub enum SpaValue {
    String(String),
    Bool(bool),
    Number(f64),
    Array(Vec<SpaValue>),
    Object(Vec<(String, SpaValue)>),
}

impl SpaValue {
    pub fn as_object(&self) -> Option<&[(String, SpaValue)]> {
        match self {
            SpaValue::Object(entries) => Some(entries),
            _ => None,
        }
    }

    pub fn as_array(&self) -> Option<&[SpaValue]> {
        match self {
            SpaValue::Array(items) => Some(items),
            _ => None,
        }
    }

    pub fn as_str(&self) -> Option<&str> {
        match self {
            SpaValue::String(s) => Some(s.as_str()),
            _ => None,
        }
    }

    pub fn as_u32(&self) -> Option<u32> {
        match self {
            SpaValue::Number(n) if *n >= 0.0 => Some(*n as u32),
            SpaValue::String(s) => s.parse::<u32>().ok(),
            _ => None,
        }
    }

    pub fn get_field(&self, key: &str) -> Option<&SpaValue> {
        self.as_object()?
            .iter()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v)
    }

    pub fn get_all_fields<'a>(&'a self, key: &'a str) -> Vec<&'a SpaValue> {
        match self.as_object() {
            Some(entries) => entries
                .iter()
                .filter(|(k, _)| k == key)
                .map(|(_, v)| v)
                .collect(),
            None => Vec::new(),
        }
    }
}

/// Parses a PipeWire / WirePlumber `.conf` file in SPA-JSON format into a top-level `SpaValue::Object`.
pub fn parse_spa_conf(input: &str) -> Result<SpaValue> {
    let mut parser = Parser::new(input);
    parser.skip_ws_and_comments();
    if parser.is_eof() {
        return Ok(SpaValue::Object(Vec::new()));
    }
    if parser.peek_char() == Some('{') {
        parser.parse_object()
    } else {
        parser.parse_implicit_root_object()
    }
}

struct Parser<'a> {
    chars: Vec<char>,
    pos: usize,
    _raw: &'a str,
}

impl<'a> Parser<'a> {
    fn new(input: &'a str) -> Self {
        Self {
            chars: input.chars().collect(),
            pos: 0,
            _raw: input,
        }
    }

    fn is_eof(&self) -> bool {
        self.pos >= self.chars.len()
    }

    fn peek_char(&self) -> Option<char> {
        self.chars.get(self.pos).copied()
    }

    fn peek_next_char(&self) -> Option<char> {
        self.chars.get(self.pos + 1).copied()
    }

    fn next_char(&mut self) -> Option<char> {
        let ch = self.peek_char()?;
        self.pos += 1;
        Some(ch)
    }

    fn skip_ws_and_comments(&mut self) {
        while let Some(c) = self.peek_char() {
            if c.is_whitespace() || c == ',' || c == ';' {
                self.pos += 1;
            } else if c == '#' {
                // Line comment
                while let Some(nc) = self.next_char() {
                    if nc == '\n' {
                        break;
                    }
                }
            } else if c == '/' && self.peek_next_char() == Some('*') {
                // Block comment
                self.pos += 2;
                while !self.is_eof() {
                    if self.peek_char() == Some('*') && self.peek_next_char() == Some('/') {
                        self.pos += 2;
                        break;
                    }
                    self.pos += 1;
                }
            } else if c == '/' && self.peek_next_char() == Some('/') {
                while let Some(nc) = self.next_char() {
                    if nc == '\n' {
                        break;
                    }
                }
            } else {
                break;
            }
        }
    }

    fn parse_implicit_root_object(&mut self) -> Result<SpaValue> {
        let mut entries = Vec::new();
        loop {
            self.skip_ws_and_comments();
            if self.is_eof() {
                break;
            }
            let key = self.parse_string_or_bareword()?;
            self.skip_ws_and_comments();
            if let Some(sep) = self.peek_char() {
                if sep == '=' || sep == ':' {
                    self.pos += 1;
                }
            }
            self.skip_ws_and_comments();
            let val = self.parse_value()?;
            entries.push((key, val));
        }
        Ok(SpaValue::Object(entries))
    }

    fn parse_object(&mut self) -> Result<SpaValue> {
        let open = self.next_char();
        if open != Some('{') {
            bail!("Expected '{{' at position {}", self.pos);
        }
        let mut entries = Vec::new();
        loop {
            self.skip_ws_and_comments();
            match self.peek_char() {
                None => bail!("Unexpected EOF inside object '{{ ... }}'"),
                Some('}') => {
                    self.pos += 1;
                    break;
                }
                Some(_) => {
                    let key = self.parse_string_or_bareword()?;
                    self.skip_ws_and_comments();
                    if let Some(sep) = self.peek_char() {
                        if sep == '=' || sep == ':' {
                            self.pos += 1;
                        }
                    }
                    self.skip_ws_and_comments();
                    let val = self.parse_value()?;
                    entries.push((key, val));
                }
            }
        }
        Ok(SpaValue::Object(entries))
    }

    fn parse_array(&mut self) -> Result<SpaValue> {
        let open = self.next_char();
        if open != Some('[') {
            bail!("Expected '[' at position {}", self.pos);
        }
        let mut items = Vec::new();
        loop {
            self.skip_ws_and_comments();
            match self.peek_char() {
                None => bail!("Unexpected EOF inside array '[ ... ]'"),
                Some(']') => {
                    self.pos += 1;
                    break;
                }
                Some(_) => {
                    let val = self.parse_value()?;
                    items.push(val);
                }
            }
        }
        Ok(SpaValue::Array(items))
    }

    fn parse_value(&mut self) -> Result<SpaValue> {
        self.skip_ws_and_comments();
        match self.peek_char() {
            None => bail!("Unexpected EOF when expecting a value"),
            Some('{') => self.parse_object(),
            Some('[') => self.parse_array(),
            Some('"' | '\'') => {
                let s = self.parse_quoted_string()?;
                Ok(SpaValue::String(s))
            }
            Some(_) => {
                let word = self.parse_bareword()?;
                if word == "true" {
                    Ok(SpaValue::Bool(true))
                } else if word == "false" {
                    Ok(SpaValue::Bool(false))
                } else if let Ok(num) = word.parse::<f64>() {
                    Ok(SpaValue::Number(num))
                } else {
                    Ok(SpaValue::String(word))
                }
            }
        }
    }

    fn parse_string_or_bareword(&mut self) -> Result<String> {
        self.skip_ws_and_comments();
        match self.peek_char() {
            Some('"' | '\'') => self.parse_quoted_string(),
            Some(_) => self.parse_bareword(),
            None => bail!("Unexpected EOF when expecting key or string"),
        }
    }

    fn parse_quoted_string(&mut self) -> Result<String> {
        let quote = self.next_char().unwrap();
        let mut out = String::new();
        while let Some(c) = self.next_char() {
            if c == quote {
                return Ok(out);
            } else if c == '\\' {
                if let Some(esc) = self.next_char() {
                    match esc {
                        'n' => out.push('\n'),
                        'r' => out.push('\r'),
                        't' => out.push('\t'),
                        other => out.push(other),
                    }
                }
            } else {
                out.push(c);
            }
        }
        bail!("Unterminated quoted string starting with {}", quote)
    }

    fn parse_bareword(&mut self) -> Result<String> {
        let mut out = String::new();
        while let Some(c) = self.peek_char() {
            if c.is_whitespace()
                || matches!(c, '{' | '}' | '[' | ']' | '=' | ':' | ',' | ';' | '#' | '"' | '\'')
            {
                break;
            }
            out.push(c);
            self.pos += 1;
        }
        if out.is_empty() {
            bail!(
                "Unexpected character {:?} at position {}",
                self.peek_char(),
                self.pos
            );
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_sinks_conf() {
        let sample = r#"
        ## Courtesy of reddit
        context.objects = [
            { factory = adapter
                args = {
                    factory.name     = support.null-audio-sink
                    node.name        = "Master"
                    media.class      = "Audio/Duplex"
                    audio.position   = "FL,FR"
                    monitor.channel-volumes = true
                    object.id = 10000
                }
            }
        ]
        "#;
        let root = parse_spa_conf(sample).unwrap();
        let objs = root.get_field("context.objects").unwrap().as_array().unwrap();
        assert_eq!(objs.len(), 1);
        let args = objs[0].get_field("args").unwrap();
        assert_eq!(args.get_field("node.name").unwrap().as_str(), Some("Master"));
        assert_eq!(args.get_field("object.id").unwrap().as_u32(), Some(10000));
    }

    #[test]
    fn test_parse_duplicate_keys_in_matches() {
        let sample = r#"
        stream.rules = [
          {
            matches = [
              {
                application.process.binary = "electron"
                node.name = "!Chromium input"
                application.process.binary = "!dotcefnative"
              }
            ]
            actions = {
              update-props = {
                target.object = "Comms"
              }
            }
          }
        ]
        "#;
        let root = parse_spa_conf(sample).unwrap();
        let rules = root.get_field("stream.rules").unwrap().as_array().unwrap();
        let matches = rules[0].get_field("matches").unwrap().as_array().unwrap();
        let bins = matches[0].get_all_fields("application.process.binary");
        assert_eq!(bins.len(), 2);
        assert_eq!(bins[0].as_str(), Some("electron"));
        assert_eq!(bins[1].as_str(), Some("!dotcefnative"));
    }
}
