//! Small bounded JSON reader for the AI usage readers (credential files and provider
//! responses). Unlike the settings reader it keeps arrays. Input size, nesting depth and
//! element counts are capped; malformed input returns `None`, never panics.

const MAX_DEPTH: usize = 12;
const MAX_ITEMS: usize = 512;

#[derive(Clone, Debug, PartialEq)]
pub enum Value {
    /// `null`, `true` and `false` (the readers never need booleans).
    Null,
    Number(f64),
    Text(String),
    Array(Vec<Value>),
    Object(Vec<(String, Value)>),
}

impl Value {
    pub fn get(&self, key: &str) -> Option<&Value> {
        match self {
            Value::Object(members) => members.iter().find(|(k, _)| k == key).map(|(_, v)| v),
            _ => None,
        }
    }

    /// Walks nested object members; `None` as soon as one step is missing.
    pub fn path(&self, keys: &[&str]) -> Option<&Value> {
        let mut current = self;
        for key in keys {
            current = current.get(key)?;
        }
        Some(current)
    }

    pub fn as_f64(&self) -> Option<f64> {
        match self {
            Value::Number(n) if n.is_finite() => Some(*n),
            _ => None,
        }
    }

    pub fn as_str(&self) -> Option<&str> {
        match self {
            Value::Text(text) => Some(text),
            _ => None,
        }
    }

    pub fn as_array(&self) -> Option<&[Value]> {
        match self {
            Value::Array(items) => Some(items),
            _ => None,
        }
    }
}

/// Parses one JSON document of at most `max_bytes` bytes (a UTF-8 BOM is tolerated).
pub fn parse(bytes: &[u8], max_bytes: usize) -> Option<Value> {
    if bytes.len() > max_bytes {
        return None;
    }
    let bytes = bytes.strip_prefix(&[0xEF, 0xBB, 0xBF]).unwrap_or(bytes);
    let mut reader = Reader { bytes, pos: 0 };
    let value = reader.value(0)?;
    reader.skip_ws();
    (reader.pos == bytes.len()).then_some(value)
}

struct Reader<'a> {
    bytes: &'a [u8],
    pos: usize,
}

impl Reader<'_> {
    fn skip_ws(&mut self) {
        while matches!(self.bytes.get(self.pos), Some(b' ' | b'\t' | b'\n' | b'\r')) {
            self.pos += 1;
        }
    }

    fn peek(&self) -> Option<u8> {
        self.bytes.get(self.pos).copied()
    }

    fn eat(&mut self, byte: u8) -> Option<()> {
        (self.peek() == Some(byte)).then(|| self.pos += 1)
    }

    fn literal(&mut self, word: &[u8]) -> Option<()> {
        if self.bytes.get(self.pos..)?.starts_with(word) {
            self.pos += word.len();
            Some(())
        } else {
            None
        }
    }

    fn value(&mut self, depth: usize) -> Option<Value> {
        if depth > MAX_DEPTH {
            return None;
        }
        self.skip_ws();
        match self.peek()? {
            b'{' => self.object(depth),
            b'[' => self.array(depth),
            b'"' => self.string().map(Value::Text),
            b't' => self.literal(b"true").map(|_| Value::Null),
            b'f' => self.literal(b"false").map(|_| Value::Null),
            b'n' => self.literal(b"null").map(|_| Value::Null),
            b'-' | b'0'..=b'9' => self.number(),
            _ => None,
        }
    }

    fn object(&mut self, depth: usize) -> Option<Value> {
        self.eat(b'{')?;
        let mut members: Vec<(String, Value)> = Vec::new();
        self.skip_ws();
        if self.eat(b'}').is_some() {
            return Some(Value::Object(members));
        }
        loop {
            self.skip_ws();
            let key = self.string()?;
            self.skip_ws();
            self.eat(b':')?;
            let value = self.value(depth + 1)?;
            members.push((key, value));
            if members.len() > MAX_ITEMS {
                return None;
            }
            self.skip_ws();
            match self.peek()? {
                b',' => self.pos += 1,
                b'}' => {
                    self.pos += 1;
                    return Some(Value::Object(members));
                }
                _ => return None,
            }
        }
    }

    fn array(&mut self, depth: usize) -> Option<Value> {
        self.eat(b'[')?;
        let mut items = Vec::new();
        self.skip_ws();
        if self.eat(b']').is_some() {
            return Some(Value::Array(items));
        }
        loop {
            items.push(self.value(depth + 1)?);
            if items.len() > MAX_ITEMS {
                return None;
            }
            self.skip_ws();
            match self.peek()? {
                b',' => self.pos += 1,
                b']' => {
                    self.pos += 1;
                    return Some(Value::Array(items));
                }
                _ => return None,
            }
        }
    }

    fn number(&mut self) -> Option<Value> {
        let start = self.pos;
        while matches!(
            self.peek(),
            Some(b'-' | b'+' | b'.' | b'e' | b'E' | b'0'..=b'9')
        ) {
            self.pos += 1;
        }
        let text = std::str::from_utf8(&self.bytes[start..self.pos]).ok()?;
        text.parse::<f64>().ok().map(Value::Number)
    }

    fn hex4(&mut self) -> Option<u32> {
        let slice = self.bytes.get(self.pos..self.pos + 4)?;
        let mut value = 0u32;
        for byte in slice {
            value = value * 16 + (*byte as char).to_digit(16)?;
        }
        self.pos += 4;
        Some(value)
    }

    fn string(&mut self) -> Option<String> {
        self.eat(b'"')?;
        let mut out: Vec<u8> = Vec::new();
        loop {
            let byte = self.peek()?;
            self.pos += 1;
            match byte {
                b'"' => break,
                b'\\' => {
                    let escape = self.peek()?;
                    self.pos += 1;
                    let ch = match escape {
                        b'"' => '"',
                        b'\\' => '\\',
                        b'/' => '/',
                        b'b' => '\u{8}',
                        b'f' => '\u{c}',
                        b'n' => '\n',
                        b'r' => '\r',
                        b't' => '\t',
                        b'u' => {
                            let first = self.hex4()?;
                            let code = if (0xD800..0xDC00).contains(&first) {
                                self.eat(b'\\')?;
                                self.eat(b'u')?;
                                let second = self.hex4()?;
                                if !(0xDC00..0xE000).contains(&second) {
                                    return None;
                                }
                                0x10000 + ((first - 0xD800) << 10) + (second - 0xDC00)
                            } else {
                                first
                            };
                            char::from_u32(code)?
                        }
                        _ => return None,
                    };
                    let mut buf = [0u8; 4];
                    out.extend_from_slice(ch.encode_utf8(&mut buf).as_bytes());
                }
                0x00..=0x1f => return None,
                other => out.push(other),
            }
        }
        String::from_utf8(out).ok()
    }
}
