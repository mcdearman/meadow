// Data parallelism: a 2000x2000 grid of independent float work, split between
// threads by taking every Nth row.

use std::thread;

const SIDE: i64 = 2000;
const LIMIT: i64 = 100;

fn band(start: i64, step: i64) -> i64 {
    let mut total = 0;
    let mut j = start;
    while j < SIDE {
        let cy = j as f64 / SIDE as f64 * 3.0 - 1.5;
        for i in 0..SIDE {
            let cx = i as f64 / SIDE as f64 * 3.0 - 2.0;
            let (mut x, mut y) = (0.0f64, 0.0f64);
            let mut n = 0;
            while n < LIMIT && x * x + y * y <= 4.0 {
                let nx = x * x - y * y + cx;
                y = 2.0 * x * y + cy;
                x = nx;
                n += 1;
            }
            total += n;
        }
        j += step;
    }
    total
}

fn main() {
    let workers = thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(4) as i64;
    let total: i64 = thread::scope(|s| {
        let handles: Vec<_> = (0..workers)
            .map(|w| s.spawn(move || band(w, workers)))
            .collect();
        handles.into_iter().map(|h| h.join().unwrap()).sum()
    });
    println!("{total}");
}
