//! Replacement of staged directories while retaining the previous version until commit.

use std::path::Path;

/// A failed swap distinguishes a restored installation from a missing destination.
#[derive(Debug, thiserror::Error)]
#[error("{error}")]
pub struct DirectorySwapError {
    /// Filesystem failure, including the outcome and location of any preserved backup.
    #[source]
    pub error: std::io::Error,
    /// Whether the previous destination is still in place or was successfully restored.
    pub previous_in_place: bool,
}

/// Moves a fully staged directory into place, restoring the previous version if commit fails.
///
/// Stage files under `root` before calling so renames stay on the same filesystem. The caller
/// must serialize changes to the destination and validate the staged contents before this commit.
/// If restoring the old directory fails too, its backup is retained and reported in the error.
pub fn swap_into_place(
    staging: tempfile::TempDir,
    dest_dir: &Path,
    root: &Path,
) -> Result<(), DirectorySwapError> {
    if !dest_dir.exists() {
        std::fs::rename(staging.path(), dest_dir).map_err(|err| DirectorySwapError {
            error: std::io::Error::new(
                err.kind(),
                format!(
                    "Failed to move the staged directory into {}: {err}",
                    dest_dir.display()
                ),
            ),
            previous_in_place: false,
        })?;
        return Ok(());
    }

    // Scanners skip hidden directories. Drop deletes the previous version only after the new
    // directory is in place; a failed rollback instead keeps this guard's path for recovery.
    let parking = tempfile::Builder::new()
        .prefix(".kanon-replaced-")
        .tempdir_in(root)
        .map_err(|error| DirectorySwapError {
            error,
            previous_in_place: true,
        })?;
    let previous = parking.path().join("previous");
    std::fs::rename(dest_dir, &previous).map_err(|err| DirectorySwapError {
        error: std::io::Error::new(
            err.kind(),
            format!(
                "Failed to move the installed version out of {}: {err}",
                dest_dir.display()
            ),
        ),
        previous_in_place: true,
    })?;
    if let Err(err) = std::fs::rename(staging.path(), dest_dir) {
        let restored = std::fs::rename(&previous, dest_dir);
        return Err(DirectorySwapError {
            previous_in_place: restored.is_ok(),
            error: std::io::Error::new(
                err.kind(),
                format!(
                    "Failed to move the new version into {}: {err}{}",
                    dest_dir.display(),
                    match restored {
                        Ok(()) => "; the previous version was restored".to_string(),
                        Err(restore) => format!(
                            "; restoring the previous version also failed ({restore}), it is kept in {}",
                            parking.keep().join("previous").display()
                        ),
                    }
                ),
            ),
        });
    }
    Ok(())
}
