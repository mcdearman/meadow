// Message passing: four producer threads each send fifty thousand numbers into
// one channel, and the main thread receives all of them and adds them up.

use std::sync::mpsc;
use std::thread;

const PRODUCERS: i64 = 4;
const EACH: i64 = 50_000;

fn main() {
    let (tx, rx) = mpsc::channel::<i64>();
    for p in 0..PRODUCERS {
        let tx = tx.clone();
        thread::spawn(move || {
            for i in 0..EACH {
                tx.send(p * EACH + i).unwrap();
            }
        });
    }
    drop(tx);
    let total: i64 = rx.iter().sum();
    println!("{total}");
}
