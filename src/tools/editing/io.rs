use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::Path;

use uuid::Uuid;

pub fn atomic_replace(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let parent = path
        .parent()
        .filter(|value| !value.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));

    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("target");
    let temp_path = parent.join(format!(".{file_name}.tmp-{}", Uuid::new_v4()));

    let mut temp_file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temp_path)?;
    temp_file.write_all(bytes)?;
    temp_file.sync_all()?;

    if let Ok(metadata) = fs::metadata(path) {
        fs::set_permissions(&temp_path, metadata.permissions())?;
    }

    drop(temp_file);

    if let Err(err) = fs::rename(&temp_path, path) {
        let _ = fs::remove_file(&temp_path);
        return Err(err);
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::atomic_replace;
    use std::fs;
    use std::io::Write;

    #[cfg(unix)]
    #[test]
    fn atomic_replace_preserves_existing_mode_bits() {
        use std::os::unix::fs::PermissionsExt;

        let temp = tempfile::tempdir().unwrap();
        let target = temp.path().join("script.sh");
        let mut file = fs::File::create(&target).unwrap();
        file.write_all(b"echo old\n").unwrap();

        let original_mode = 0o755;
        fs::set_permissions(&target, fs::Permissions::from_mode(original_mode)).unwrap();

        atomic_replace(&target, b"echo new\n").unwrap();

        let updated_mode = fs::metadata(&target).unwrap().permissions().mode() & 0o777;
        assert_eq!(updated_mode, original_mode);
        assert_eq!(fs::read_to_string(&target).unwrap(), "echo new\n");
    }
}
