use std::path::Path;

/// Check whether a file at the given path has executable permissions.
pub fn is_executable(path: &Path) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::metadata(path)
            .map(|m| m.permissions().mode() & 0o111 != 0)
            .unwrap_or(false)
    }
    #[cfg(not(unix))]
    {
        path.is_file()
    }
}

/// Returns `true` if `n` is even.
pub fn is_even(n: i32) -> bool {
    n % 2 == 0
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn is_even_returns_true_for_even_numbers() {
        assert!(is_even(0));
        assert!(is_even(2));
        assert!(is_even(-4));
    }

    #[test]
    fn is_even_returns_false_for_odd_numbers() {
        assert!(!is_even(1));
        assert!(!is_even(-3));
        assert!(!is_even(99));
    }

    #[test]
    fn is_executable_returns_true_for_executable_file() {
        let dir = tempfile::tempdir().unwrap();
        let file_path = dir.path().join("test_exec");
        fs::write(&file_path, "#!/bin/sh\n").unwrap();
        #[cfg(unix)]
        fs::set_permissions(&file_path, fs::Permissions::from_mode(0o755)).unwrap();
        assert!(is_executable(&file_path));
    }

    #[test]
    fn is_executable_returns_false_for_non_executable_file() {
        let dir = tempfile::tempdir().unwrap();
        let file_path = dir.path().join("test_noexec");
        fs::write(&file_path, "hello").unwrap();
        #[cfg(unix)]
        fs::set_permissions(&file_path, fs::Permissions::from_mode(0o644)).unwrap();
        assert!(!is_executable(&file_path));
    }

    #[test]
    fn is_executable_returns_false_for_nonexistent_path() {
        let path = Path::new("/tmp/definitely_does_not_exist_xyz_123");
        assert!(!is_executable(path));
    }
}
