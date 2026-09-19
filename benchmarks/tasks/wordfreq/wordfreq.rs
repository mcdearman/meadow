// Strings, a hash map and a sort: count the words of a 3MB file and report the
// ten commonest, count first and then the word.

use std::collections::HashMap;
use std::fs;

fn main() {
    let text = fs::read_to_string("work/corpus.txt").unwrap();
    let mut counts: HashMap<&str, i64> = HashMap::new();
    for word in text.split_ascii_whitespace() {
        *counts.entry(word).or_insert(0) += 1;
    }
    let mut ranked: Vec<(&str, i64)> = counts.into_iter().collect();
    ranked.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(b.0)));
    let top: Vec<String> = ranked[..10]
        .iter()
        .map(|(w, c)| format!("{w}:{c}"))
        .collect();
    println!("{}", top.join(" "));
}
