//! Files the viewer keeps across runs next to the onboarding id: the unanswered peers and the
//! app volume placements.

use std::ffi::OsString;
use std::io::ErrorKind;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};

/// Writes `text` into a temporary file beside `file` and renames it over `file`, so a run that
/// ends mid-write leaves the previous contents, never half of the new ones.
pub(crate) fn replace_file_contents(file: &Path, text: &str) -> std::io::Result<()> {
    let temporary = file.with_file_name(temporary_file_name(file));
    let written = std::fs::write(&temporary, text).and_then(|()| std::fs::rename(&temporary, file));
    if written.is_err()
        && let Err(cleanup) = std::fs::remove_file(&temporary)
        && cleanup.kind() != ErrorKind::NotFound
    {
        log::warn!("qaros: cannot remove {}: {cleanup}", temporary.display());
    }
    written
}

/// Unique per process and write, so viewers sharing a directory do not write into each
/// other's temporary file.
fn temporary_file_name(file: &Path) -> OsString {
    static WRITES: AtomicU64 = AtomicU64::new(0);
    let mut name = file.file_name().unwrap_or_default().to_owned();
    name.push(format!(
        ".{}.{}.tmp",
        std::process::id(),
        WRITES.fetch_add(1, Ordering::Relaxed)
    ));
    name
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn temporary_file_names_differ_per_write_and_carry_the_process_id() {
        let file = Path::new("directory/peers.txt");
        let first = temporary_file_name(file).into_string().unwrap();
        let second = temporary_file_name(file).into_string().unwrap();
        assert_ne!(first, second);
        let write_counter = first
            .strip_prefix(&format!("peers.txt.{}.", std::process::id()))
            .and_then(|rest| rest.strip_suffix(".tmp"))
            .unwrap_or_else(|| panic!("{first}"));
        assert!(write_counter.parse::<u64>().is_ok(), "{first}");
    }

    #[test]
    fn replaced_contents_land_whole_and_a_missing_directory_is_an_error() {
        let directory =
            std::env::temp_dir().join(format!("qaros-sink-state-files-{}", std::process::id()));
        std::fs::create_dir_all(&directory).unwrap();
        let file = directory.join("state.txt");
        replace_file_contents(&file, "first\n").unwrap();
        replace_file_contents(&file, "second\n").unwrap();
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "second\n");
        assert_eq!(std::fs::read_dir(&directory).unwrap().count(), 1);

        let unreachable = directory.join("missing-directory").join("state.txt");
        assert!(replace_file_contents(&unreachable, "text\n").is_err());

        let _ = std::fs::remove_dir_all(&directory);
    }
}
