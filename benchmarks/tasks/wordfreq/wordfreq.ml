(* Strings, a hash map and a sort: count the words of a 3MB file and report the
   ten commonest, count first and then the word. *)

let read path =
  let ch = open_in_bin path in
  let n = in_channel_length ch in
  let s = really_input_string ch n in
  close_in ch;
  s

let words text =
  let out = ref [] in
  let n = String.length text in
  let i = ref 0 in
  while !i < n do
    while !i < n && (text.[!i] = ' ' || text.[!i] = '\n') do incr i done;
    let start = !i in
    while !i < n && text.[!i] <> ' ' && text.[!i] <> '\n' do incr i done;
    if !i > start then out := String.sub text start (!i - start) :: !out
  done;
  !out

let () =
  let counts = Hashtbl.create 8192 in
  List.iter
    (fun w ->
      let was = try Hashtbl.find counts w with Not_found -> 0 in
      Hashtbl.replace counts w (was + 1))
    (words (read "work/corpus.txt"));
  let ranked = Hashtbl.fold (fun w c acc -> (w, c) :: acc) counts [] in
  let ranked =
    List.sort (fun (wa, ca) (wb, cb) -> if ca = cb then compare wa wb else compare cb ca) ranked
  in
  let rec take n = function [] -> [] | x :: xs -> if n = 0 then [] else x :: take (n - 1) xs in
  print_string (String.concat " " (List.map (fun (w, c) -> w ^ ":" ^ string_of_int c) (take 10 ranked)));
  print_newline ()
