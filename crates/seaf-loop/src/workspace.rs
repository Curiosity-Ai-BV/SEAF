use std::{
    fs,
    path::{Path, PathBuf},
};

pub fn write_artifact(
    run_directory: &Path,
    file_name: &str,
    bytes: &[u8],
) -> std::io::Result<PathBuf> {
    fs::create_dir_all(run_directory)?;
    let path = run_directory.join(file_name);
    fs::write(&path, bytes)?;
    Ok(path)
}
