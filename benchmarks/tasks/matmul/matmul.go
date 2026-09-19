// Three nested loops over flat slices of float64: the shape of numerical code,
// and of nothing else. `i k j` order, so the innermost loop walks in order.

package main

import "fmt"

const N = 256

func main() {
	a := make([]float64, N*N)
	b := make([]float64, N*N)
	c := make([]float64, N*N)
	for i := 0; i < N; i++ {
		for j := 0; j < N; j++ {
			a[i*N+j] = float64((i + j) % 10)
			b[i*N+j] = float64((i * j) % 10)
		}
	}
	for i := 0; i < N; i++ {
		for k := 0; k < N; k++ {
			aik := a[i*N+k]
			for j := 0; j < N; j++ {
				c[i*N+j] += aik * b[k*N+j]
			}
		}
	}
	total := 0.0
	for i := 0; i < N; i++ {
		total += c[i*N+i]
	}
	fmt.Println(int64(total))
}
