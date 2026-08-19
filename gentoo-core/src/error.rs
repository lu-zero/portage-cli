//! Gentoo-specific error types

use thiserror::Error;

/// Gentoo operation errors
#[derive(Error, Debug)]
pub enum Error {
    /// Architecture-related error
    #[error("Architecture error: {0}")]
    ArchError(String),

    /// Variant configuration error
    #[error("Variant configuration error: {0}")]
    VariantError(String),

    /// I/O operation failed
    #[error("I/O error: {0}")]
    IoError(#[from] std::io::Error),

    /// Parse error
    #[error("Parse error: {0}")]
    ParseError(String),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_error_display() {
        let arch_error = Error::ArchError("Invalid architecture".to_string());
        assert!(arch_error.to_string().contains("Architecture error"));

        let variant_error = Error::VariantError("Invalid variant".to_string());
        assert!(
            variant_error
                .to_string()
                .contains("Variant configuration error")
        );

        let io_error = Error::IoError(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            "File not found",
        ));
        assert!(io_error.to_string().contains("I/O error"));

        let parse_error = Error::ParseError("Failed to parse".to_string());
        assert!(parse_error.to_string().contains("Parse error"));
    }
}
