//! Read source only after the OS identifies the file actually opened.
//! Pathname checks cannot establish this when a symlink changes between calls.

use std::fs::{self, File, OpenOptions};
use std::io::{self, Read};
use std::path::{Component, Path, PathBuf};

const MAX_READ_BYTES: u64 = 64 * 1024 * 1024;

struct SourceReader {
    root: PathBuf,
}

#[derive(Default)]
struct ReadOutcome {
    content: Option<Vec<u8>>,
    bytes_read: u32,
    limit_reached: bool,
}

impl SourceReader {
    fn new(root: &Path) -> io::Result<Self> {
        let root = fs::canonicalize(root)?;
        if !root.is_dir() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "source root is not a directory",
            ));
        }
        Ok(Self { root })
    }

    fn read(&self, relative: &Path, max_file_bytes: u32, remaining_bytes: u32) -> ReadOutcome {
        if relative.as_os_str().is_empty()
            || relative
                .components()
                .any(|c| !matches!(c, Component::Normal(_) | Component::CurDir))
        {
            return ReadOutcome::default();
        }
        let file = match open_source(&self.root.join(relative)) {
            Ok(file) => file,
            Err(_) => return ReadOutcome::default(),
        };
        // Inspect the open handle, never a second lookup of the supplied name.
        if !opened_path(&file).is_ok_and(|p| p.starts_with(&self.root)) {
            return ReadOutcome::default();
        }
        let metadata = match file.metadata() {
            Ok(m) if m.is_file() => m,
            _ => return ReadOutcome::default(),
        };
        let size = metadata.len();
        if size > u64::from(max_file_bytes).min(MAX_READ_BYTES) {
            return ReadOutcome::default();
        }
        if size > u64::from(remaining_bytes).min(MAX_READ_BYTES) {
            return ReadOutcome {
                limit_reached: true,
                ..ReadOutcome::default()
            };
        }
        read_open_file(file, size)
    }
}

fn read_open_file(file: File, size: u64) -> ReadOutcome {
    // One extra byte detects growth after metadata. Even discarded or failed
    // reads count against the caller's total scan budget.
    let mut content = Vec::with_capacity(size as usize + 1);
    let result = file.take(size + 1).read_to_end(&mut content);
    let bytes_read = content.len() as u32;
    let content = (result.is_ok() && u64::from(bytes_read) <= size).then_some(content);
    ReadOutcome {
        content,
        bytes_read,
        limit_reached: false,
    }
}

fn open_source(path: &Path) -> io::Result<File> {
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NONBLOCK);
    }
    options.open(path)
}

#[cfg(target_os = "linux")]
fn opened_path(file: &File) -> io::Result<PathBuf> {
    use std::os::fd::AsRawFd;
    fs::read_link(format!("/proc/self/fd/{}", file.as_raw_fd()))
}

#[cfg(target_os = "macos")]
fn opened_path(file: &File) -> io::Result<PathBuf> {
    use std::os::fd::AsRawFd;
    use std::os::unix::ffi::OsStringExt;
    let mut buffer = vec![0u8; libc::PATH_MAX as usize];
    // SAFETY: the descriptor remains owned by file; F_GETPATH writes at most
    // PATH_MAX bytes into the allocated buffer and does not take ownership.
    if unsafe { libc::fcntl(file.as_raw_fd(), libc::F_GETPATH, buffer.as_mut_ptr()) } == -1 {
        return Err(io::Error::last_os_error());
    }
    let end = buffer
        .iter()
        .position(|b| *b == 0)
        .ok_or_else(|| io::Error::other("unterminated descriptor path"))?;
    buffer.truncate(end);
    Ok(std::ffi::OsString::from_vec(buffer).into())
}

#[cfg(windows)]
fn opened_path(file: &File) -> io::Result<PathBuf> {
    use std::os::windows::ffi::OsStringExt;
    use std::os::windows::io::AsRawHandle;
    #[link(name = "kernel32")]
    extern "system" {
        fn GetFinalPathNameByHandleW(
            handle: *mut std::ffi::c_void,
            path: *mut u16,
            size: u32,
            flags: u32,
        ) -> u32;
        fn GetFileType(handle: *mut std::ffi::c_void) -> u32;
    }
    // SAFETY: file owns a live handle. Only disk files have source content;
    // refuse pipes/devices before asking for their normalized path.
    if unsafe { GetFileType(file.as_raw_handle()) } != 1 {
        return Err(io::Error::other("source is not a disk file"));
    }
    let mut buffer = vec![0u16; 512];
    for _ in 0..2 {
        // SAFETY: the live handle is borrowed and the buffer size is exact.
        // Flags 0 request a normalized DOS path, matching fs::canonicalize.
        let size = unsafe {
            GetFinalPathNameByHandleW(
                file.as_raw_handle(),
                buffer.as_mut_ptr(),
                buffer.len() as u32,
                0,
            )
        };
        if size == 0 {
            return Err(io::Error::last_os_error());
        }
        if (size as usize) < buffer.len() {
            buffer.truncate(size as usize);
            return Ok(std::ffi::OsString::from_wide(&buffer).into());
        }
        if size > 32_768 {
            break;
        }
        buffer.resize(size as usize + 1, 0);
    }
    Err(io::Error::other("descriptor path exceeds its buffer"))
}

#[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
fn opened_path(_file: &File) -> io::Result<PathBuf> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "no descriptor path lookup",
    ))
}

#[napi_derive::napi(object)]
pub struct ContainedSourceRead {
    pub content: Option<napi::bindgen_prelude::Buffer>,
    pub bytes_read: u32,
    pub limit_reached: bool,
}

#[napi_derive::napi]
pub struct ContainedSourceReader {
    inner: SourceReader,
}

#[napi_derive::napi]
impl ContainedSourceReader {
    #[napi(constructor)]
    pub fn new(root: String) -> napi::Result<Self> {
        let inner = SourceReader::new(Path::new(&root))
            .map_err(|e| napi::Error::from_reason(e.to_string()))?;
        Ok(Self { inner })
    }

    #[napi]
    pub fn read(
        &self,
        relative: String,
        max_file_bytes: u32,
        remaining_bytes: u32,
    ) -> ContainedSourceRead {
        let read = self
            .inner
            .read(Path::new(&relative), max_file_bytes, remaining_bytes);
        ContainedSourceRead {
            content: read.content.map(Into::into),
            bytes_read: read.bytes_read,
            limit_reached: read.limit_reached,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    static NEXT: AtomicUsize = AtomicUsize::new(0);

    struct Fixture(PathBuf);
    impl Fixture {
        fn new() -> Self {
            let path = std::env::temp_dir().join(format!(
                "cg-contained-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir_all(path.join("root")).unwrap();
            fs::write(path.join("root/source.ts"), b"alpha").unwrap();
            fs::write(path.join("outside.ts"), b"secret").unwrap();
            Self(path)
        }
        fn reader(&self) -> SourceReader {
            SourceReader::new(&self.0.join("root")).unwrap()
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn reads_only_regular_in_root_files_and_enforces_both_byte_limits() {
        let f = Fixture::new();
        let reader = f.reader();
        assert_eq!(
            reader.read(Path::new("source.ts"), 5, 5).content.unwrap(),
            b"alpha"
        );
        assert!(reader.read(Path::new("source.ts"), 4, 5).content.is_none());
        assert!(reader.read(Path::new("source.ts"), 5, 4).limit_reached);
        for path in ["../outside.ts", "/outside.ts", ".", "missing.ts"] {
            assert!(
                reader.read(Path::new(path), 100, 100).content.is_none(),
                "{path}"
            );
        }
    }

    #[test]
    fn charges_growth_even_when_content_is_discarded() {
        let f = Fixture::new();
        let path = f.0.join("root/source.ts");
        let file = open_source(&path).unwrap();
        let size = file.metadata().unwrap().len();
        fs::write(&path, b"alphabet").unwrap();
        let result = read_open_file(file, size);
        assert!(result.content.is_none());
        assert_eq!(result.bytes_read, 6);
    }

    #[test]
    fn descriptor_path_follows_the_open_file_after_its_name_is_replaced() {
        let f = Fixture::new();
        let original = f.0.join("root/source.ts");
        let file = open_source(&original).unwrap();
        fs::rename(&original, f.0.join("moved-outside.ts")).unwrap();
        fs::write(&original, b"replacement").unwrap();
        assert!(!opened_path(&file).unwrap().starts_with(&f.reader().root));
        assert_eq!(
            f.reader()
                .read(Path::new("source.ts"), 100, 100)
                .content
                .unwrap(),
            b"replacement"
        );
    }

    #[test]
    fn supports_unicode_root_and_source_names() {
        let f = Fixture::new();
        let root = f.0.join("root/Évidence");
        fs::create_dir(&root).unwrap();
        fs::write(root.join("résumé.ts"), b"unicode").unwrap();
        let reader = SourceReader::new(&root).unwrap();
        assert_eq!(
            reader
                .read(Path::new("résumé.ts"), 100, 100)
                .content
                .unwrap(),
            b"unicode"
        );
    }

    #[cfg(unix)]
    #[test]
    fn checks_open_file_after_a_symlink_is_replaced() {
        use std::os::unix::fs::symlink;
        let f = Fixture::new();
        let alias = f.0.join("root/alias.ts");
        symlink(f.0.join("outside.ts"), &alias).unwrap();
        let outside = open_source(&alias).unwrap();
        fs::remove_file(&alias).unwrap();
        symlink(f.0.join("root/source.ts"), &alias).unwrap();
        assert!(!opened_path(&outside).unwrap().starts_with(&f.reader().root));
        assert_eq!(
            f.reader()
                .read(Path::new("alias.ts"), 100, 100)
                .content
                .unwrap(),
            b"alpha"
        );
        fs::remove_file(&alias).unwrap();
        symlink(f.0.join("outside.ts"), &alias).unwrap();
        assert!(f
            .reader()
            .read(Path::new("alias.ts"), 100, 100)
            .content
            .is_none());
    }
}
