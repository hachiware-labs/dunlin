use std::borrow::Cow;
use std::path::Path;
use std::sync::OnceLock;

use lindera::dictionary::{Dictionary, load_dictionary, load_user_dictionary_from_csv};
use lindera::mode::Mode;
use lindera::segmenter::Segmenter;

use crate::{Error, Result};

pub(crate) const GAP: &str = "!";
const INVALID_USER_CSV: Error = Error::Invalid(
    "Expected a UTF-8 user dictionary CSV with surface, part of speech, and reading",
);

pub(crate) struct JapaneseTokenizer(Segmenter);

impl JapaneseTokenizer {
    pub(crate) fn open(path: Option<&Path>) -> Result<Self> {
        static IPADIC: OnceLock<Result<Dictionary>> = OnceLock::new();
        let dictionary = IPADIC
            .get_or_init(|| load_dictionary("embedded://ipadic").map_err(|_| Error::Index))
            .as_ref()
            .map_err(|_| Error::Index)?
            .clone();
        let user_dictionary = if let Some(path) = path {
            // The simple three-column format lets Lindera assign connection IDs.
            let mut reader = csv::ReaderBuilder::new()
                .has_headers(false)
                .from_path(path)
                .map_err(|_| INVALID_USER_CSV)?;
            for record in reader.records() {
                let record = record.map_err(|_| INVALID_USER_CSV)?;
                if record.len() != 3
                    || record.iter().any(|value| value.trim().is_empty())
                    || record[0].chars().any(char::is_whitespace)
                {
                    return Err(INVALID_USER_CSV);
                }
            }
            Some(
                load_user_dictionary_from_csv(&dictionary.metadata, path)
                    .map_err(|_| INVALID_USER_CSV)?,
            )
        } else {
            None
        };
        Ok(Self(Segmenter::new(
            Mode::Normal,
            dictionary,
            user_dictionary,
        )))
    }

    pub(crate) fn terms(&self, text: &str) -> Result<Vec<String>> {
        let mut terms = Vec::new();
        let mut end = 0;
        for token in self
            .0
            .segment(Cow::Borrowed(text))
            .map_err(|_| Error::Index)?
        {
            if text[end..token.byte_start].contains(['\r', '\n']) {
                add_gap(&mut terms);
            }
            end = token.byte_end;
            let mut token = token;
            // Keep unknown terms and user-defined forms, and normalize known inflections.
            let base = {
                let mut details = token.details_iter();
                match details.next() {
                    Some("助詞" | "助動詞") => continue,
                    Some("記号") => {
                        add_gap(&mut terms);
                        continue;
                    }
                    _ => {}
                }
                details
                    .nth(5)
                    .filter(|word| {
                        *word != "*" && !word.is_empty() && !word.chars().any(char::is_whitespace)
                    })
                    .map(str::to_lowercase)
            };
            if let Some(base) = base {
                terms.push(base);
            } else if token
                .surface
                .chars()
                .any(|ch| ch.is_alphanumeric() || ch == '_')
            {
                terms.push(token.surface.to_lowercase());
            } else {
                add_gap(&mut terms);
            }
        }
        Ok(terms)
    }
}

fn add_gap(terms: &mut Vec<String>) {
    if terms.last().is_some_and(|term| term != GAP) {
        terms.push(GAP.into());
    }
}
