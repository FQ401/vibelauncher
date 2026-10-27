//! Java runtimes.
//!
//! - Discovery ([`discover`]): the Windows registry, `/Library/Java/JavaVirtualMachines` and
//!   `java_home` on macOS, `/usr/lib/jvm` and similar on Linux, `PATH`, `JAVA_HOME`, other
//!   launchers' runtimes, SDK managers and the launcher's own managed runtimes.
//! - Probing ([`probe`]): each candidate runs `java -XshowSettings:properties -version` with a
//!   timeout and an output cap; the result (`vl_core::java::JavaRuntime`) never trusts a directory
//!   name. A broken or hanging Java is skipped and never stops the scan.
//! - Cache ([`cache`]): results are remembered by path and validated by the executable's
//!   modification time and size.
//! - Managed downloads of runtimes and the choice of a runtime for a game version belong here
//!   too, in later work.

pub mod cache;
pub mod discover;
pub mod probe;
mod run;

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Mutex, PoisonError};
use std::time::Duration;

use vl_core::java::JavaRuntime;

use cache::ProbeCache;
use discover::{Candidate, Env};

/// How a [`JavaFinder`] works.
#[derive(Debug, Clone)]
pub struct FinderOptions {
    /// The directory of the launcher's own runtimes; what is found there is marked `managed`.
    pub managed_dir: Option<PathBuf>,
    /// Where to keep the probe cache between runs; `None` keeps it in memory only.
    pub cache_file: Option<PathBuf>,
    /// The time one check may take.
    pub probe_timeout: Duration,
    /// How many checks run at once.
    pub jobs: usize,
}

impl Default for FinderOptions {
    fn default() -> Self {
        Self {
            managed_dir: None,
            cache_file: None,
            probe_timeout: Duration::from_secs(10),
            jobs: 8,
        }
    }
}

/// Finds and checks the Java runtimes of this machine.
pub struct JavaFinder {
    options: FinderOptions,
    cache: Mutex<ProbeCache>,
}

impl JavaFinder {
    /// A finder; the cache file, if any, is read now.
    pub fn new(options: FinderOptions) -> Self {
        let cache = options
            .cache_file
            .as_deref()
            .map(ProbeCache::load)
            .unwrap_or_default();
        Self {
            options,
            cache: Mutex::new(cache),
        }
    }

    /// Searches this machine and checks every candidate. The result is sorted by major version
    /// (newest first), then managed runtimes first, then by path.
    pub fn scan(&self) -> Vec<JavaRuntime> {
        let candidates =
            discover::all_candidates(&Env::from_process(), self.options.managed_dir.as_deref());
        self.check(candidates)
    }

    /// Checks the given candidates (duplicates are dropped) and updates the cache.
    pub fn check(&self, candidates: Vec<Candidate>) -> Vec<JavaRuntime> {
        let candidates = discover::dedupe(candidates);
        let next = AtomicUsize::new(0);
        let results: Mutex<Vec<JavaRuntime>> = Mutex::new(Vec::new());
        let workers = self.options.jobs.clamp(1, candidates.len().max(1));
        std::thread::scope(|scope| {
            for _ in 0..workers {
                scope.spawn(|| {
                    loop {
                        let i = next.fetch_add(1, Ordering::Relaxed);
                        let Some(candidate) = candidates.get(i) else {
                            break;
                        };
                        if let Some(runtime) = self.check_one(candidate) {
                            results
                                .lock()
                                .unwrap_or_else(PoisonError::into_inner)
                                .push(runtime);
                        }
                    }
                });
            }
        });
        let mut found = results.into_inner().unwrap_or_else(PoisonError::into_inner);
        found.sort_by(|a, b| {
            b.major
                .cmp(&a.major)
                .then(b.managed.cmp(&a.managed))
                .then_with(|| a.path.cmp(&b.path))
        });

        let mut cache = self.cache.lock().unwrap_or_else(PoisonError::into_inner);
        cache.retain(|path| found.iter().any(|r| r.path == path));
        if let Some(file) = &self.options.cache_file {
            // A cache that cannot be written only costs time on the next run.
            let _ = cache.save(file);
        }
        found
    }

    /// The runtime behind one candidate, from the cache or by a check; `None` when it is not a
    /// working Java.
    fn check_one(&self, candidate: &Candidate) -> Option<JavaRuntime> {
        let cached = self
            .cache
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(&candidate.path);
        let mut runtime = match cached {
            Some(runtime) => runtime,
            None => {
                let runtime = probe::probe(&candidate.path, self.options.probe_timeout).ok()?;
                self.cache
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .insert(&runtime);
                runtime
            }
        };
        runtime.managed = candidate.managed;
        Some(runtime)
    }

    /// Checks one executable without caching.
    pub fn probe(&self, java: &Path) -> Result<JavaRuntime, probe::ProbeError> {
        probe::probe(java, self.options.probe_timeout)
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::time::Instant;

    use super::*;
    use discover::testutil::TempDir;

    const NOISE: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

    /// Writes a fake `java` whose behavior `kind` names.
    fn fake(dir: &Path, name: &str, kind: &str) -> PathBuf {
        let bin = dir.join(name).join("bin");
        fs::create_dir_all(&bin).unwrap();
        let exe = bin.join(if cfg!(windows) { "java.bat" } else { "java" });
        fs::write(&exe, script(kind)).unwrap();
        make_executable(&exe);
        exe
    }

    #[cfg(windows)]
    fn script(kind: &str) -> String {
        let say = |line: &str| format!("echo     {line} 1>&2\r\n");
        let body = match kind {
            "NOTJAVA" => "echo Error: nothing here 1>&2\r\nexit /b 1\r\n".to_owned(),
            "HANG" => "ping -n 30 127.0.0.1 >nul\r\n".to_owned(),
            "FLOOD" => format!(
                "{}for /l %%i in (1,1,3000) do echo {NOISE} 1>&2\r\n",
                say("java.version = 17.0.2")
            ),
            version => format!(
                "{}{}{}",
                say(&format!("java.version = {version}")),
                say("java.vendor = Synthetic"),
                say("os.arch = amd64")
            ),
        };
        format!("@echo off\r\n{body}")
    }

    #[cfg(not(windows))]
    fn script(kind: &str) -> String {
        let say = |line: &str| format!("echo '    {line}' >&2\n");
        let body = match kind {
            "NOTJAVA" => "echo 'Error: nothing here' >&2\nexit 1\n".to_owned(),
            "HANG" => "sleep 30\n".to_owned(),
            "FLOOD" => format!(
                "{}i=0\nwhile [ $i -lt 3000 ]; do echo {NOISE} >&2; i=$((i+1)); done\n",
                say("java.version = 17.0.2")
            ),
            version => format!(
                "{}{}{}",
                say(&format!("java.version = {version}")),
                say("java.vendor = Synthetic"),
                say("os.arch = amd64")
            ),
        };
        format!("#!/bin/sh\n{body}")
    }

    #[cfg(unix)]
    fn make_executable(path: &Path) {
        use std::os::unix::fs::PermissionsExt;
        let _ = fs::set_permissions(path, fs::Permissions::from_mode(0o755));
    }

    #[cfg(not(unix))]
    fn make_executable(_path: &Path) {}

    fn candidate(path: &Path, managed: bool) -> Candidate {
        Candidate {
            path: path.to_path_buf(),
            managed,
        }
    }

    // One test starts every fake program: parallel tests forking while a script is being
    // written can make `exec` fail with a busy-file error on Linux.
    #[test]
    fn broken_and_hanging_javas_do_not_stop_the_scan() {
        let tmp = TempDir::new();
        let good8 = fake(tmp.path(), "good8", "1.8.0_345");
        let good21 = fake(tmp.path(), "good21", "21.0.1");
        let managed = fake(tmp.path(), "managed", "17.0.9");
        let not_java = fake(tmp.path(), "notjava", "NOTJAVA");
        let hang = fake(tmp.path(), "hang", "HANG");
        let flood = fake(tmp.path(), "flood", "FLOOD");
        let garbage = tmp.path().join("garbage").join("java");
        fs::create_dir_all(garbage.parent().unwrap()).unwrap();
        fs::write(&garbage, [0u8, 159, 146, 150, 1, 2]).unwrap();
        let missing = tmp.path().join("missing").join("java");

        let cache_file = tmp.path().join("cache").join("java-cache.json");
        let options = FinderOptions {
            probe_timeout: Duration::from_secs(2),
            cache_file: Some(cache_file.clone()),
            ..FinderOptions::default()
        };
        let list = vec![
            candidate(&good8, false),
            candidate(&not_java, false),
            candidate(&hang, false),
            candidate(&flood, false),
            candidate(&garbage, false),
            candidate(&missing, false),
            candidate(&good21, false),
            candidate(&managed, true),
            candidate(&good21, false),
        ];
        let started = Instant::now();
        let found = JavaFinder::new(options.clone()).check(list.clone());
        assert!(started.elapsed() < Duration::from_secs(30));

        let got: Vec<(u32, PathBuf, bool)> = found
            .iter()
            .map(|r| (r.major, r.path.clone(), r.managed))
            .collect();
        assert_eq!(
            got,
            vec![
                (21, good21, false),
                (17, managed, true),
                (17, flood, false),
                (8, good8, false),
            ],
            "unexpected result {found:?}"
        );
        assert_eq!(found[0].vendor, "Synthetic");

        // A second finder starts from the cache file and reaches the same result.
        assert!(cache_file.is_file());
        let again = JavaFinder::new(options);
        let cached = again.cache.lock().map(|c| c.len()).unwrap_or(0);
        assert_eq!(cached, 4);
        assert_eq!(again.check(list), found);
    }
}
