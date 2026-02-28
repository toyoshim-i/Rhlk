use thiserror::Error;

#[derive(Debug, Error)]
pub enum FormatError {
    #[error("unsupported object command: {0:#06x}")]
    UnsupportedCommand(u16),
    #[error("unexpected end of file while reading object stream")]
    UnexpectedEof,
    #[error("unterminated null-terminated string in object stream")]
    UnterminatedString,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Section {
    Abs,
    Text,
    Data,
    Bss,
    Stack,
    Xref,
    Common,
}

pub mod obj;
