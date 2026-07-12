use std::{
    ffi::{CString, OsStr},
    fs, io,
    path::{Component, Path, PathBuf},
};

#[cfg(unix)]
use std::os::{
    fd::{AsRawFd, FromRawFd},
    unix::{
        ffi::OsStrExt,
        fs::{DirBuilderExt, MetadataExt, OpenOptionsExt},
    },
};

pub(crate) const PRIVATE_DIRECTORY_MODE: u32 = 0o700;
pub(crate) const PRIVATE_FILE_MODE: u32 = 0o600;

pub(crate) fn same_file_identity(left: &fs::Metadata, right: &fs::Metadata) -> bool {
    #[cfg(unix)]
    {
        left.dev() == right.dev() && left.ino() == right.ino()
    }
    #[cfg(not(unix))]
    {
        let _ = (left, right);
        false
    }
}

#[derive(Debug)]
pub(crate) struct PinnedPrivateDirectory {
    file: fs::File,
    path: PathBuf,
}

impl PinnedPrivateDirectory {
    pub(crate) fn open(path: &Path) -> io::Result<Self> {
        #[cfg(unix)]
        {
            validate_private_directory(path)?;
            let mut options = fs::OpenOptions::new();
            options
                .read(true)
                .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW);
            let file = options.open(path)?;
            let opened = file.metadata()?;
            let current = fs::symlink_metadata(path)?;
            validate_private_directory_mode(path, &opened)?;
            validate_private_directory_mode(path, &current)?;
            if opened.dev() != current.dev() || opened.ino() != current.ino() {
                return Err(invalid(format!(
                    "private run directory identity changed: {}",
                    path.display()
                )));
            }
            Ok(Self {
                file,
                path: path.to_path_buf(),
            })
        }
        #[cfg(not(unix))]
        {
            let _ = path;
            Err(unsupported())
        }
    }

    pub(crate) fn path(&self) -> &Path {
        &self.path
    }

    pub(crate) fn validate_identity(&self) -> io::Result<()> {
        #[cfg(unix)]
        {
            let opened = self.file.metadata()?;
            let current = fs::symlink_metadata(&self.path)?;
            validate_private_directory_mode(&self.path, &opened)?;
            validate_private_directory_mode(&self.path, &current)?;
            if opened.dev() != current.dev() || opened.ino() != current.ino() {
                return Err(invalid(format!(
                    "private run directory identity changed: {}",
                    self.path.display()
                )));
            }
            Ok(())
        }
        #[cfg(not(unix))]
        {
            Err(unsupported())
        }
    }

    pub(crate) fn open_existing_file(
        &self,
        name: &OsStr,
        read: bool,
        write: bool,
    ) -> io::Result<fs::File> {
        #[cfg(unix)]
        {
            let mut flags = libc::O_CLOEXEC | libc::O_NOFOLLOW;
            flags |= match (read, write) {
                (true, true) => libc::O_RDWR,
                (false, true) => libc::O_WRONLY,
                _ => libc::O_RDONLY,
            };
            let name = c_name(name)?;
            // SAFETY: dirfd and C string are valid; the returned descriptor is uniquely owned.
            let fd = unsafe { libc::openat(self.file.as_raw_fd(), name.as_ptr(), flags) };
            if fd < 0 {
                return Err(io::Error::last_os_error());
            }
            // SAFETY: successful openat returned a new owned descriptor.
            let file = unsafe { fs::File::from_raw_fd(fd) };
            let opened = file.metadata()?;
            self.validate_file_identity(name.as_c_str(), &opened)?;
            Ok(file)
        }
        #[cfg(not(unix))]
        {
            let _ = (name, read, write);
            Err(unsupported())
        }
    }

    pub(crate) fn open_append_file(&self, name: &OsStr) -> io::Result<fs::File> {
        #[cfg(unix)]
        {
            let name = c_name(name)?;
            let flags = libc::O_WRONLY | libc::O_APPEND | libc::O_CLOEXEC | libc::O_NOFOLLOW;
            // SAFETY: dirfd and C string are valid; returned descriptor is uniquely owned.
            let fd = unsafe { libc::openat(self.file.as_raw_fd(), name.as_ptr(), flags) };
            if fd < 0 {
                return Err(io::Error::last_os_error());
            }
            // SAFETY: successful openat returned a new owned descriptor.
            let file = unsafe { fs::File::from_raw_fd(fd) };
            self.validate_file_identity(name.as_c_str(), &file.metadata()?)?;
            Ok(file)
        }
        #[cfg(not(unix))]
        {
            let _ = name;
            Err(unsupported())
        }
    }

    pub(crate) fn open_child_directory(&self, name: &OsStr) -> io::Result<Self> {
        #[cfg(unix)]
        {
            let name = c_name(name)?;
            let flags = libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC;
            // SAFETY: dirfd and C string are valid; returned descriptor is uniquely owned.
            let fd = unsafe { libc::openat(self.file.as_raw_fd(), name.as_ptr(), flags) };
            if fd < 0 {
                return Err(io::Error::last_os_error());
            }
            // SAFETY: successful openat returned a new owned descriptor.
            let file = unsafe { fs::File::from_raw_fd(fd) };
            let path = self.path.join(OsStr::from_bytes(name.to_bytes()));
            let opened = file.metadata()?;
            validate_private_directory_mode(&path, &opened)?;
            let mut current: libc::stat = unsafe { std::mem::zeroed() };
            // SAFETY: stat is initialized on success; dirfd and name are valid.
            let result = unsafe {
                libc::fstatat(
                    self.file.as_raw_fd(),
                    name.as_ptr(),
                    &mut current,
                    libc::AT_SYMLINK_NOFOLLOW,
                )
            };
            if result != 0 {
                return Err(io::Error::last_os_error());
            }
            if current.st_mode & libc::S_IFMT != libc::S_IFDIR
                || (current.st_mode as u32) & 0o7777 != PRIVATE_DIRECTORY_MODE
                || opened.dev() != current.st_dev as u64
                || opened.ino() != current.st_ino as u64
            {
                return Err(invalid(format!(
                    "private run directory must be the same real 0700 directory: {}",
                    path.display()
                )));
            }
            Ok(Self { file, path })
        }
        #[cfg(not(unix))]
        {
            let _ = name;
            Err(unsupported())
        }
    }

    pub(crate) fn create_child_directory(&self, name: &OsStr) -> io::Result<Self> {
        #[cfg(unix)]
        {
            let name = c_name(name)?;
            // SAFETY: dirfd and C string are valid and mode is exact.
            let result = unsafe {
                libc::mkdirat(
                    self.file.as_raw_fd(),
                    name.as_ptr(),
                    PRIVATE_DIRECTORY_MODE as libc::mode_t,
                )
            };
            if result != 0 {
                return Err(io::Error::last_os_error());
            }
            self.open_child_directory(OsStr::from_bytes(name.to_bytes()))
        }
        #[cfg(not(unix))]
        {
            let _ = name;
            Err(unsupported())
        }
    }

    pub(crate) fn create_file(&self, name: &OsStr) -> io::Result<fs::File> {
        #[cfg(unix)]
        {
            let name = c_name(name)?;
            let flags =
                libc::O_RDWR | libc::O_CREAT | libc::O_EXCL | libc::O_CLOEXEC | libc::O_NOFOLLOW;
            // SAFETY: dirfd and C string are valid; mode is exact and the descriptor is owned.
            let fd = unsafe {
                libc::openat(
                    self.file.as_raw_fd(),
                    name.as_ptr(),
                    flags,
                    PRIVATE_FILE_MODE as libc::c_uint,
                )
            };
            if fd < 0 {
                return Err(io::Error::last_os_error());
            }
            // SAFETY: successful openat returned a new owned descriptor.
            let file = unsafe { fs::File::from_raw_fd(fd) };
            let opened = file.metadata()?;
            validate_private_file_mode(
                &self.path.join(OsStr::from_bytes(name.to_bytes())),
                &opened,
            )?;
            Ok(file)
        }
        #[cfg(not(unix))]
        {
            let _ = name;
            Err(unsupported())
        }
    }

    pub(crate) fn validate_file(&self, name: &OsStr, opened: &fs::Metadata) -> io::Result<()> {
        #[cfg(unix)]
        {
            let name = c_name(name)?;
            self.validate_file_identity(name.as_c_str(), opened)
        }
        #[cfg(not(unix))]
        {
            let _ = (name, opened);
            Err(unsupported())
        }
    }

    pub(crate) fn validate_single_link_file(
        &self,
        name: &OsStr,
        opened: &fs::Metadata,
    ) -> io::Result<()> {
        self.validate_file(name, opened)?;
        #[cfg(unix)]
        if opened.nlink() != 1 {
            return Err(invalid(format!(
                "private run artifact must not be hard-linked: {}",
                self.path.join(name).display()
            )));
        }
        Ok(())
    }

    #[cfg(unix)]
    fn validate_file_identity(
        &self,
        name: &std::ffi::CStr,
        opened: &fs::Metadata,
    ) -> io::Result<()> {
        validate_private_file_mode(&self.path.join(OsStr::from_bytes(name.to_bytes())), opened)?;
        // SAFETY: stat is initialized by fstatat on success; dirfd and name are valid.
        let mut current: libc::stat = unsafe { std::mem::zeroed() };
        let result = unsafe {
            libc::fstatat(
                self.file.as_raw_fd(),
                name.as_ptr(),
                &mut current,
                libc::AT_SYMLINK_NOFOLLOW,
            )
        };
        if result != 0 {
            return Err(io::Error::last_os_error());
        }
        if current.st_mode & libc::S_IFMT != libc::S_IFREG
            || (current.st_mode as u32) & 0o7777 != PRIVATE_FILE_MODE
        {
            return Err(invalid(format!(
                "private run artifact {} must be a real 0600 regular file; run `chmod 600 {}` before retrying",
                self.path.join(OsStr::from_bytes(name.to_bytes())).display(),
                self.path.join(OsStr::from_bytes(name.to_bytes())).display()
            )));
        }
        if opened.dev() != current.st_dev as u64 || opened.ino() != current.st_ino as u64 {
            return Err(invalid(format!(
                "private run artifact identity changed: {}",
                self.path.join(OsStr::from_bytes(name.to_bytes())).display()
            )));
        }
        Ok(())
    }

    pub(crate) fn hard_link(&self, source: &OsStr, target: &OsStr) -> io::Result<()> {
        #[cfg(unix)]
        {
            let source = c_name(source)?;
            let target = c_name(target)?;
            // SAFETY: both directory descriptors and names are valid.
            let result = unsafe {
                libc::linkat(
                    self.file.as_raw_fd(),
                    source.as_ptr(),
                    self.file.as_raw_fd(),
                    target.as_ptr(),
                    0,
                )
            };
            if result == 0 {
                Ok(())
            } else {
                Err(io::Error::last_os_error())
            }
        }
        #[cfg(not(unix))]
        {
            let _ = (source, target);
            Err(unsupported())
        }
    }

    pub(crate) fn rename(&self, source: &OsStr, target: &OsStr) -> io::Result<()> {
        #[cfg(unix)]
        {
            let source = c_name(source)?;
            let target = c_name(target)?;
            // SAFETY: both directory descriptors and names are valid.
            let result = unsafe {
                libc::renameat(
                    self.file.as_raw_fd(),
                    source.as_ptr(),
                    self.file.as_raw_fd(),
                    target.as_ptr(),
                )
            };
            if result == 0 {
                Ok(())
            } else {
                Err(io::Error::last_os_error())
            }
        }
        #[cfg(not(unix))]
        {
            let _ = (source, target);
            Err(unsupported())
        }
    }

    pub(crate) fn unlink(&self, name: &OsStr) -> io::Result<()> {
        #[cfg(unix)]
        {
            let name = c_name(name)?;
            // SAFETY: directory descriptor and name are valid.
            let result = unsafe { libc::unlinkat(self.file.as_raw_fd(), name.as_ptr(), 0) };
            if result == 0 {
                Ok(())
            } else {
                Err(io::Error::last_os_error())
            }
        }
        #[cfg(not(unix))]
        {
            let _ = name;
            Err(unsupported())
        }
    }

    pub(crate) fn unlink_if_same(&self, name: &OsStr, opened: &fs::Metadata) -> io::Result<()> {
        self.validate_file(name, opened)?;
        self.unlink(name)
    }

    pub(crate) fn sync_all(&self) -> io::Result<()> {
        self.file.sync_all()
    }
}

#[cfg(unix)]
fn c_name(name: &OsStr) -> io::Result<CString> {
    let mut components = Path::new(name).components();
    if name.is_empty()
        || name.as_bytes().contains(&b'/')
        || !matches!(components.next(), Some(Component::Normal(_)))
        || components.next().is_some()
    {
        return Err(invalid(
            "pinned directory operation requires one file name".to_string(),
        ));
    }
    CString::new(name.as_bytes()).map_err(|_| invalid("file name contains NUL".to_string()))
}

pub(crate) fn create_private_directory(path: &Path) -> io::Result<()> {
    #[cfg(unix)]
    {
        let mut builder = fs::DirBuilder::new();
        builder.mode(PRIVATE_DIRECTORY_MODE).create(path)?;
        validate_private_directory(path)
    }
    #[cfg(not(unix))]
    {
        let _ = path;
        Err(unsupported())
    }
}

pub(crate) fn ensure_private_standalone_directory(path: &Path) -> io::Result<()> {
    match fs::symlink_metadata(path) {
        Ok(_) => validate_private_directory(path),
        Err(error) if error.kind() == io::ErrorKind::NotFound => create_private_directory(path),
        Err(error) => Err(error),
    }
}

pub(crate) fn ensure_private_child_directory(path: &Path) -> io::Result<()> {
    let parent = path.parent().ok_or_else(|| {
        invalid(format!(
            "private directory has no parent: {}",
            path.display()
        ))
    })?;
    let name = path
        .file_name()
        .ok_or_else(|| invalid(format!("private directory has no name: {}", path.display())))?;
    let parent = PinnedPrivateDirectory::open(parent)?;
    match parent.open_child_directory(name) {
        Ok(_) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            parent.create_child_directory(name)?;
            parent.sync_all()
        }
        Err(error) => Err(error),
    }
}

pub(crate) fn validate_private_directory(path: &Path) -> io::Result<()> {
    #[cfg(unix)]
    {
        let metadata = fs::symlink_metadata(path)?;
        validate_private_directory_mode(path, &metadata)
    }
    #[cfg(not(unix))]
    {
        let _ = path;
        Err(unsupported())
    }
}

#[cfg(unix)]
fn validate_private_directory_mode(path: &Path, metadata: &fs::Metadata) -> io::Result<()> {
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(invalid(format!(
            "private run directory must be a real directory: {}",
            path.display()
        )));
    }
    let mode = metadata.mode() & 0o7777;
    if mode != PRIVATE_DIRECTORY_MODE {
        return Err(invalid(format!(
            "private run directory {} has mode {mode:04o}; run `chmod 700 {}` before retrying",
            path.display(),
            path.display()
        )));
    }
    Ok(())
}

#[cfg(test)]
pub(crate) fn configure_private_file(options: &mut fs::OpenOptions) {
    #[cfg(unix)]
    {
        options
            .mode(PRIVATE_FILE_MODE)
            .custom_flags(libc::O_NOFOLLOW);
    }
}

pub(crate) fn validate_private_regular_file(path: &Path) -> io::Result<()> {
    #[cfg(unix)]
    {
        let metadata = fs::symlink_metadata(path)?;
        if metadata.file_type().is_symlink() || !metadata.is_file() {
            return Err(invalid(format!(
                "private run artifact must be a real regular file: {}",
                path.display()
            )));
        }
        validate_private_file_mode(path, &metadata)
    }
    #[cfg(not(unix))]
    {
        let _ = path;
        Err(unsupported())
    }
}

pub(crate) fn validate_opened_private_regular_file(
    path: &Path,
    opened: &fs::Metadata,
) -> io::Result<()> {
    #[cfg(unix)]
    {
        if !opened.is_file() {
            return Err(invalid(format!(
                "private run artifact is not a regular file: {}",
                path.display()
            )));
        }
        validate_private_file_mode(path, opened)
    }
    #[cfg(not(unix))]
    {
        let _ = (path, opened);
        Err(unsupported())
    }
}

pub(crate) fn open_private_descendant_parent(
    run_directory: &Path,
    relative_path: &Path,
) -> io::Result<PinnedPrivateDirectory> {
    let parent = relative_path.parent().ok_or_else(|| {
        invalid(format!(
            "private run artifact has no parent: {}",
            relative_path.display()
        ))
    })?;
    let mut pinned = PinnedPrivateDirectory::open(run_directory)?;
    for component in parent.components() {
        let Component::Normal(component) = component else {
            return Err(invalid(format!(
                "private run artifact parent is unsafe: {}",
                relative_path.display()
            )));
        };
        pinned = pinned.open_child_directory(component)?;
    }
    Ok(pinned)
}

#[cfg(unix)]
fn validate_private_file_mode(path: &Path, metadata: &fs::Metadata) -> io::Result<()> {
    let mode = metadata.mode() & 0o7777;
    if mode != PRIVATE_FILE_MODE {
        return Err(invalid(format!(
            "private run artifact {} has mode {mode:04o}; run `chmod 600 {}` before retrying",
            path.display(),
            path.display()
        )));
    }
    Ok(())
}

fn invalid(message: String) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, message)
}

#[cfg(all(test, unix))]
pub(crate) fn write_private_fixture(
    path: impl AsRef<Path>,
    bytes: impl AsRef<[u8]>,
) -> io::Result<()> {
    use std::io::Write;

    let path = path.as_ref();
    let bytes = bytes.as_ref();
    match fs::symlink_metadata(path) {
        Ok(_) => fs::write(path, bytes),
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            let mut options = fs::OpenOptions::new();
            options.write(true).create_new(true);
            configure_private_file(&mut options);
            let mut file = options.open(path)?;
            file.write_all(bytes)
        }
        Err(error) => Err(error),
    }
}

#[cfg(all(test, unix))]
pub(crate) fn make_private_directory_fixture(path: &Path) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt;

    fs::set_permissions(path, fs::Permissions::from_mode(PRIVATE_DIRECTORY_MODE))
}

#[cfg(not(unix))]
fn unsupported() -> io::Error {
    io::Error::new(
        io::ErrorKind::Unsupported,
        "private loop workspaces are supported only on macOS and Linux",
    )
}

#[cfg(all(test, unix))]
mod tests {
    use std::os::unix::ffi::OsStrExt;

    use super::*;

    #[test]
    fn pinned_names_reject_separators_dot_components_and_nul() {
        for bytes in [
            b"name/".as_slice(),
            b"name/.",
            b"/name",
            b"a/b",
            b".",
            b"..",
            b"nul\0name",
        ] {
            assert!(c_name(OsStr::from_bytes(bytes)).is_err(), "{bytes:?}");
        }
        assert!(c_name(OsStr::new("name")).is_ok());
    }
}
