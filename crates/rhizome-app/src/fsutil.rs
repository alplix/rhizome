//! Small file helpers shared by the things the application saves.

use std::fs;
use std::path::{Path, PathBuf};

/// Writes a file so that a crash mid-write leaves the old contents, never a
/// half-written file: the data goes to a sibling temporary file first, which is
/// then renamed over the target.
pub(crate) fn write_atomic(path: &Path, contents: &str) -> Result<(), String> {
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir).map_err(|e| format!("could not create {}: {e}", dir.display()))?;
    }
    let mut temp = path.as_os_str().to_owned();
    temp.push(".tmp");
    let temp = PathBuf::from(temp);
    fs::write(&temp, contents).map_err(|e| format!("could not write {}: {e}", temp.display()))?;
    fs::rename(&temp, path).map_err(|e| {
        // Do not leave the temporary file behind if the swap failed.
        let _ = fs::remove_file(&temp);
        format!("could not replace {}: {e}", path.display())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dir(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("rhizome-fsutil-{}-{name}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        d
    }

    #[test]
    fn creates_missing_directories_and_writes() {
        let d = dir("create");
        let file = d.join("a").join("b.json");
        write_atomic(&file, "one").unwrap();
        assert_eq!(fs::read_to_string(&file).unwrap(), "one");
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn replaces_existing_contents_and_leaves_no_temporary_file() {
        let d = dir("replace");
        let file = d.join("x.json");
        write_atomic(&file, "old").unwrap();
        write_atomic(&file, "new").unwrap();
        assert_eq!(fs::read_to_string(&file).unwrap(), "new");
        let names: Vec<_> = fs::read_dir(&d)
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        assert_eq!(names.len(), 1);
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn a_failed_write_keeps_the_old_file_intact() {
        let d = dir("fail");
        fs::create_dir_all(&d).unwrap();
        let file = d.join("keep.json");
        write_atomic(&file, "precious").unwrap();
        // A directory where the temporary file should go makes the write fail.
        fs::create_dir_all(d.join("keep.json.tmp")).unwrap();
        assert!(write_atomic(&file, "clobber").is_err());
        assert_eq!(fs::read_to_string(&file).unwrap(), "precious");
        let _ = fs::remove_dir_all(&d);
    }
}
