//! The one version string: the repository's `VERSION` file, read when the
//! program is built. Nothing else holds a version literal.

const RAW: &str = include_str!("../../../VERSION");

/// `YYYY.MM.PP`.
pub fn version() -> &'static str {
    RAW.trim()
}

/// The `{"product", "version", "px4_profile"}` answer of `GET /api/version`.
pub fn product() -> &'static str {
    "Corvus GCS"
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_version_is_calver() {
        let parts: Vec<&str> = version().split('.').collect();
        assert_eq!(parts.len(), 3, "{}", version());
        assert!(parts.iter().all(|p| p.chars().all(|c| c.is_ascii_digit())));
        assert_eq!(parts[0].len(), 4);
    }
}
