//! Java runtimes.
//!
//! - Discovery: the Windows registry, `/Library/Java/JavaVirtualMachines` and `java_home` on
//!   macOS, `/usr/lib/jvm` and similar on Linux, `PATH`, `JAVA_HOME`, the launcher's own managed
//!   runtimes.
//! - Probing: each candidate runs a small check with a timeout and an output cap; the result
//!   (`vl_core::java::JavaRuntime`) never trusts a directory name.
//! - Managed downloads of runtimes and the choice of a runtime for a game version.
