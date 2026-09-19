// Strings, a hash map and a sort: count the words of a 3MB file and report the
// ten commonest, count first and then the word.

import java.io.IOException;
import java.nio.file.Files;
import java.nio.file.Path;
import java.util.*;
import java.util.stream.Collectors;

public class Wordfreq {
    public static void main(String[] args) throws IOException {
        String text = Files.readString(Path.of("work/corpus.txt"));
        Map<String, Long> counts = new HashMap<>();
        for (String word : text.split("\\s+")) {
            if (!word.isEmpty()) counts.merge(word, 1L, Long::sum);
        }
        String top = counts.entrySet().stream()
                .sorted(Map.Entry.<String, Long>comparingByValue(Comparator.reverseOrder())
                        .thenComparing(Map.Entry.comparingByKey()))
                .limit(10)
                .map(e -> e.getKey() + ":" + e.getValue())
                .collect(Collectors.joining(" "));
        System.out.println(top);
    }
}
