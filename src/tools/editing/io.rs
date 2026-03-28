use std::collections::HashSet;
use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use uuid::Uuid;

#[cfg(not(unix))]
compile_error!(
    "Kley only supports Unix write paths today; do not add Windows or other non-Unix compatibility work unless repo policy changes."
);

const MAX_SYMLINK_CHAIN_HOPS: usize = 64;

fn resolve_final_target(path: &Path) -> io::Result<PathBuf> {
    let mut current = path.to_path_buf();
    let mut seen = HashSet::new();
    let mut hops = 0;

    loop {
        match fs::symlink_metadata(&current) {
            Ok(metadata) if metadata.file_type().is_symlink() => {}
            Ok(_) => return Ok(current),
            Err(err) => {
                if err.kind() == io::ErrorKind::NotFound {
                    return Ok(current);
                }
                return Err(err);
            }
        }

        if !seen.insert(current.clone()) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("symlink loop detected for {}", current.display()),
            ));
        }

        if hops >= MAX_SYMLINK_CHAIN_HOPS {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "symlink chain exceeded maximum depth",
            ));
        }
        hops += 1;

        let link_target = fs::read_link(&current)?;
        let parent = current
            .parent()
            .filter(|value| !value.as_os_str().is_empty())
            .unwrap_or_else(|| Path::new("."));
        current = if link_target.is_absolute() {
            link_target
        } else {
            parent.join(link_target)
        };
    }
}

pub fn atomic_replace(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let target_path = resolve_final_target(path)?;

    let parent = target_path
        .parent()
        .filter(|value| !value.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));

    let file_name = target_path
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

    if let Ok(metadata) = fs::metadata(&target_path) {
        fs::set_permissions(&temp_path, metadata.permissions())?;
    }

    drop(temp_file);

    if let Err(err) = fs::rename(&temp_path, &target_path) {
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
    use std::os::unix::fs::symlink;

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

    #[cfg(unix)]
    #[test]
    fn atomic_replace_updates_symlink_target_without_replacing_link() {
        let temp = tempfile::tempdir().unwrap();
        let target = temp.path().join("target.txt");
        let mut file = fs::File::create(&target).unwrap();
        file.write_all(b"original").unwrap();

        let link_path = temp.path().join("link.txt");
        symlink(&target, &link_path).unwrap();

        atomic_replace(&link_path, b"updated").unwrap();

        let metadata = fs::symlink_metadata(&link_path).unwrap();
        assert!(metadata.file_type().is_symlink());
        assert_eq!(fs::read_to_string(&target).unwrap(), "updated");
        assert_eq!(fs::read_to_string(&link_path).unwrap(), "updated");
    }

    #[cfg(unix)]
    #[test]
    fn atomic_replace_updates_multi_hop_symlink_target() {
        let temp = tempfile::tempdir().unwrap();
        let target = temp.path().join("final.txt");
        let mut file = fs::File::create(&target).unwrap();
        file.write_all(b"original").unwrap();

        let mid_link = temp.path().join("mid.txt");
        symlink(&target, &mid_link).unwrap();

        let entry_point = temp.path().join("entry-point.txt");
        symlink(&mid_link, &entry_point).unwrap();

        atomic_replace(&entry_point, b"updated").unwrap();

        assert!(
            fs::symlink_metadata(&entry_point)
                .unwrap()
                .file_type()
                .is_symlink()
        );
        assert!(
            fs::symlink_metadata(&mid_link)
                .unwrap()
                .file_type()
                .is_symlink()
        );
        assert_eq!(fs::read_to_string(&target).unwrap(), "updated");
        assert_eq!(fs::read_to_string(&mid_link).unwrap(), "updated");
        assert_eq!(fs::read_to_string(&entry_point).unwrap(), "updated");
    }
}
