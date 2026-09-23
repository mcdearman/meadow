// `wordfreq`, with an arena: the same counts, with nothing allocated per word.
//
// The corpus is read once into one block, and a word is a pair of numbers into
// it -- where it starts and how long it is -- so no word is ever copied. The
// table is three flat arrays of a fixed size rather than a `HashMap<String, _>`,
// and probing writes numbers into them. What is left in the number is the
// hashing, the probing and the byte comparisons, with the allocator and the
// string type taken out.
//
// Only the ten reported at the end become strings.

const CAP: usize = 1 << 14; // 16384 slots for a vocabulary of 5000

struct Table {
    starts: Vec<usize>,
    lens: Vec<usize>,
    counts: Vec<i64>,
}

fn hash_of(text: &[u8], at: usize, n: usize) -> u64 {
    let mut h: u64 = 1469598103934665603;
    for i in 0..n {
        h ^= text[at + i] as u64;
        h = h.wrapping_mul(1099511628211);
    }
    h
}

fn bump(t: &mut Table, text: &[u8], at: usize, n: usize) {
    let mut i = (hash_of(text, at, n) as usize) & (CAP - 1);
    loop {
        if t.counts[i] == 0 {
            t.starts[i] = at;
            t.lens[i] = n;
            t.counts[i] = 1;
            return;
        }
        if t.lens[i] == n && text[t.starts[i]..t.starts[i] + n] == text[at..at + n] {
            t.counts[i] += 1;
            return;
        }
        i = (i + 1) & (CAP - 1);
    }
}

/// Is the word in slot `a` before the one in slot `b`, count first and then
/// the bytes?
fn before(t: &Table, text: &[u8], a: usize, b: usize) -> bool {
    if t.counts[a] != t.counts[b] {
        return t.counts[a] > t.counts[b];
    }
    text[t.starts[a]..t.starts[a] + t.lens[a]] < text[t.starts[b]..t.starts[b] + t.lens[b]]
}

fn main() {
    let text = std::fs::read("work/corpus.txt").unwrap();
    let mut t = Table {
        starts: vec![0; CAP],
        lens: vec![0; CAP],
        counts: vec![0; CAP],
    };

    let mut i = 0;
    while i < text.len() {
        while i < text.len() && (text[i] == b' ' || text[i] == b'\n') {
            i += 1;
        }
        let start = i;
        while i < text.len() && text[i] != b' ' && text[i] != b'\n' {
            i += 1;
        }
        if i > start {
            bump(&mut t, &text, start, i - start);
        }
    }

    // The ten commonest, kept in order as the table is walked.
    let mut top: Vec<usize> = Vec::with_capacity(10);
    for i in 0..CAP {
        if t.counts[i] == 0 {
            continue;
        }
        let mut at = top.len();
        while at > 0 && before(&t, &text, i, top[at - 1]) {
            at -= 1;
        }
        if at >= 10 {
            continue;
        }
        top.insert(at, i);
        top.truncate(10);
    }
    let line: Vec<String> = top
        .iter()
        .map(|&i| {
            format!(
                "{}:{}",
                std::str::from_utf8(&text[t.starts[i]..t.starts[i] + t.lens[i]]).unwrap(),
                t.counts[i]
            )
        })
        .collect();
    println!("{}", line.join(" "));
}
