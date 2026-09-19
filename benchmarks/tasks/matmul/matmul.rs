// Three nested loops over flat arrays of doubles: the shape of numerical code,
// and of nothing else. `i k j` order, so the innermost loop walks in order.

fn main() {
    const N: usize = 256;
    let mut a = vec![0.0f64; N * N];
    let mut b = vec![0.0f64; N * N];
    let mut c = vec![0.0f64; N * N];
    for i in 0..N {
        for j in 0..N {
            a[i * N + j] = ((i + j) % 10) as f64;
            b[i * N + j] = ((i * j) % 10) as f64;
        }
    }
    for i in 0..N {
        for k in 0..N {
            let aik = a[i * N + k];
            for j in 0..N {
                c[i * N + j] += aik * b[k * N + j];
            }
        }
    }
    let total: f64 = (0..N).map(|i| c[i * N + i]).sum();
    println!("{}", total as i64);
}
