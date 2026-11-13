//! `qaros-stub`'s control exports, for tests in crates that forbid unsafe code. The stub is
//! opened from the file the library under test loads, so both share the stub's state.

use std::ffi::c_char;
use std::path::PathBuf;

pub struct StubControl {
    stub: libloading::Library,
}

impl StubControl {
    /// The stub library built next to the running test executable, as `ConnectOptions`'s
    /// `library_path`. Cargo names it `qaros_stub` when the stub comes from a path and
    /// `qaros_stub-<hash>` when it comes from git; the newest match wins.
    pub fn library_path() -> Result<PathBuf, String> {
        let prefix = format!("{}qaros_stub", std::env::consts::DLL_PREFIX);
        let suffix = std::env::consts::DLL_SUFFIX;
        let executable =
            std::env::current_exe().map_err(|error| format!("test executable: {error}"))?;
        executable
            .ancestors()
            .skip(1)
            .take(2)
            .filter_map(|directory| std::fs::read_dir(directory).ok())
            .flatten()
            .filter_map(Result::ok)
            .filter(|entry| is_stub_library(&entry.file_name().to_string_lossy(), &prefix, suffix))
            .max_by_key(|entry| {
                entry
                    .metadata()
                    .and_then(|metadata| metadata.modified())
                    .ok()
            })
            .map(|entry| entry.path())
            .ok_or_else(|| format!("{prefix}{suffix} not found next to the test executable"))
    }

    /// Opens the stub and resets its state.
    pub fn open() -> Result<Self, String> {
        let path = Self::library_path()?;
        #[allow(unsafe_code)]
        // SAFETY: the stub has no initialization side effects.
        let stub = unsafe { libloading::Library::new(&path) }
            .map_err(|error| format!("cannot open {}: {error}", path.display()))?;
        let control = Self { stub };
        let reset: unsafe extern "C" fn() = control.symbol("qar_stub_reset")?;
        #[allow(unsafe_code)]
        // SAFETY: no arguments.
        unsafe {
            reset();
        }
        Ok(control)
    }

    fn symbol<Function: Copy>(&self, name: &str) -> Result<Function, String> {
        #[allow(unsafe_code)]
        // SAFETY: every caller names a stub export with this signature.
        unsafe {
            self.stub
                .get::<Function>(name.as_bytes())
                .map(|symbol| *symbol)
                .map_err(|error| format!("{name}: {error}"))
        }
    }

    /// The stub's call log, one entry per line.
    pub fn calls(&self) -> Result<Vec<String>, String> {
        let call_log: unsafe extern "C" fn(*mut c_char, usize) -> usize =
            self.symbol("qar_stub_call_log")?;
        #[allow(unsafe_code)]
        // SAFETY: a null buffer only queries the length.
        let length = unsafe { call_log(std::ptr::null_mut(), 0) };
        let mut buffer = vec![0 as c_char; length + 1];
        #[allow(unsafe_code)]
        // SAFETY: the buffer holds length + 1 bytes.
        unsafe {
            call_log(buffer.as_mut_ptr(), buffer.len());
        }
        let bytes: Vec<u8> = buffer
            .iter()
            .take_while(|&&character| character != 0)
            .map(|&character| character.to_ne_bytes()[0])
            .collect();
        let text = String::from_utf8(bytes).map_err(|error| format!("call log: {error}"))?;
        Ok(text.lines().map(str::to_owned).collect())
    }

    pub fn live_handle_count(&self) -> Result<usize, String> {
        let count: unsafe extern "C" fn() -> usize = self.symbol("qar_stub_live_handle_count")?;
        #[allow(unsafe_code)]
        // SAFETY: no arguments.
        Ok(unsafe { count() })
    }
}

fn is_stub_library(file_name: &str, prefix: &str, suffix: &str) -> bool {
    let Some(stem) = file_name
        .strip_prefix(prefix)
        .and_then(|rest| rest.strip_suffix(suffix))
    else {
        return false;
    };
    stem.is_empty()
        || stem.strip_prefix('-').is_some_and(|hash| {
            !hash.is_empty() && hash.chars().all(|character| character.is_ascii_hexdigit())
        })
}
