use std::path::Path;

/// Returns the square of `n`.
pub fn square<T>(n: T) -> T
where
    T: std::ops::Mul<Output = T> + Copy,
{
    n * n
}

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

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn square_integers() {
        assert_eq!(square(0), 0);
        assert_eq!(square(3), 9);
        assert_eq!(square(-4), 16);
        assert_eq!(square(10_i64), 100);
    }

    #[test]
    fn square_floats() {
        assert!((square(2.5_f64) - 6.25).abs() < f64::EPSILON);
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
