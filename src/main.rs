mod intern;
mod lexer;
mod span;
mod token;

fn main() {
    let src = r#"
    fun map (f, xs) =
      case xs of
      | [] -> []
      | x::xs -> f x :: map (f, xs)
    "#;

    let mut lexer = lexer::Lexer::new(src);
    while let Some(token) = lexer.next() {
        println!("{:?}", token);
    }
}
