// Threads contending for shared mutable state: eight goroutines moving money
// between sixteen accounts, every transfer reading two accounts and writing
// two as one indivisible step. One mutex over the whole set.

package main

import (
	"fmt"
	"strconv"
	"strings"
	"sync"
)

const accounts = 16
const workers = 8
const moves = 20000

func main() {
	bank := make([]int64, accounts)
	for i := range bank {
		bank[i] = 1000
	}
	var mu sync.Mutex
	var wg sync.WaitGroup
	for w := int64(0); w < workers; w++ {
		wg.Add(1)
		go func(w int64) {
			defer wg.Done()
			s := w + 1
			for i := 0; i < moves; i++ {
				s = s * 48271 % 2147483647
				a := s % accounts
				b := s / accounts % accounts
				amount := 1 + s%10
				if a == b {
					continue
				}
				mu.Lock()
				bank[a] -= amount
				bank[b] += amount
				mu.Unlock()
			}
		}(w)
	}
	wg.Wait()
	parts := make([]string, accounts)
	for i, b := range bank {
		parts[i] = strconv.FormatInt(b, 10)
	}
	fmt.Println(strings.Join(parts, ","))
}
