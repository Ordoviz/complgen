use std::string::FromUtf8Error;

#[derive(thiserror::Error, Debug)]
pub enum Error {
    #[error("Parsing error: {:?}", .0)]
    ParsingError(String),

    #[error("Empty grammar")]
    EmptyGrammar,

    #[error("One one command is allowed in completions definition")]
    VaryingCommandNames(Vec<String>),

    #[error("UTF-8 conversion error")]
    FromUtf8Error(#[from] FromUtf8Error),

    #[error("Formatting error")]
    FmtError(#[from] std::fmt::Error),
}
