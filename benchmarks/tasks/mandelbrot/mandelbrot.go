// Data parallelism: a 2000x2000 grid of independent float work, split between
// goroutines by taking every Nth row.

package main

import (
	"fmt"
	"runtime"
	"sync"
)

const side = 2000
const limit = 100

func band(start, step int64) int64 {
	var total int64
	for j := start; j < side; j += step {
		cy := float64(j)/float64(side)*3.0 - 1.5
		for i := int64(0); i < side; i++ {
			cx := float64(i)/float64(side)*3.0 - 2.0
			x, y := 0.0, 0.0
			var n int64
			for n < limit && x*x+y*y <= 4.0 {
				x, y = x*x-y*y+cx, 2.0*x*y+cy
				n++
			}
			total += n
		}
	}
	return total
}

func main() {
	workers := int64(runtime.NumCPU())
	sums := make([]int64, workers)
	var wg sync.WaitGroup
	for w := int64(0); w < workers; w++ {
		wg.Add(1)
		go func(w int64) {
			defer wg.Done()
			sums[w] = band(w, workers)
		}(w)
	}
	wg.Wait()
	var total int64
	for _, s := range sums {
		total += s
	}
	fmt.Println(total)
}
