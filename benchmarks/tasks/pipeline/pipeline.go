// Message passing: four producer goroutines each send fifty thousand numbers
// into one channel, and main receives all of them and adds them up.

package main

import "fmt"

const producers = 4
const each = 50000

func main() {
	ch := make(chan int64, 1024)
	for p := int64(0); p < producers; p++ {
		go func(p int64) {
			for i := int64(0); i < each; i++ {
				ch <- p*each + i
			}
		}(p)
	}
	var total int64
	for n := 0; n < producers*each; n++ {
		total += <-ch
	}
	fmt.Println(total)
}
