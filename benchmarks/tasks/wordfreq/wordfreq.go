// Strings, a hash map and a sort: count the words of a 3MB file and report the
// ten commonest, count first and then the word.

package main

import (
	"fmt"
	"os"
	"sort"
	"strings"
)

func main() {
	text, err := os.ReadFile("work/corpus.txt")
	if err != nil {
		panic(err)
	}
	counts := make(map[string]int64)
	for _, word := range strings.Fields(string(text)) {
		counts[word]++
	}
	type pair struct {
		word  string
		count int64
	}
	ranked := make([]pair, 0, len(counts))
	for w, c := range counts {
		ranked = append(ranked, pair{w, c})
	}
	sort.Slice(ranked, func(i, j int) bool {
		if ranked[i].count != ranked[j].count {
			return ranked[i].count > ranked[j].count
		}
		return ranked[i].word < ranked[j].word
	})
	parts := make([]string, 10)
	for i := 0; i < 10; i++ {
		parts[i] = fmt.Sprintf("%s:%d", ranked[i].word, ranked[i].count)
	}
	fmt.Println(strings.Join(parts, " "))
}
