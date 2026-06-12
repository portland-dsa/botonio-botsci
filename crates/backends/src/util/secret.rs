//! Reading a secret from the systemd credentials directory, falling back to an
//! environment variable. In production each secret is delivered by
//! `LoadCredentialEncrypted=` into the unit's private `$CREDENTIALS_DIRECTORY`, never the
//! process environment; in local dev the same value comes from an env var.

use std::path::{Path, PathBuf};

/// The plaintext of credential `cred_name` from the credentials dir (preferred), else the
/// already-read `env_value`; `None` if neither. A trailing newline in the credential file
/// is trimmed (`systemd-creds encrypt` of an editor-authored file often carries one).
/// Pure: takes the dir + env value as arguments, so it unit-tests without touching the
/// process environment.
fn resolve(cred_dir: Option<&Path>, cred_name: &str, env_value: Option<String>) -> Option<String> {
    if let Some(dir) = cred_dir
        && let Ok(s) = std::fs::read_to_string(dir.join(cred_name))
    {
        return Some(s.trim_end_matches(['\n', '\r']).to_string());
    }
    env_value
}

/// Prefer `$CREDENTIALS_DIRECTORY/<cred_name>`, else the env var `env_var`.
pub fn from_credstore_or_env(cred_name: &str, env_var: &str) -> Option<String> {
    let dir = std::env::var_os("CREDENTIALS_DIRECTORY").map(PathBuf::from);
    resolve(dir.as_deref(), cred_name, std::env::var(env_var).ok())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    /// Creates a uniquely-named temp subdir, writes a credential file into it,
    /// and returns the dir path (caller is responsible for cleanup).
    fn make_cred_dir(test_id: &str, cred_name: &str, content: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("backends-secret-{test_id}"));
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join(cred_name), content).unwrap();
        dir
    }

    #[test]
    fn prefers_credentials_dir_over_env_value() {
        let dir = make_cred_dir("prefer-cred", "tok", "from-credstore\n");
        let result = resolve(Some(&dir), "tok", Some("from-env".to_string()));
        assert_eq!(result.as_deref(), Some("from-credstore"));
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn trims_trailing_newline_from_credential_file() {
        // Covers both \n and \r\n line endings.
        let dir = make_cred_dir("trim-newline", "tok", "mytoken\r\n");
        let result = resolve(Some(&dir), "tok", None);
        assert_eq!(result.as_deref(), Some("mytoken"));
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn falls_back_to_env_value_when_no_cred_dir() {
        let result = resolve(None, "tok_b", Some("from-env".to_string()));
        assert_eq!(result.as_deref(), Some("from-env"));
    }

    #[test]
    fn falls_back_to_env_value_when_cred_file_absent() {
        // Dir exists but the named credential file does not.
        let dir = std::env::temp_dir().join("backends-secret-missing-file");
        fs::create_dir_all(&dir).unwrap();
        let result = resolve(Some(&dir), "no_such_cred", Some("from-env".to_string()));
        assert_eq!(result.as_deref(), Some("from-env"));
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn returns_none_when_neither_is_present() {
        let result = resolve(None, "tok_c", None);
        assert!(result.is_none());
    }
}
