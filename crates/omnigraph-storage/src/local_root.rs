//! Local storage roots as filesystem paths and as `file://` addresses.
//!
//! A local root is written `file://<native absolute path>`, or as the bare
//! path. That spelling is persisted (deployment digests, receipts, the root a
//! server advertises), so this module keeps it byte for byte for every path
//! that already works and refuses, by name, the paths it cannot represent.
//!
//! Two things go wrong when callers build these strings themselves:
//!
//! - On Windows `std::fs::canonicalize` returns the verbatim form
//!   (`\\?\C:\data`). Inside a `file://` address the `?` starts a URL query,
//!   and a library that splits a location on `?` cuts the path in two.
//!   [`canonicalize_local_path`] returns the ordinary form (`C:\data`).
//! - On any platform a path containing `?`, `#` or a percent-escape does not
//!   survive `file://` + path followed by a URL parse: the parse drops or
//!   decodes part of it and the root silently names another directory.
//!   [`local_file_uri`] reads the address back and refuses a path that does
//!   not return unchanged.

use std::io;
use std::path::{Path, PathBuf};

use crate::{
    FILE_SCHEME_PREFIX, Result, StorageError, StorageFailureKind, local_path_from_file_uri,
};

/// `std::fs::canonicalize` for a path that will be written into a storage
/// root: symlinks are resolved, and on Windows the verbatim prefix is removed.
///
/// A canonical Windows path that only the verbatim form can spell (a volume
/// GUID or device namespace, a reserved device name, a component ending in a
/// dot or a space) is refused with [`io::ErrorKind::Unsupported`] rather than
/// rewritten into a path that names something else.
pub fn canonicalize_local_path(path: impl AsRef<Path>) -> io::Result<PathBuf> {
    let canonical = std::fs::canonicalize(path)?;
    #[cfg(windows)]
    {
        simplify_verbatim(&canonical)
    }
    #[cfg(not(windows))]
    {
        Ok(canonical)
    }
}

/// The `file://` address of an absolute local path, in the spelling local
/// roots have always used: the scheme followed by the native path.
///
/// The address is read back before it is returned. A path that does not
/// return unchanged is refused as unsupported, so no caller can persist a
/// root that later resolves to a different directory.
pub fn local_file_uri(path: &Path) -> Result<String> {
    let uri = format!("{FILE_SCHEME_PREFIX}{}", path.to_string_lossy());
    match local_path_from_file_uri(&uri) {
        Ok(read_back) if read_back == path => Ok(uri),
        _ => Err(StorageError::backend(
            StorageFailureKind::Configuration,
            format!(
                "unsupported local storage root '{}': it must be an absolute path on a local \
                 drive, without '?', '#' or percent-escapes, to be addressed as file://",
                path.display()
            ),
        )),
    }
}

/// The filesystem path of a local root given as a bare path or a `file://`
/// address. The inverse of [`local_file_uri`].
pub fn local_root_path(root: &str) -> Result<PathBuf> {
    if root.starts_with(FILE_SCHEME_PREFIX) {
        local_path_from_file_uri(root)
    } else {
        Ok(PathBuf::from(root))
    }
}

/// A canonical Windows path without its verbatim prefix.
#[cfg(windows)]
fn simplify_verbatim(path: &Path) -> io::Result<PathBuf> {
    use std::ffi::OsString;
    use std::path::{Component, Prefix};

    let mut components = path.components();
    let mut simplified = match components.next() {
        Some(Component::Prefix(prefix)) => match prefix.kind() {
            Prefix::VerbatimDisk(letter) => PathBuf::from(format!("{}:\\", char::from(letter))),
            Prefix::VerbatimUNC(server, share) => {
                let mut unc = OsString::from(r"\\");
                unc.push(server);
                unc.push(r"\");
                unc.push(share);
                unc.push(r"\");
                PathBuf::from(unc)
            }
            Prefix::Disk(_) | Prefix::UNC(..) => return Ok(path.to_path_buf()),
            Prefix::Verbatim(_) | Prefix::DeviceNS(_) => {
                return Err(unsupported_path(
                    path,
                    "it names a volume or device, not a drive letter or a share",
                ));
            }
        },
        _ => return Ok(path.to_path_buf()),
    };
    for component in components {
        match component {
            Component::RootDir => {}
            Component::Normal(name) if !needs_verbatim_prefix(&name.to_string_lossy()) => {
                simplified.push(name);
            }
            _ => {
                return Err(unsupported_path(
                    path,
                    "a component is a reserved device name or ends in a dot or a space",
                ));
            }
        }
    }
    Ok(simplified)
}

/// Whether Windows resolves `name` differently once the verbatim prefix is
/// gone: trailing dots and spaces are trimmed, and device names such as `NUL`
/// or `COM1` (with any extension) open the device.
#[cfg(any(windows, test))]
fn needs_verbatim_prefix(name: &str) -> bool {
    if name.is_empty() || name.ends_with('.') || name.ends_with(' ') {
        return true;
    }
    let stem = name.split('.').next().unwrap_or(name).trim_end_matches(' ');
    let stem = stem.to_ascii_uppercase();
    if matches!(
        stem.as_str(),
        "CON" | "PRN" | "AUX" | "NUL" | "CONIN$" | "CONOUT$"
    ) {
        return true;
    }
    let numbered = |prefix: &str| {
        stem.strip_prefix(prefix).is_some_and(|rest| {
            matches!(
                rest,
                "0" | "1" | "2" | "3" | "4" | "5" | "6" | "7" | "8" | "9" | "\u{b9}" | "\u{b2}"
                    | "\u{b3}"
            )
        })
    };
    numbered("COM") || numbered("LPT")
}

#[cfg(windows)]
fn unsupported_path(path: &Path, reason: &str) -> io::Error {
    io::Error::new(
        io::ErrorKind::Unsupported,
        format!(
            "unsupported local storage path '{}': {reason}",
            path.display()
        ),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reserved_and_trimmed_names_need_the_verbatim_prefix() {
        for name in [
            "NUL", "nul", "con.txt", "COM1", "lpt9.log", "aux ", "trailing.", "trailing ", ".",
            "..", "",
        ] {
            assert!(needs_verbatim_prefix(name), "{name:?}");
        }
        for name in [
            "data",
            "graph.omni",
            "__cluster",
            "console",
            "com10",
            "COM",
            "nul_backup",
            ".hidden",
            "a b",
        ] {
            assert!(!needs_verbatim_prefix(name), "{name:?}");
        }
    }

    #[test]
    fn canonical_directory_round_trips_through_its_file_uri() {
        let dir = tempfile::tempdir().unwrap();
        let nested = dir.path().join("og 13").join("data");
        std::fs::create_dir_all(&nested).unwrap();

        let canonical = canonicalize_local_path(&nested).unwrap();
        let uri = local_file_uri(&canonical).unwrap();

        assert_eq!(
            uri,
            format!("file://{}", canonical.to_string_lossy()),
            "the persisted spelling is the scheme followed by the native path"
        );
        assert_eq!(local_root_path(&uri).unwrap(), canonical);
        assert_eq!(
            local_root_path(&canonical.to_string_lossy()).unwrap(),
            canonical
        );
        assert!(
            !uri.contains('?'),
            "a canonical root must not carry a URL query: {uri}"
        );
    }

    #[test]
    fn relative_path_has_no_file_uri() {
        let error = local_file_uri(Path::new("relative/root")).unwrap_err();
        assert!(
            error.to_string().contains("unsupported local storage root"),
            "{error}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn unix_spelling_is_unchanged_for_paths_that_already_work() {
        for path in ["/srv/company-data", "/srv/og 13/data", "/tmp/a+b,c=d"] {
            assert_eq!(
                local_file_uri(Path::new(path)).unwrap(),
                format!("file://{path}")
            );
            assert_eq!(
                local_root_path(&format!("file://{path}")).unwrap(),
                Path::new(path)
            );
        }
    }

    #[cfg(unix)]
    #[test]
    fn unix_path_a_url_parse_would_alter_is_refused() {
        // Each of these used to produce a root that resolved to another
        // directory: `?` and `#` end the URL path, `%41` decodes to `A`.
        for path in ["/srv/og?13", "/srv/og#13", "/srv/og%41"] {
            let error = local_file_uri(Path::new(path)).unwrap_err();
            assert!(
                error.to_string().contains("unsupported local storage root"),
                "{path}: {error}"
            );
        }
    }

    #[cfg(windows)]
    #[test]
    fn windows_canonical_path_has_no_verbatim_prefix() {
        let dir = tempfile::tempdir().unwrap();

        let raw = std::fs::canonicalize(dir.path()).unwrap();
        assert!(
            raw.to_string_lossy().starts_with(r"\\?\"),
            "std canonicalize is expected to return the verbatim form: {}",
            raw.display()
        );

        let canonical = canonicalize_local_path(dir.path()).unwrap();
        let spelled = canonical.to_string_lossy();
        assert!(!spelled.starts_with(r"\\"), "{spelled}");
        assert!(!spelled.contains('?'), "{spelled}");
        assert_eq!(
            std::fs::canonicalize(&canonical).unwrap(),
            raw,
            "the simplified path names the same directory"
        );
    }

    #[cfg(windows)]
    #[test]
    fn windows_verbatim_forms_simplify_or_refuse() {
        assert_eq!(
            simplify_verbatim(Path::new(r"\\?\C:\Users\og\data")).unwrap(),
            Path::new(r"C:\Users\og\data")
        );
        assert_eq!(
            simplify_verbatim(Path::new(r"\\?\C:\")).unwrap(),
            Path::new(r"C:\")
        );
        assert_eq!(
            simplify_verbatim(Path::new(r"\\?\UNC\server\share\og")).unwrap(),
            Path::new(r"\\server\share\og")
        );
        assert_eq!(
            simplify_verbatim(Path::new(r"C:\already\plain")).unwrap(),
            Path::new(r"C:\already\plain")
        );
        for path in [
            r"\\?\Volume{b75e2c83-0000-0000-0000-602f00000000}\data",
            r"\\?\C:\data\NUL",
            r"\\?\C:\data\trailing.",
            r"\\?\C:\data\..\other",
        ] {
            let error = simplify_verbatim(Path::new(path)).unwrap_err();
            assert_eq!(error.kind(), io::ErrorKind::Unsupported, "{path}");
        }
    }

    #[cfg(windows)]
    #[test]
    fn windows_drive_root_round_trips_in_both_file_uri_spellings() {
        let path = Path::new(r"C:\Users\og 13\data");
        let uri = local_file_uri(path).unwrap();
        assert_eq!(uri, r"file://C:\Users\og 13\data");
        assert_eq!(local_root_path(&uri).unwrap(), path);
        // The URL spelling an operator types resolves to the same path.
        assert_eq!(
            local_root_path("file:///C:/Users/og%2013/data").unwrap(),
            path
        );
    }
}
