use std::{
    fs::OpenOptions,
    io::Write,
    path::PathBuf,
    sync::OnceLock,
    time::Instant,
};

static PROFILE_PATH: OnceLock<Option<PathBuf>> = OnceLock::new();

fn profile_path() -> Option<&'static PathBuf> {
    PROFILE_PATH
        .get_or_init(|| {
            let value = std::env::var_os("MUSCLI_PROFILE")?;
            if value.is_empty() || value == "0" {
                return None;
            }
            if value == "1" {
                Some(std::env::temp_dir().join("muscli-profile.log"))
            } else {
                Some(PathBuf::from(value))
            }
        })
        .as_ref()
}

pub struct ProfileSpan {
    name: &'static str,
    started: Instant,
    path: Option<&'static PathBuf>,
}

pub fn span(name: &'static str) -> ProfileSpan {
    ProfileSpan {
        name,
        started: Instant::now(),
        path: profile_path(),
    }
}

impl Drop for ProfileSpan {
    fn drop(&mut self) {
        let Some(path) = self.path else {
            return;
        };
        let elapsed = self.started.elapsed();
        if let Ok(mut file) = OpenOptions::new().create(true).append(true).open(path) {
            let _ = writeln!(
                file,
                "{}\t{}\t{}.{:03}ms",
                std::process::id(),
                self.name,
                elapsed.as_millis(),
                elapsed.subsec_micros() % 1000
            );
        }
    }
}
