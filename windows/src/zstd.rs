//! A small, bounded Zstandard frame decoder (RFC 8878). It exists for one job: reading the
//! few-kilobyte `zstd`-encoded usage responses Claude Desktop keeps in its HTTP cache. No
//! dictionaries, no streaming, no `unsafe`; the output is capped by the caller, every length
//! read from the input is checked before it is believed, and every failure is `None`. The
//! content checksum is skipped (the caller parses the result as JSON). The FSE and Huffman
//! tables follow the RFC; the predefined distributions and code tables are the reference
//! decoder's.

const MAGIC: [u8; 4] = [0x28, 0xB5, 0x2F, 0xFD];
const MAX_BLOCK: usize = 128 * 1024;
const MAX_HUFFMAN_BITS: u32 = 11;

const LL_BITS: [u8; 36] = [
    0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1, 1, 1, 1, 2, 2, 3, 3, 4, 6, 7, 8, 9, 10, 11,
    12, 13, 14, 15, 16,
];
const LL_BASE: [u32; 36] = [
    0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 18, 20, 22, 24, 28, 32, 40, 48, 64,
    0x80, 0x100, 0x200, 0x400, 0x800, 0x1000, 0x2000, 0x4000, 0x8000, 0x10000,
];
const ML_BITS: [u8; 53] = [
    0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
    1, 1, 1, 1, 2, 2, 3, 3, 4, 4, 5, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16,
];
const ML_BASE: [u32; 53] = [
    3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18, 19, 20, 21, 22, 23, 24, 25, 26, 27, 28,
    29, 30, 31, 32, 33, 34, 35, 37, 39, 41, 43, 47, 51, 59, 67, 83, 99, 0x83, 0x103, 0x203, 0x403,
    0x803, 0x1003, 0x2003, 0x4003, 0x8003, 0x10003,
];
const LL_NORM: [i16; 36] = [
    4, 3, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 1, 1, 1, 2, 2, 2, 2, 2, 2, 2, 2, 2, 3, 2, 1, 1, 1, 1, 1,
    -1, -1, -1, -1,
];
const ML_NORM: [i16; 53] = [
    1, 4, 3, 2, 2, 2, 2, 2, 2, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1,
    1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, -1, -1, -1, -1, -1, -1, -1,
];
const OF_NORM: [i16; 29] = [
    1, 1, 1, 1, 1, 1, 2, 2, 2, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, -1, -1, -1, -1, -1,
];

/// How one of the three sequence symbol kinds is coded.
struct Spec {
    default_log: u32,
    default_counts: &'static [i16],
    max_symbol: usize,
    max_log: u32,
}

const LL_SPEC: Spec = Spec {
    default_log: 6,
    default_counts: &LL_NORM,
    max_symbol: 35,
    max_log: 9,
};
const OF_SPEC: Spec = Spec {
    default_log: 5,
    default_counts: &OF_NORM,
    max_symbol: 31,
    max_log: 8,
};
const ML_SPEC: Spec = Spec {
    default_log: 6,
    default_counts: &ML_NORM,
    max_symbol: 52,
    max_log: 9,
};

/// Decodes one frame. `cap` bounds the decoded size; a larger frame is `None`.
pub fn decompress(input: &[u8], cap: usize) -> Option<Vec<u8>> {
    let mut decoder = Decoder {
        cap,
        out: Vec::new(),
        huffman: None,
        ll: None,
        of: None,
        ml: None,
        rep: [1, 4, 8],
    };
    decoder.frame(input)?;
    Some(decoder.out)
}

// ------------------------------------------------------------------ bit readers

/// Little-endian bit reader over a forward-read stream (the FSE table descriptions).
struct Forward<'a> {
    data: &'a [u8],
    /// Bit position of the next unread bit.
    pos: usize,
}

impl Forward<'_> {
    /// The next `count` bits without consuming them; bits past the end read as zero.
    fn peek(&self, count: u32) -> u32 {
        let mut value = 0u32;
        for i in 0..count {
            let bit = self.pos + i as usize;
            if let Some(byte) = self.data.get(bit >> 3) {
                value |= u32::from((*byte >> (bit & 7)) & 1) << i;
            }
        }
        value
    }

    fn skip(&mut self, count: u32) -> Option<()> {
        let end = self.pos + count as usize;
        if end > self.data.len() * 8 {
            return None;
        }
        self.pos = end;
        Some(())
    }

    fn read(&mut self, count: u32) -> Option<u32> {
        let value = self.peek(count);
        self.skip(count)?;
        Some(value)
    }
}

/// The backward bit stream of Huffman literals and FSE sequences: the last byte holds a
/// marker bit, and bits are consumed from just below it towards the start.
struct Backward<'a> {
    data: &'a [u8],
    /// Unread bits; negative once a read ran past the start of the stream.
    left: i64,
}

impl<'a> Backward<'a> {
    fn new(data: &'a [u8]) -> Option<Self> {
        let last = *data.last()?;
        if last == 0 {
            return None;
        }
        let marker = 7 - i64::from(last.leading_zeros());
        Some(Self {
            data,
            left: (data.len() as i64 - 1) * 8 + marker,
        })
    }

    fn bit(&self, index: i64) -> u64 {
        if index < 0 {
            return 0;
        }
        match self.data.get((index >> 3) as usize) {
            Some(byte) => u64::from((*byte >> (index & 7)) & 1),
            None => 0,
        }
    }

    /// The next `count` bits, most significant first, without consuming them; bits before
    /// the start of the stream read as zero.
    fn peek(&self, count: u32) -> u64 {
        let start = self.left - i64::from(count);
        let mut value = 0u64;
        for i in 0..i64::from(count) {
            value |= self.bit(start + i) << i;
        }
        value
    }

    fn read(&mut self, count: u32) -> u64 {
        let value = self.peek(count);
        self.left -= i64::from(count);
        value
    }

    fn overflowed(&self) -> bool {
        self.left < 0
    }
}

// ------------------------------------------------------------------ FSE

/// A finite-state-entropy decoding table: per state, the symbol, the bits to read for the
/// next state and that state's base.
#[derive(Clone)]
struct Fse {
    log: u32,
    entries: Vec<(u8, u8, u16)>,
}

/// Reads a normalized distribution: the accuracy log and the symbol counts (`-1` is a
/// "less than one" probability).
fn read_distribution(
    reader: &mut Forward,
    max_symbol: usize,
    max_log: u32,
) -> Option<(u32, Vec<i16>)> {
    let log = 5 + reader.read(4)?;
    if log > max_log {
        return None;
    }
    let mut remaining = (1i32 << log) + 1;
    let mut threshold = 1i32 << log;
    let mut bits = log + 1;
    let mut counts: Vec<i16> = Vec::new();
    let mut previous_zero = false;
    while remaining > 1 {
        if counts.len() > max_symbol {
            return None;
        }
        if previous_zero {
            let mut repeat = 0usize;
            loop {
                let flag = reader.read(2)?;
                repeat += flag as usize;
                if flag != 3 {
                    break;
                }
            }
            if counts.len() + repeat > max_symbol + 1 {
                return None;
            }
            counts.resize(counts.len() + repeat, 0);
            previous_zero = false;
            continue;
        }
        let max = (2 * threshold - 1) - remaining;
        let low = reader.peek(bits - 1) as i32;
        let value = if low < max {
            reader.skip(bits - 1)?;
            low
        } else {
            let mut wide = reader.peek(bits) as i32;
            if wide >= threshold {
                wide -= max;
            }
            reader.skip(bits)?;
            wide
        };
        let count = value - 1;
        remaining -= count.abs();
        counts.push(count as i16);
        previous_zero = count == 0;
        while remaining < threshold && bits > 1 && threshold > 1 {
            bits -= 1;
            threshold >>= 1;
        }
    }
    (remaining == 1).then_some((log, counts))
}

fn build_fse(log: u32, counts: &[i16]) -> Option<Fse> {
    let size = 1usize << log;
    let mut total = 0usize;
    for count in counts {
        if *count < -1 {
            return None;
        }
        total += usize::from(count.unsigned_abs());
    }
    if total != size {
        return None;
    }
    let mut spread = vec![0u8; size];
    let mut next = vec![0u32; counts.len()];
    let mut high = size as isize - 1;
    for (symbol, count) in counts.iter().enumerate() {
        if *count == -1 {
            spread[high as usize] = symbol as u8;
            high -= 1;
            next[symbol] = 1;
        } else {
            next[symbol] = *count as u32;
        }
    }
    let step = (size >> 1) + (size >> 3) + 3;
    let mask = size - 1;
    let mut pos = 0usize;
    for (symbol, count) in counts.iter().enumerate() {
        for _ in 0..(*count).max(0) {
            spread[pos] = symbol as u8;
            pos = (pos + step) & mask;
            while pos as isize > high {
                pos = (pos + step) & mask;
            }
        }
    }
    if pos != 0 {
        return None;
    }
    let mut entries = Vec::with_capacity(size);
    for symbol in spread {
        let state = next[usize::from(symbol)];
        next[usize::from(symbol)] += 1;
        let bits = log - (31 - state.leading_zeros());
        let base = (state << bits) - size as u32;
        entries.push((symbol, bits as u8, base as u16));
    }
    Some(Fse { log, entries })
}

// ------------------------------------------------------------------ Huffman literals

/// A Huffman decoding table indexed by the next `bits` bits: symbol and code length.
struct Huffman {
    bits: u32,
    table: Vec<(u8, u8)>,
}

/// The weights of a literals Huffman tree and how many input bytes its description took.
fn huffman_weights(data: &[u8]) -> Option<(Vec<u8>, usize)> {
    let header = usize::from(*data.first()?);
    if header >= 128 {
        let count = header - 127;
        let bytes = count.div_ceil(2);
        let packed = data.get(1..1 + bytes)?;
        let mut weights = Vec::with_capacity(count);
        for index in 0..count {
            let byte = packed[index / 2];
            weights.push(if index & 1 == 0 { byte >> 4 } else { byte & 15 });
        }
        return Some((weights, 1 + bytes));
    }
    if header == 0 {
        return None;
    }
    let compressed = data.get(1..1 + header)?;
    let mut forward = Forward {
        data: compressed,
        pos: 0,
    };
    let (log, counts) = read_distribution(&mut forward, 255, 6)?;
    let fse = build_fse(log, &counts)?;
    let mut stream = Backward::new(compressed.get(forward.pos.div_ceil(8)..)?)?;
    let mut first = stream.read(fse.log) as usize;
    let mut second = stream.read(fse.log) as usize;
    let mut weights: Vec<u8> = Vec::new();
    loop {
        if weights.len() > 255 {
            return None;
        }
        let (symbol, bits, base) = *fse.entries.get(first)?;
        weights.push(symbol);
        first = usize::from(base) + stream.read(u32::from(bits)) as usize;
        if stream.overflowed() {
            weights.push(fse.entries.get(second)?.0);
            break;
        }
        let (symbol, bits, base) = *fse.entries.get(second)?;
        weights.push(symbol);
        second = usize::from(base) + stream.read(u32::from(bits)) as usize;
        if stream.overflowed() {
            weights.push(fse.entries.get(first)?.0);
            break;
        }
    }
    Some((weights, 1 + header))
}

/// Builds the decoding table from the listed weights; the last symbol's weight is implied.
fn build_huffman(weights: &[u8]) -> Option<Huffman> {
    if weights.len() > 255 {
        return None;
    }
    let mut total = 0u32;
    for weight in weights {
        if u32::from(*weight) > MAX_HUFFMAN_BITS {
            return None;
        }
        if *weight > 0 {
            total += 1 << (*weight - 1);
        }
    }
    if total == 0 {
        return None;
    }
    let bits = 32 - total.leading_zeros();
    let rest = (1u32 << bits) - total;
    if bits > MAX_HUFFMAN_BITS || !rest.is_power_of_two() {
        return None;
    }
    let mut all = weights.to_vec();
    all.push(rest.trailing_zeros() as u8 + 1);
    let mut table = vec![(0u8, 0u8); 1usize << bits];
    let mut pos = 0usize;
    for weight in 1..=bits {
        for (symbol, listed) in all.iter().enumerate() {
            if u32::from(*listed) != weight {
                continue;
            }
            let span = 1usize << (weight - 1);
            table
                .get_mut(pos..pos + span)?
                .fill((symbol as u8, (bits - weight + 1) as u8));
            pos += span;
        }
    }
    (pos == table.len()).then_some(Huffman { bits, table })
}

/// Decodes exactly `count` literals from one backward stream.
fn huffman_stream(data: &[u8], tree: &Huffman, count: usize, out: &mut Vec<u8>) -> Option<()> {
    let mut stream = Backward::new(data)?;
    for _ in 0..count {
        let (symbol, bits) = *tree.table.get(stream.peek(tree.bits) as usize)?;
        out.push(symbol);
        stream.left -= i64::from(bits);
        if stream.overflowed() {
            return None;
        }
    }
    (stream.left == 0).then_some(())
}

// ------------------------------------------------------------------ the frame

struct Decoder {
    cap: usize,
    out: Vec<u8>,
    /// The last literals tree, kept for treeless blocks.
    huffman: Option<Huffman>,
    /// The last sequence tables, kept for "repeat" blocks.
    ll: Option<Fse>,
    of: Option<Fse>,
    ml: Option<Fse>,
    /// The three most recent match offsets.
    rep: [usize; 3],
}

impl Decoder {
    fn frame(&mut self, input: &[u8]) -> Option<()> {
        if input.get(..4)? != MAGIC {
            return None;
        }
        let descriptor = *input.get(4)?;
        let single_segment = descriptor & 0x20 != 0;
        let checksum = descriptor & 0x04 != 0;
        // A reserved bit, or a dictionary id (no dictionary is available).
        if descriptor & 0x08 != 0 || descriptor & 0x03 != 0 {
            return None;
        }
        let mut at = if single_segment { 5 } else { 6 };
        let size_bytes = match descriptor >> 6 {
            0 => usize::from(single_segment),
            1 => 2,
            2 => 4,
            _ => 8,
        };
        let mut declared = None;
        if size_bytes > 0 {
            let mut value = 0u64;
            for (index, byte) in input.get(at..at + size_bytes)?.iter().enumerate() {
                value |= u64::from(*byte) << (8 * index);
            }
            if descriptor >> 6 == 1 {
                value += 256;
            }
            if value > self.cap as u64 {
                return None;
            }
            declared = Some(value as usize);
            at += size_bytes;
        }
        loop {
            let header = input.get(at..at + 3)?;
            let word = usize::from(header[0])
                | (usize::from(header[1]) << 8)
                | (usize::from(header[2]) << 16);
            at += 3;
            let last = word & 1 != 0;
            let size = word >> 3;
            match (word >> 1) & 3 {
                0 => {
                    if size > MAX_BLOCK || self.out.len() + size > self.cap {
                        return None;
                    }
                    self.out.extend_from_slice(input.get(at..at + size)?);
                    at += size;
                }
                1 => {
                    if size > MAX_BLOCK || self.out.len() + size > self.cap {
                        return None;
                    }
                    let byte = *input.get(at)?;
                    self.out.resize(self.out.len() + size, byte);
                    at += 1;
                }
                2 => {
                    if size > MAX_BLOCK {
                        return None;
                    }
                    self.block(input.get(at..at + size)?)?;
                    at += size;
                }
                _ => return None,
            }
            if self.out.len() > self.cap {
                return None;
            }
            if last {
                break;
            }
        }
        if checksum && input.len() < at + 4 {
            return None;
        }
        match declared {
            Some(size) if size != self.out.len() => None,
            _ => Some(()),
        }
    }

    /// The literals section: the literals and how many block bytes it took.
    fn literals(&mut self, block: &[u8]) -> Option<(Vec<u8>, usize)> {
        let first = *block.first()?;
        let kind = first & 3;
        let format = (first >> 2) & 3;
        if kind < 2 {
            let (size, header) = match format {
                1 => {
                    let word = usize::from(first) | (usize::from(*block.get(1)?) << 8);
                    (word >> 4, 2)
                }
                3 => {
                    let word = usize::from(first)
                        | (usize::from(*block.get(1)?) << 8)
                        | (usize::from(*block.get(2)?) << 16);
                    (word >> 4, 3)
                }
                _ => (usize::from(first >> 3), 1),
            };
            if size > MAX_BLOCK {
                return None;
            }
            if kind == 0 {
                let raw = block.get(header..header + size)?;
                return Some((raw.to_vec(), header + size));
            }
            let byte = *block.get(header)?;
            return Some((vec![byte; size], header + 1));
        }
        let (header, streams) = match format {
            0 => (3, 1),
            1 => (3, 4),
            2 => (4, 4),
            _ => (5, 4),
        };
        let mut word = 0u64;
        for (index, byte) in block.get(..header)?.iter().enumerate() {
            word |= u64::from(*byte) << (8 * index);
        }
        word >>= 4;
        let width = match header {
            3 => 10,
            4 => 14,
            _ => 18,
        };
        let mask = (1u64 << width) - 1;
        let regenerated = (word & mask) as usize;
        let compressed = ((word >> width) & mask) as usize;
        if regenerated > MAX_BLOCK {
            return None;
        }
        let body = block.get(header..header + compressed)?;
        let mut streams_data = body;
        if kind == 2 {
            let (weights, used) = huffman_weights(body)?;
            self.huffman = Some(build_huffman(&weights)?);
            streams_data = body.get(used..)?;
        }
        let tree = self.huffman.as_ref()?;
        let mut literals = Vec::with_capacity(regenerated);
        if streams == 1 {
            huffman_stream(streams_data, tree, regenerated, &mut literals)?;
        } else {
            let jump = streams_data.get(..6)?;
            let length = |at: usize| usize::from(jump[at]) | (usize::from(jump[at + 1]) << 8);
            let (one, two, three) = (length(0), length(2), length(4));
            let data = streams_data.get(6..)?;
            let segment = regenerated.div_ceil(4);
            if regenerated < 3 * segment {
                return None;
            }
            let parts = [
                (data.get(..one)?, segment),
                (data.get(one..one + two)?, segment),
                (data.get(one + two..one + two + three)?, segment),
                (data.get(one + two + three..)?, regenerated - 3 * segment),
            ];
            for (part, count) in parts {
                huffman_stream(part, tree, count, &mut literals)?;
            }
        }
        Some((literals, header + compressed))
    }

    fn block(&mut self, block: &[u8]) -> Option<()> {
        let (literals, mut at) = self.literals(block)?;
        let mut count = usize::from(*block.get(at)?);
        at += 1;
        if count == 0 {
            self.out.extend_from_slice(&literals);
            return Some(());
        }
        if count >= 128 {
            if count < 255 {
                count = ((count - 128) << 8) + usize::from(*block.get(at)?);
                at += 1;
            } else {
                count = usize::from(*block.get(at)?) + (usize::from(*block.get(at + 1)?) << 8);
                count += 0x7F00;
                at += 2;
            }
        }
        let modes = *block.get(at)?;
        at += 1;
        if modes & 3 != 0 {
            return None;
        }
        let (table, used) =
            sequence_table(modes >> 6, self.ll.as_ref(), block.get(at..)?, &LL_SPEC)?;
        self.ll = Some(table);
        at += used;
        let (table, used) = sequence_table(
            (modes >> 4) & 3,
            self.of.as_ref(),
            block.get(at..)?,
            &OF_SPEC,
        )?;
        self.of = Some(table);
        at += used;
        let (table, used) = sequence_table(
            (modes >> 2) & 3,
            self.ml.as_ref(),
            block.get(at..)?,
            &ML_SPEC,
        )?;
        self.ml = Some(table);
        at += used;
        let ll_table = self.ll.as_ref()?;
        let of_table = self.of.as_ref()?;
        let ml_table = self.ml.as_ref()?;
        let mut stream = Backward::new(block.get(at..)?)?;
        let mut ll_state = stream.read(ll_table.log) as usize;
        let mut of_state = stream.read(of_table.log) as usize;
        let mut ml_state = stream.read(ml_table.log) as usize;
        let mut taken = 0usize;
        for number in 0..count {
            let of_entry = *of_table.entries.get(of_state)?;
            let ml_entry = *ml_table.entries.get(ml_state)?;
            let ll_entry = *ll_table.entries.get(ll_state)?;
            let of_code = usize::from(of_entry.0);
            let ml_code = usize::from(ml_entry.0);
            let ll_code = usize::from(ll_entry.0);
            if of_code > 31 || ml_code > 52 || ll_code > 35 {
                return None;
            }
            let offset_value = (1usize << of_code) + stream.read(of_code as u32) as usize;
            let match_length =
                ML_BASE[ml_code] as usize + stream.read(u32::from(ML_BITS[ml_code])) as usize;
            let literal_length =
                LL_BASE[ll_code] as usize + stream.read(u32::from(LL_BITS[ll_code])) as usize;
            if number + 1 < count {
                ll_state = usize::from(ll_entry.2) + stream.read(u32::from(ll_entry.1)) as usize;
                ml_state = usize::from(ml_entry.2) + stream.read(u32::from(ml_entry.1)) as usize;
                of_state = usize::from(of_entry.2) + stream.read(u32::from(of_entry.1)) as usize;
            }
            if stream.overflowed() {
                return None;
            }
            let previous = self.rep;
            let offset = if offset_value > 3 {
                let offset = offset_value - 3;
                self.rep = [offset, previous[0], previous[1]];
                offset
            } else {
                // Repeat offsets shift by one when the sequence has no literals.
                let slot = offset_value - 1 + usize::from(literal_length == 0);
                let offset = if slot == 3 {
                    previous[0].checked_sub(1)?
                } else {
                    previous[slot]
                };
                if offset == 0 {
                    return None;
                }
                match slot {
                    0 => {}
                    1 => self.rep = [offset, previous[0], previous[2]],
                    _ => self.rep = [offset, previous[0], previous[1]],
                }
                offset
            };
            self.out
                .extend_from_slice(literals.get(taken..taken + literal_length)?);
            taken += literal_length;
            if offset > self.out.len() || self.out.len() + match_length > self.cap {
                return None;
            }
            let start = self.out.len() - offset;
            for index in 0..match_length {
                let byte = self.out[start + index];
                self.out.push(byte);
            }
        }
        self.out.extend_from_slice(literals.get(taken..)?);
        Some(())
    }
}

/// One sequence symbol table by its mode (predefined, run-length, described, or repeated),
/// and how many input bytes it took.
fn sequence_table(
    mode: u8,
    current: Option<&Fse>,
    data: &[u8],
    spec: &Spec,
) -> Option<(Fse, usize)> {
    match mode {
        0 => Some((build_fse(spec.default_log, spec.default_counts)?, 0)),
        1 => {
            let symbol = *data.first()?;
            if usize::from(symbol) > spec.max_symbol {
                return None;
            }
            let entries = vec![(symbol, 0, 0)];
            Some((Fse { log: 0, entries }, 1))
        }
        2 => {
            let mut reader = Forward { data, pos: 0 };
            let (log, counts) = read_distribution(&mut reader, spec.max_symbol, spec.max_log)?;
            Some((build_fse(log, &counts)?, reader.pos.div_ceil(8)))
        }
        _ => Some((current?.clone(), 0)),
    }
}
