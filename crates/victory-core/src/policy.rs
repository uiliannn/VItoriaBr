//! Rule #1, enforced at runtime: game-derived data never lands in the repo.
//!
//! Everything VIctory produces from the user's files (converted assets,
//! prepared images, shader caches, crash dumps) is as much "from the game"
//! as the files themselves. Every tool that writes such output must call
//! [`ensure_outside_repo`] on the destination first. The static half of the
//! rule is `cargo xtask policy`, which checks what git tracks.

use std::path::{Component, Path, PathBuf};

/// Marker file at the root of the source tree.
pub const ROOT_MARKER: &str = ".victory-root";

/// The source tree this binary was compiled from.
fn compiled_workspace_root() -> PathBuf {
    // crates/victory-core -> workspace root
    Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join("..")
}

#[derive(Debug, thiserror::Error)]
pub enum PolicyError {
    #[error(
        "refusing to use {path} for game-derived data: it is inside the VItoriaBr source tree \
         ({root}). Keep game files, caches and outputs outside the repository \
         (see docs/asset-policy.md)"
    )]
    InsideRepo { path: PathBuf, root: PathBuf },
    #[error("cannot resolve {path}: {source}")]
    Io {
        path: PathBuf,
        source: std::io::Error,
    },
}

/// Returns `Ok` only if `path` is outside every VIctory source tree.
///
/// `path` does not have to exist yet. Two checks are made:
/// 1. it is not below the source tree this binary was built from;
/// 2. none of its ancestors contains the [`ROOT_MARKER`] file (catches
///    other clones/copies of the repository).
pub fn ensure_outside_repo(path: &Path) -> Result<(), PolicyError> {
    let resolved = resolve(path)?;

    if let Ok(root) = std::fs::canonicalize(compiled_workspace_root())
        && root.join(ROOT_MARKER).is_file()
        && resolved.starts_with(&root)
    {
        return Err(PolicyError::InsideRepo {
            path: path.to_path_buf(),
            root,
        });
    }

    for ancestor in resolved.ancestors() {
        if ancestor.join(ROOT_MARKER).is_file() {
            return Err(PolicyError::InsideRepo {
                path: path.to_path_buf(),
                root: ancestor.to_path_buf(),
            });
        }
    }
    Ok(())
}

/// True if `path` is `root` or lies below it, after resolving symlinks and
/// `..` (neither path has to exist yet). Compares whole components, so
/// `cache-old` is not inside `cache`.
pub fn is_within(path: &Path, root: &Path) -> Result<bool, PolicyError> {
    let resolved_path = resolve(path)?;
    let resolved_root = resolve(root)?;

    #[cfg(not(windows))]
    {
        Ok(resolved_path.starts_with(resolved_root))
    }

    #[cfg(windows)]
    {
        windows_case::starts_with_resolved(&resolved_path, &resolved_root)
    }
}

#[cfg(windows)]
mod windows_case {
    use std::ffi::{OsStr, c_void};
    use std::fs::{File, OpenOptions};
    use std::io;
    use std::os::windows::ffi::OsStrExt;
    use std::os::windows::fs::OpenOptionsExt;
    use std::os::windows::io::AsRawHandle;
    use std::path::{Component, Path, PathBuf};

    use super::PolicyError;

    const FILE_READ_ATTRIBUTES: u32 = 0x0000_0080;
    const FILE_SHARE_ALL: u32 = 0x0000_0001 | 0x0000_0002 | 0x0000_0004;
    const FILE_FLAG_BACKUP_SEMANTICS: u32 = 0x0200_0000;

    /// `FILE_INFO_BY_HANDLE_CLASS::FileCaseSensitiveInfo` (`minwinbase.h`, Windows 10 1903+).
    const FILE_CASE_SENSITIVE_INFO_CLASS: i32 = 23;
    const FILE_CS_FLAG_CASE_SENSITIVE_DIR: u32 = 0x0000_0001;
    const FILE_CASE_SENSITIVE_SEARCH: u32 = 0x0000_0001;
    const CSTR_EQUAL: i32 = 2;

    const ERROR_INVALID_FUNCTION: i32 = 1;
    const ERROR_FILE_NOT_FOUND: i32 = 2;
    const ERROR_PATH_NOT_FOUND: i32 = 3;
    const ERROR_NOT_SUPPORTED: i32 = 50;
    const ERROR_INVALID_PARAMETER: i32 = 87;

    #[repr(C)]
    struct FileCaseSensitiveInfo {
        flags: u32,
    }

    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn GetFileInformationByHandleEx(
            h_file: *mut c_void,
            file_information_class: i32,
            lp_file_information: *mut c_void,
            dw_buffer_size: u32,
        ) -> i32;

        fn GetVolumeInformationByHandleW(
            h_file: *mut c_void,
            lp_volume_name_buffer: *mut u16,
            n_volume_name_size: u32,
            lp_volume_serial_number: *mut u32,
            lp_maximum_component_length: *mut u32,
            lp_file_system_flags: *mut u32,
            lp_file_system_name_buffer: *mut u16,
            n_file_system_name_size: u32,
        ) -> i32;

        fn CompareStringOrdinal(
            lp_string1: *const u16,
            cch_count1: i32,
            lp_string2: *const u16,
            cch_count2: i32,
            b_ignore_case: i32,
        ) -> i32;
    }

    /// Compares two resolved paths component by component, accounting for
    /// case-insensitive Windows directories on nonexistent path components.
    ///
    /// Inference & limitations:
    /// - For nonexistent components, the nearest existing ancestor directory is
    ///   queried via `GetFileInformationByHandleEx(FileCaseSensitiveInfo)`. On
    ///   NTFS (Windows 10 1803+), newly created subdirectories inherit the
    ///   parent's `FILE_CS_FLAG_CASE_SENSITIVE_DIR` attribute, so the ancestor's
    ///   mode is the best non-mutating predictor without creating directories.
    ///   This cannot foresee later manual flag changes, mount/reparse boundaries
    ///   introduced before creation, or WSL 1 creation semantics.
    /// - When a directory is confirmed case-insensitive, components are compared
    ///   with `CompareStringOrdinal(..., TRUE)` (Windows OS binary UTF-16
    ///   uppercase table). This closely matches standard Win32/NTFS behavior,
    ///   but is not universally identical to every filesystem's rules (e.g.,
    ///   NTFS volumes formatted with a different `$UpCase` table, FAT/exFAT
    ///   quirks, or remote SMB/FUSE/APFS servers with server-side folding or
    ///   Unicode normalization).
    /// - If case sensitivity cannot be affirmatively determined by the OS, an
    ///   error is returned rather than assuming case-insensitivity.
    pub(super) fn starts_with_resolved(path: &Path, root: &Path) -> Result<bool, PolicyError> {
        let mut path_components = path.components();
        let mut prefix = PathBuf::new();

        for root_comp in root.components() {
            let Some(path_comp) = path_components.next() else {
                return Ok(false);
            };
            if root_comp == path_comp {
                prefix.push(root_comp);
                continue;
            }
            match (root_comp, path_comp) {
                (Component::Normal(r_name), Component::Normal(p_name)) => {
                    if !equal_ignore_case_ordinal(r_name, p_name, path)? {
                        return Ok(false);
                    }
                    if ancestor_is_case_sensitive(&prefix)? {
                        return Ok(false);
                    }
                    prefix.push(root_comp);
                }
                _ => return Ok(false),
            }
        }
        Ok(true)
    }

    fn equal_ignore_case_ordinal(
        lhs: &OsStr,
        rhs: &OsStr,
        diag_path: &Path,
    ) -> Result<bool, PolicyError> {
        let a: Vec<u16> = lhs.encode_wide().collect();
        let b: Vec<u16> = rhs.encode_wide().collect();
        if a.is_empty() || b.is_empty() {
            return Ok(a.is_empty() && b.is_empty());
        }
        let to_cch = |len: usize| {
            i32::try_from(len).map_err(|_| PolicyError::Io {
                path: diag_path.to_path_buf(),
                source: io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "path component exceeds Win32 i32 UTF-16 length limit",
                ),
            })
        };
        let a_len = to_cch(a.len())?;
        let b_len = to_cch(b.len())?;

        // SAFETY: `a` and `b` are non-empty UTF-16 buffers valid for `a_len` and `b_len`
        // `u16` elements for the duration of the call; `b_ignore_case = 1` (`TRUE`).
        let cmp = unsafe { CompareStringOrdinal(a.as_ptr(), a_len, b.as_ptr(), b_len, 1) };
        if cmp == 0 {
            return Err(PolicyError::Io {
                path: diag_path.to_path_buf(),
                source: io::Error::last_os_error(),
            });
        }
        Ok(cmp == CSTR_EQUAL)
    }

    fn ancestor_is_case_sensitive(dir: &Path) -> Result<bool, PolicyError> {
        let mut current = Some(dir);
        while let Some(candidate) = current {
            match open_existing_dir(candidate)? {
                Some(handle) => return query_handle_case_sensitive(&handle, candidate),
                None => current = candidate.parent(),
            }
        }
        Err(PolicyError::Io {
            path: dir.to_path_buf(),
            source: io::Error::new(
                io::ErrorKind::NotFound,
                "no existing ancestor directory found to query case sensitivity",
            ),
        })
    }

    /// Opens an existing directory with `FILE_READ_ATTRIBUTES` without creating
    /// any filesystem entry. Returns `Ok(None)` only when the path does not exist.
    fn open_existing_dir(dir: &Path) -> Result<Option<File>, PolicyError> {
        match OpenOptions::new()
            .access_mode(FILE_READ_ATTRIBUTES)
            .share_mode(FILE_SHARE_ALL)
            .custom_flags(FILE_FLAG_BACKUP_SEMANTICS)
            .open(dir)
        {
            Ok(file) => Ok(Some(file)),
            Err(err)
                if matches!(
                    err.raw_os_error(),
                    Some(ERROR_FILE_NOT_FOUND | ERROR_PATH_NOT_FOUND)
                ) =>
            {
                Ok(None)
            }
            Err(err) => Err(PolicyError::Io {
                path: dir.to_path_buf(),
                source: err,
            }),
        }
    }

    /// Queries per-directory case sensitivity via `GetFileInformationByHandleEx`
    /// (`FileCaseSensitiveInfo`). If that class is unsupported by the filesystem,
    /// checks volume flags via `GetVolumeInformationByHandleW` and only treats
    /// the directory as case-insensitive when `FILE_CASE_SENSITIVE_SEARCH` is
    /// affirmatively absent (e.g., FAT32/exFAT). Otherwise propagates the error.
    fn query_handle_case_sensitive(handle: &File, dir: &Path) -> Result<bool, PolicyError> {
        let raw_handle = handle.as_raw_handle();
        let mut info = FileCaseSensitiveInfo { flags: 0 };
        let size = u32::try_from(std::mem::size_of::<FileCaseSensitiveInfo>())
            .expect("FileCaseSensitiveInfo size fits in u32");

        // SAFETY: `raw_handle` is a valid open directory handle with `FILE_READ_ATTRIBUTES`;
        // `info` is an aligned `FileCaseSensitiveInfo` buffer of `size` bytes.
        let ok = unsafe {
            GetFileInformationByHandleEx(
                raw_handle,
                FILE_CASE_SENSITIVE_INFO_CLASS,
                (&raw mut info).cast::<c_void>(),
                size,
            )
        };
        if ok != 0 {
            return Ok((info.flags & FILE_CS_FLAG_CASE_SENSITIVE_DIR) != 0);
        }

        let query_err = io::Error::last_os_error();
        if matches!(
            query_err.raw_os_error(),
            Some(ERROR_INVALID_FUNCTION | ERROR_NOT_SUPPORTED | ERROR_INVALID_PARAMETER)
        ) {
            let mut fs_flags: u32 = 0;
            // SAFETY: `raw_handle` is a valid open handle with `FILE_READ_ATTRIBUTES`;
            // `fs_flags` is a valid aligned `u32` and all unused buffers are null with size 0.
            let vol_ok = unsafe {
                GetVolumeInformationByHandleW(
                    raw_handle,
                    std::ptr::null_mut(),
                    0,
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                    &raw mut fs_flags,
                    std::ptr::null_mut(),
                    0,
                )
            };
            if vol_ok != 0 && (fs_flags & FILE_CASE_SENSITIVE_SEARCH) == 0 {
                return Ok(false);
            }
        }

        Err(PolicyError::Io {
            path: dir.to_path_buf(),
            source: query_err,
        })
    }
}

/// Canonicalizes the longest existing prefix of `path` and re-appends the
/// rest, so that paths which do not exist yet can still be compared.
fn resolve(path: &Path) -> Result<PathBuf, PolicyError> {
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()
            .map_err(|source| PolicyError::Io {
                path: path.to_path_buf(),
                source,
            })?
            .join(path)
    };

    let mut existing = absolute.as_path();
    let mut rest = Vec::new();
    loop {
        match std::fs::canonicalize(existing) {
            Ok(canon) => {
                let mut out = canon;
                for component in rest.iter().rev() {
                    out.push(component);
                }
                return Ok(normalize(&out));
            }
            Err(source) => match (existing.parent(), existing.file_name()) {
                (Some(parent), Some(name)) => {
                    rest.push(name.to_os_string());
                    existing = parent;
                }
                _ => {
                    return Err(PolicyError::Io {
                        path: path.to_path_buf(),
                        source,
                    });
                }
            },
        }
    }
}

/// Removes `.` and resolves `..` lexically (only used on the non-existing tail).
fn normalize(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            c => out.push(c),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_paths_inside_this_repo() {
        let inside = compiled_workspace_root().join("cache").join("textures");
        assert!(matches!(
            ensure_outside_repo(&inside),
            Err(PolicyError::InsideRepo { .. })
        ));
        let sneaky = std::env::temp_dir().join("..").join("x"); // sanity: no panic
        let _ = ensure_outside_repo(&sneaky);
    }

    #[test]
    fn rejects_dotdot_escapes_back_into_repo() {
        let root = compiled_workspace_root();
        let path = root
            .join("crates")
            .join("..")
            .join("not-yet-created")
            .join("out");
        assert!(ensure_outside_repo(&path).is_err());
    }

    #[test]
    fn rejects_other_clones_via_marker() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join(ROOT_MARKER), "").unwrap();
        assert!(ensure_outside_repo(&tmp.path().join("cache")).is_err());
    }

    #[test]
    fn accepts_unrelated_directories() {
        let tmp = tempfile::tempdir().unwrap();
        ensure_outside_repo(&tmp.path().join("does").join("not").join("exist")).unwrap();
    }

    #[cfg(windows)]
    #[test]
    fn rejects_interior_nul_when_querying_ancestor_case_sensitivity() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir(tmp.path().join("foo")).unwrap();
        let parent_with_nul = tmp.path().join("foo\0bar");
        let root = parent_with_nul.join("cache");
        let path = parent_with_nul.join("CACHE");
        let err = is_within(&path, &root)
            .expect_err("interior NUL in ancestor path must not be silently truncated");
        assert!(
            matches!(err, PolicyError::Io { ref source, .. } if source.kind() == std::io::ErrorKind::InvalidInput),
            "expected InvalidInput Io error, got: {err:?}"
        );
    }

    #[cfg(windows)]
    #[test]
    fn detects_overlap_across_multiple_future_components() {
        let tmp = tempfile::tempdir().unwrap();
        let probe = tmp.path().join("case-probe");
        std::fs::create_dir(&probe).unwrap();
        assert!(tmp.path().join("CASE-PROBE").try_exists().unwrap());
        std::fs::remove_dir(probe).unwrap();

        let root = tmp.path().join("a").join("b");
        let path = tmp.path().join("A").join("B").join("saves");
        assert!(is_within(&path, &root).unwrap());
        assert!(std::fs::read_dir(tmp.path()).unwrap().next().is_none());
    }
}
