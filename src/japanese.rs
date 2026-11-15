use std::borrow::Cow;
use std::path::Path;
use std::sync::OnceLock;

use lindera::dictionary::{Dictionary, load_dictionary, load_user_dictionary_from_csv};
use lindera::mode::Mode;
use lindera::segmenter::Segmenter;
use lindera::token::Token;

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
            read_user_records(path, |_| Ok(()))?;
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
        for mut token in self
            .0
            .segment(Cow::Borrowed(text))
            .map_err(|_| Error::Index)?
        {
            if text[end..token.byte_start].contains(['\r', '\n']) {
                add_gap(&mut terms);
            }
            end = token.byte_end;
            match search_token(&mut token) {
                SearchToken::Term(term) => terms.push(term),
                SearchToken::Skip => (),
                SearchToken::Gap => add_gap(&mut terms),
            }
        }
        Ok(terms)
    }

    #[cfg(all(feature = "node", not(test)))]
    pub(crate) fn analyze(&self, text: &str) -> Result<Vec<AnalyzedToken>> {
        self.0
            .segment(Cow::Borrowed(text))
            .map_err(|_| Error::Index)
            .map(|tokens| {
                tokens
                    .into_iter()
                    .map(|mut token| {
                        let term = match search_token(&mut token) {
                            SearchToken::Term(term) => Some(term),
                            _ => None,
                        };
                        let reading = token
                            .details_iter()
                            .nth(7)
                            .filter(|&value| value != "*" && !value.is_empty())
                            .map(str::to_owned);
                        AnalyzedToken {
                            start: token.byte_start,
                            end: token.byte_end,
                            term,
                            reading,
                        }
                    })
                    .collect()
            })
    }
}

#[cfg(all(feature = "node", not(test)))]
#[derive(serde::Serialize)]
pub(crate) struct AnalyzedToken {
    start: usize,
    end: usize,
    term: Option<String>,
    reading: Option<String>,
}

enum SearchToken {
    Term(String),
    Skip,
    Gap,
}

fn search_token(token: &mut Token) -> SearchToken {
    // Keep unknown terms and user-defined forms, and normalize known inflections.
    let base = {
        let mut details = token.details_iter();
        match details.next() {
            Some("助詞" | "助動詞") => return SearchToken::Skip,
            Some("記号") => return SearchToken::Gap,
            _ => (),
        }
        details
            .nth(5)
            .filter(|word| {
                *word != "*" && !word.is_empty() && !word.chars().any(char::is_whitespace)
            })
            .map(str::to_lowercase)
    };
    if let Some(base) = base {
        SearchToken::Term(base)
    } else if token
        .surface
        .chars()
        .any(|ch| ch.is_alphanumeric() || ch == '_')
    {
        SearchToken::Term(token.surface.to_lowercase())
    } else {
        SearchToken::Gap
    }
}

fn read_user_records(
    path: &Path,
    mut visit: impl FnMut(csv::StringRecord) -> Result<()>,
) -> Result<()> {
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
        visit(record)?;
    }
    Ok(())
}

#[cfg(all(feature = "node", not(test)))]
pub(crate) fn merge_user_records(existing: Option<&Path>, selected: &Path) -> Result<String> {
    let mut seen = std::collections::HashSet::new();
    let mut writer = csv::Writer::from_writer(Vec::new());
    let mut append = |record: csv::StringRecord| {
        if seen.insert(record.iter().map(str::to_owned).collect::<Vec<_>>()) {
            writer.write_record(&record).map_err(|_| INVALID_USER_CSV)?;
        }
        Ok(())
    };
    if let Some(path) = existing {
        read_user_records(path, &mut append)?;
    }
    let mut has_selection = false;
    read_user_records(selected, |record| {
        has_selection = true;
        append(record)
    })?;
    if !has_selection {
        return Err(Error::Invalid("The selected dictionary CSV has no entries"));
    }
    let bytes = writer.into_inner().map_err(|_| INVALID_USER_CSV)?;
    String::from_utf8(bytes).map_err(|_| INVALID_USER_CSV)
}

fn add_gap(terms: &mut Vec<String>) {
    if terms.last().is_some_and(|term| term != GAP) {
        terms.push(GAP.into());
    }
}
