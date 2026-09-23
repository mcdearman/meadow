// `wordfreq`, with an arena: the same counts, with nothing allocated per word.
//
// The corpus is read once into one block, and a word is a pair of numbers into
// it -- where it starts and how long it is -- so no word is ever copied. The
// table is three flat slices of a fixed size rather than a map[string]int, and
// probing writes numbers into them. What is left in the number is the hashing,
// the probing and the byte comparisons, with the allocator and the string type
// taken out.
//
// Only the ten reported at the end become strings.

package main

import (
	"bytes"
	"fmt"
	"os"
	"strings"
)

const cap_ = 1 << 14 // 16384 slots for a vocabulary of 5000

var (
	starts [cap_]int
	lens   [cap_]int
	counts [cap_]int64
	text   []byte
)

func hashOf(at, n int) uint64 {
	h := uint64(1469598103934665603)
	for i := 0; i < n; i++ {
		h ^= uint64(text[at+i])
		h *= 1099511628211
	}
	return h
}

func bump(at, n int) {
	i := int(hashOf(at, n)) & (cap_ - 1)
	for {
		if counts[i] == 0 {
			starts[i], lens[i], counts[i] = at, n, 1
			return
		}
		if lens[i] == n && bytes.Equal(text[starts[i]:starts[i]+n], text[at:at+n]) {
			counts[i]++
			return
		}
		i = (i + 1) & (cap_ - 1)
	}
}

// Is the word in slot a before the one in slot b, count first and then bytes?
func before(a, b int) bool {
	if counts[a] != counts[b] {
		return counts[a] > counts[b]
	}
	return bytes.Compare(text[starts[a]:starts[a]+lens[a]], text[starts[b]:starts[b]+lens[b]]) < 0
}

func main() {
	var err error
	text, err = os.ReadFile("work/corpus.txt")
	if err != nil {
		panic(err)
	}

	for i := 0; i < len(text); {
		for i < len(text) && (text[i] == ' ' || text[i] == '\n') {
			i++
		}
		start := i
		for i < len(text) && text[i] != ' ' && text[i] != '\n' {
			i++
		}
		if i > start {
			bump(start, i-start)
		}
	}

	// The ten commonest, kept in order as the table is walked.
	top := make([]int, 0, 10)
	for i := 0; i < cap_; i++ {
		if counts[i] == 0 {
			continue
		}
		at := len(top)
		for at > 0 && before(i, top[at-1]) {
			at--
		}
		if at >= 10 {
			continue
		}
		top = append(top, 0)
		copy(top[at+1:], top[at:])
		top[at] = i
		if len(top) > 10 {
			top = top[:10]
		}
	}
	parts := make([]string, len(top))
	for j, i := range top {
		parts[j] = fmt.Sprintf("%s:%d", text[starts[i]:starts[i]+lens[i]], counts[i])
	}
	fmt.Println(strings.Join(parts, " "))
}
