// Threads contending for shared mutable state: eight of them moving money
// between sixteen accounts, every transfer reading two accounts and writing
// two as one indivisible step.
//
// One mutex over the whole set. Rust's alternative -- a lock per account --
// needs the two to be taken in a fixed order or the threads deadlock, which is
// exactly the bookkeeping a transaction does away with.

use std::sync::{Arc, Mutex};
use std::thread;

const ACCOUNTS: usize = 16;
const WORKERS: i64 = 8;
const MOVES: i64 = 20_000;

fn main() {
    let bank = Arc::new(Mutex::new(vec![1000i64; ACCOUNTS]));
    let mut handles = Vec::new();
    for w in 0..WORKERS {
        let bank = Arc::clone(&bank);
        handles.push(thread::spawn(move || {
            let mut s = w + 1;
            for _ in 0..MOVES {
                s = s * 48271 % 2147483647;
                let a = (s as usize) % ACCOUNTS;
                let b = (s as usize / ACCOUNTS) % ACCOUNTS;
                let amount = 1 + s % 10;
                if a != b {
                    let mut accounts = bank.lock().unwrap();
                    accounts[a] -= amount;
                    accounts[b] += amount;
                }
            }
        }));
    }
    for h in handles {
        h.join().unwrap();
    }
    let accounts = bank.lock().unwrap();
    let line: Vec<String> = accounts.iter().map(|b| b.to_string()).collect();
    println!("{}", line.join(","));
}
