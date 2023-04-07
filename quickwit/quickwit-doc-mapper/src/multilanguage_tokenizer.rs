// Copyright (C) 2023 Quickwit, Inc.
//
// Quickwit is offered under the AGPL v3.0 and as commercial software.
// For commercial licensing, contact us at hello@quickwit.io.
//
// AGPL:
// This program is free software: you can redistribute it and/or modify
// it under the terms of the GNU Affero General Public License as
// published by the Free Software Foundation, either version 3 of the
// License, or (at your option) any later version.
//
// This program is distributed in the hope that it will be useful,
// but WITHOUT ANY WARRANTY; without even the implied warranty of
// MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE. See the
// GNU Affero General Public License for more details.
//
// You should have received a copy of the GNU Affero General Public License
// along with this program. If not, see <http://www.gnu.org/licenses/>.

use lindera_tantivy::dictionary::load_dictionary;
use lindera_tantivy::stream::LinderaTokenStream;
use lindera_tantivy::tokenizer::LinderaTokenizer;
use lindera_tantivy::{DictionaryConfig, DictionaryKind, Mode};
use tantivy::tokenizer::{SimpleTokenStream, SimpleTokenizer, Token, TokenStream, Tokenizer};
use whichlang::detect_language;

#[derive(Clone)]
pub(crate) struct MultiLanguageTokenizer {
    cmn_tokenizer: LinderaTokenizer,
    jpn_tokenizer: LinderaTokenizer,
    default_tokenizer: SimpleTokenizer,
}

impl MultiLanguageTokenizer {
    pub fn new() -> Self {
        let cmn_dictionary_config = DictionaryConfig {
            kind: Some(DictionaryKind::CcCedict),
            path: None,
        };
        let cmn_dictionary = load_dictionary(cmn_dictionary_config)
            .expect("Lindera `CcCedict` dictionary must be present");
        let cmn_tokenizer = LinderaTokenizer::new(cmn_dictionary, None, Mode::Normal);

        let jpn_dictionary_config = DictionaryConfig {
            kind: Some(DictionaryKind::IPADIC),
            path: None,
        };
        let jpn_dictionary = load_dictionary(jpn_dictionary_config)
            .expect("Lindera `IPAD` dictionary must be present");
        let jpn_tokenizer = LinderaTokenizer::new(jpn_dictionary, None, Mode::Normal);

        Self {
            cmn_tokenizer,
            jpn_tokenizer,
            default_tokenizer: SimpleTokenizer,
        }
    }
}

pub(crate) enum MultiLanguageTokenStream<'a> {
    Lindera(LinderaTokenStream),
    Simple(SimpleTokenStream<'a>),
}

impl<'a> TokenStream for MultiLanguageTokenStream<'a> {
    fn advance(&mut self) -> bool {
        match self {
            MultiLanguageTokenStream::Lindera(tokenizer) => tokenizer.advance(),
            MultiLanguageTokenStream::Simple(tokenizer) => tokenizer.advance(),
        }
    }

    fn token(&self) -> &Token {
        match self {
            MultiLanguageTokenStream::Lindera(tokenizer) => tokenizer.token(),
            MultiLanguageTokenStream::Simple(tokenizer) => tokenizer.token(),
        }
    }

    fn token_mut(&mut self) -> &mut Token {
        match self {
            MultiLanguageTokenStream::Lindera(tokenizer) => tokenizer.token_mut(),
            MultiLanguageTokenStream::Simple(tokenizer) => tokenizer.token_mut(),
        }
    }
}

impl Tokenizer for MultiLanguageTokenizer {
    type TokenStream<'a> = MultiLanguageTokenStream<'a>;
    fn token_stream<'a>(&self, text: &'a str) -> MultiLanguageTokenStream<'a> {
        // TODO: let the user defined the language with a prefix like `jpn:こんにちは`
        let language = detect_language(text);
        match language {
            whichlang::Lang::Cmn => {
                MultiLanguageTokenStream::Lindera(self.cmn_tokenizer.token_stream(text))
            }
            whichlang::Lang::Jpn => {
                MultiLanguageTokenStream::Lindera(self.jpn_tokenizer.token_stream(text))
            }
            _ => MultiLanguageTokenStream::Simple(self.default_tokenizer.token_stream(text)),
        }
    }
}

#[cfg(test)]
mod tests {
    use tantivy::tokenizer::{Token, TokenStream, Tokenizer};

    use super::{MultiLanguageTokenStream, MultiLanguageTokenizer};

    fn test_helper(mut tokenizer: MultiLanguageTokenStream) -> Vec<Token> {
        let mut tokens: Vec<Token> = vec![];
        tokenizer.process(&mut |token: &Token| tokens.push(token.clone()));
        tokens
    }

    #[test]
    fn test_multilanguage_tokenizer_jpn() {
        let tokenizer = MultiLanguageTokenizer::new();
        let tokens = test_helper(tokenizer.token_stream("すもももももももものうち"));
        assert_eq!(tokens.len(), 7);
        {
            let token = &tokens[0];
            assert_eq!(token.text, "すもも");
            assert_eq!(token.offset_from, 0);
            assert_eq!(token.offset_to, 9);
            assert_eq!(token.position, 0);
            assert_eq!(token.position_length, 1);
        }
    }

    #[test]
    fn test_multilanguage_tokenizer_cmn() {
        let tokenizer = MultiLanguageTokenizer::new();
        let tokens = test_helper(
            tokenizer.token_stream("地址1，包含無效的字元 (包括符號與不標準的asci阿爾發字元"),
        );
        assert_eq!(tokens.len(), 19);
        {
            let token = &tokens[0];
            assert_eq!(token.text, "地址");
            assert_eq!(token.offset_from, 0);
            assert_eq!(token.offset_to, 6);
            assert_eq!(token.position, 0);
            assert_eq!(token.position_length, 1);
        }
    }
}