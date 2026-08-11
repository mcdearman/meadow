use crate::{
    span::Span,
    token::{LToken, Token},
};
use logos::{Lexer as LogosLexer, Logos};

#[derive(Debug, Clone)]
pub struct Lexer<'src> {
    logos: LogosLexer<'src, Token>,
    peek: Option<LToken>,
}

impl<'src> Lexer<'src> {
    pub(crate) fn new(src: &'src str) -> Self {
        Self {
            logos: Token::lexer(src),
            peek: None,
        }
    }

    pub fn fetch_token(&mut self) -> LToken {
        match self.logos.next().map(|res| match res {
            Ok(t) => (t, Span::from(self.logos.span())),
            Err(_) => (Token::Error, Span::from(self.logos.span())),
        }) {
            Some((token, s)) => LToken::new(token, s),
            None => LToken::new(Token::Eof, Span::from(self.logos.span())),
        }
    }

    pub fn peek_tok(&mut self) -> LToken {
        if let Some(token) = self.peek.clone() {
            token
        } else {
            let token = self.fetch_token();
            self.peek = Some(token.clone());
            token
        }
    }

    pub fn next_tok(&mut self) -> LToken {
        if let Some(token) = self.peek.take() {
            self.peek = None;
            token
        } else {
            self.fetch_token()
        }
    }
}

impl<'src> Iterator for Lexer<'src> {
    type Item = (Token<'src>, Span);

    fn next(&mut self) -> Option<Self::Item> {
        let token = self.next_tok();
        if *token.value == Token::Eof {
            None
        } else {
            Some((*token.value, token.span))
        }
    }
}
