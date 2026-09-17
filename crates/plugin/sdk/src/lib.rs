//! Compile FutureSDR plugins against a prebuilt `futuresdr-plugin-rt`.
//!
//! Rust mangles into every symbol a hash of the crate that defines it, and
//! Cargo derives that hash from the whole dependency resolution — not only
//! from the crate's source and features. Rebuilding FutureSDR for a plugin
//! therefore easily yields other symbol names than the library the host has
//! loaded. An [`Sdk`] avoids the rebuild: it is one build of the shared
//! library plus the compiled metadata of every crate in it, and plugins are
//! compiled against exactly those files (`--extern` / `-L dependency=`).
//!
//! The settings that must match that build are fixed here: the compiler
//! (toolchain), the profile (it decides which generic instantiations the
//! library shares), and a shared standard library (`-C prefer-dynamic`).
//!
//! A plugin crate has no Cargo dependencies and is its own workspace; it uses
//! `futuresdr_plugin_rt`, which the SDK provides.

use std::fs;
use std::io::BufRead;
use std::io::BufReader;
use std::path::Path;
use std::path::PathBuf;
use std::process::Command;
use std::process::Stdio;

use anyhow::Context;
use anyhow::Result;
use anyhow::anyhow;
use anyhow::bail;

/// Crate name of the shared library.
pub const RT_CRATE: &str = "futuresdr_plugin_rt";
/// File describing an SDK directory.
pub const MANIFEST: &str = "sdk.env";

/// Compiler version this crate was built with (`rustc -V`).
pub const RUSTC: &str = env!("PLUGIN_SDK_RUSTC");
/// rustup toolchain this crate was built with, if known.
pub const TOOLCHAIN: &str = env!("PLUGIN_SDK_TOOLCHAIN");

/// How [`Sdk::build_plugin_with`] builds a plugin.
#[derive(Debug, Clone, Default)]
pub struct BuildOptions {
    /// Run Clippy's lints as well (`clippy-driver` must be installed).
    pub clippy: bool,
    /// Turn warnings into errors.
    pub deny_warnings: bool,
}

/// Cargo profile of an SDK build.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Profile {
    /// `cargo build`
    Dev,
    /// `cargo build --release`
    Release,
}

impl Profile {
    fn name(self) -> &'static str {
        match self {
            Profile::Dev => "dev",
            Profile::Release => "release",
        }
    }

    fn parse(s: &str) -> Result<Self> {
        match s {
            "dev" | "debug" => Ok(Profile::Dev),
            "release" => Ok(Profile::Release),
            other => bail!("unknown profile '{other}'"),
        }
    }
}

/// One build of the shared library, ready to compile plugins against.
#[derive(Debug, Clone)]
pub struct Sdk {
    /// The `futuresdr-plugin-rt` shared library.
    pub rt: PathBuf,
    /// Its full metadata, when kept apart from the library (newer rustc keeps
    /// only a stub inside the library).
    pub rt_metadata: Option<PathBuf>,
    /// Directories holding the compiled metadata of every crate in `rt`.
    pub deps: Vec<PathBuf>,
    /// Profile `rt` was built with; plugins are built with the same.
    pub profile: Profile,
    /// rustup toolchain `rt` was built with; empty if unknown.
    pub toolchain: String,
    /// `rustc -V` of that toolchain.
    pub rustc: String,
}

impl Sdk {
    /// The SDK of the running process: the shared library it has loaded,
    /// used in place in its Cargo target directory.
    ///
    /// Meant for hosts and tests built in this workspace, which were compiled
    /// by the same toolchain as this crate.
    pub fn of_this_process() -> Result<Self> {
        let maps = fs::read_to_string("/proc/self/maps").context("reading /proc/self/maps")?;
        // The path is the last field and may contain spaces.
        let rt = maps
            .lines()
            .filter_map(|line| line.find('/').map(|i| PathBuf::from(line[i..].trim_end())))
            .find(|path| is_rt_library(path))
            .ok_or_else(|| anyhow!("this process has not loaded lib{RT_CRATE}"))?;
        Self::of_library(&rt)
    }

    /// The SDK of the shared library at `rt`, used in place in the Cargo
    /// target directory it was built in, e.g.
    /// `target/release/libfuturesdr_plugin_rt.so`.
    ///
    /// The toolchain is taken to be the one this crate was built with.
    pub fn of_library(rt: &Path) -> Result<Self> {
        let rt = rt
            .canonicalize()
            .with_context(|| format!("{}", rt.display()))?;
        let profile_dir = rt
            .ancestors()
            .find(|dir| {
                matches!(
                    dir.file_name().and_then(|n| n.to_str()),
                    Some("debug" | "release")
                )
            })
            .ok_or_else(|| anyhow!("{} is not in a Cargo target directory", rt.display()))?
            .to_path_buf();
        let profile = Profile::parse(profile_dir.file_name().unwrap().to_str().unwrap())?;
        Ok(Self {
            deps: target_dependency_dirs(&profile_dir)?,
            rt_metadata: find_rt_metadata(&rt, &profile_dir),
            profile,
            rt,
            toolchain: TOOLCHAIN.to_string(),
            rustc: RUSTC.to_string(),
        })
    }

    /// The crates the shared library was built from, as file stems
    /// (`futuresdr-456abc4db73f1f48`), standard library included.
    pub fn crates(&self) -> Result<Vec<String>> {
        let metadata = self.rt_metadata.as_ref().unwrap_or(&self.rt);
        let mut rustc = Command::new("rustc");
        rustc.args(["-Z", "ls=root"]).arg(metadata);
        if !self.toolchain.is_empty() {
            rustc.env("RUSTUP_TOOLCHAIN", &self.toolchain);
        }
        let out = rustc.output().context("running rustc -Z ls")?;
        if !out.status.success() {
            bail!(
                "rustc -Z ls {}: {}",
                metadata.display(),
                String::from_utf8_lossy(&out.stderr)
            );
        }
        // "=External Dependencies=" lines: "<n> <name>-<disambiguator> hash ..."
        Ok(String::from_utf8_lossy(&out.stdout)
            .lines()
            .filter_map(|line| {
                let mut words = line.split_whitespace();
                words.next()?.parse::<usize>().ok()?;
                let stem = words.next()?;
                (words.next() == Some("hash")).then(|| stem.to_string())
            })
            .collect())
    }

    /// Copy the shared library and every crate it was built from into `out`,
    /// and describe them there. Returns the SDK in `out`.
    pub fn pack(&self, out: &Path) -> Result<Self> {
        let deps = out.join("deps");
        fs::create_dir_all(&deps)?;
        let sysroot = sysroot_libs(&self.toolchain)?;
        let copy = |from: &Path| -> Result<PathBuf> {
            let to = deps.join(from.file_name().unwrap());
            fs::copy(from, &to).with_context(|| format!("copying {}", from.display()))?;
            Ok(to)
        };

        for stem in self.crates()? {
            let find = |ext: &str| {
                let name = format!("lib{stem}.{ext}");
                self.deps
                    .iter()
                    .map(|dir| dir.join(&name))
                    .find(|f| f.is_file())
            };
            // Plugins get the code of these crates from the shared library;
            // compiling against them needs their metadata only. Proc-macro
            // crates are shared libraries themselves.
            let files: Vec<PathBuf> = match (find("rmeta"), find("rlib")) {
                (Some(meta), _) => vec![meta],
                (None, rlib) => rlib.into_iter().collect(),
            }
            .into_iter()
            .chain(find("so"))
            .collect();
            if files.is_empty() {
                // The standard library comes with the toolchain.
                let in_sysroot = ["rlib", "rmeta", "so"]
                    .iter()
                    .any(|ext| sysroot.join(format!("lib{stem}.{ext}")).is_file());
                if !in_sysroot {
                    bail!("crate {stem} of lib{RT_CRATE} not found");
                }
                continue;
            }
            for f in files {
                copy(&f)?;
            }
        }

        let sdk = Self {
            rt: copy(&self.rt)?,
            rt_metadata: self.rt_metadata.as_deref().map(copy).transpose()?,
            deps: vec![deps],
            profile: self.profile,
            toolchain: self.toolchain.clone(),
            rustc: self.rustc.clone(),
        };
        sdk.write_manifest(out)?;
        Ok(sdk)
    }

    /// Open an SDK directory written by [`Sdk::pack`].
    pub fn open(dir: &Path) -> Result<Self> {
        // Absolute: builds run the compiler from the plugin's directory.
        let dir = &dir
            .canonicalize()
            .with_context(|| format!("{} is not an SDK", dir.display()))?;
        let text = fs::read_to_string(dir.join(MANIFEST))
            .with_context(|| format!("{} is not an SDK", dir.display()))?;
        let get = |key: &str| -> Result<String> {
            text.lines()
                .find_map(|l| l.strip_prefix(key)?.strip_prefix('='))
                .map(str::to_string)
                .ok_or_else(|| anyhow!("{MANIFEST}: missing '{key}'"))
        };
        let deps = dir.join("deps");
        let rt = deps.join(get("rt")?);
        let rt_metadata = Some(rt.with_extension("rmeta")).filter(|m| m.is_file());
        Ok(Self {
            rt,
            rt_metadata,
            deps: vec![deps],
            profile: Profile::parse(&get("profile")?)?,
            toolchain: get("toolchain")?,
            rustc: get("rustc")?,
        })
    }

    fn write_manifest(&self, dir: &Path) -> Result<()> {
        let text = format!(
            "rt={}\nprofile={}\ntoolchain={}\nrustc={}\n",
            self.rt.file_name().unwrap().to_string_lossy(),
            self.profile.name(),
            self.toolchain,
            self.rustc,
        );
        fs::write(dir.join(MANIFEST), text)?;
        Ok(())
    }

    /// Compile the plugin crate whose manifest is `manifest` against this
    /// SDK, into `target_dir`. Returns the plugin library.
    ///
    /// The crate must have `crate-type = ["dylib"]`, no dependencies, and its
    /// own `[workspace]`.
    ///
    /// Release builds use one codegen unit and drop the symbol table (the
    /// exported symbols stay), for small and fast plugins, unless the crate's
    /// `[profile.release]` sets `codegen-units` or `strip`.
    pub fn build_plugin(&self, manifest: &Path, target_dir: &Path) -> Result<PathBuf> {
        self.build_plugin_with(manifest, target_dir, &BuildOptions::default())
    }

    /// [`build_plugin`](Self::build_plugin), with `options`.
    pub fn build_plugin_with(
        &self,
        manifest: &Path,
        target_dir: &Path,
        options: &BuildOptions,
    ) -> Result<PathBuf> {
        let mut cargo = Command::new(cargo_bin());
        if options.clippy {
            cargo.env("RUSTC_WORKSPACE_WRAPPER", "clippy-driver");
        }
        if self.profile == Profile::Release {
            let text = fs::read_to_string(manifest)
                .with_context(|| format!("reading {}", manifest.display()))?;
            let table: toml::Table =
                toml::from_str(&text).with_context(|| format!("parsing {}", manifest.display()))?;
            let release = table
                .get("profile")
                .and_then(|p| p.get("release"))
                .and_then(|r| r.as_table());
            for (key, var, value) in [
                ("codegen-units", "CARGO_PROFILE_RELEASE_CODEGEN_UNITS", "1"),
                ("strip", "CARGO_PROFILE_RELEASE_STRIP", "symbols"),
            ] {
                if !release.is_some_and(|r| r.contains_key(key)) && std::env::var_os(var).is_none()
                {
                    cargo.env(var, value);
                }
            }
        }
        self.prepare(&mut cargo);
        cargo
            .args(["rustc", "--lib", "--message-format=json-render-diagnostics"])
            .arg("--manifest-path")
            .arg(manifest)
            .arg("--target-dir")
            .arg(target_dir);
        if self.profile == Profile::Release {
            cargo.arg("--release");
        }
        cargo.arg("--").args(self.rustc_flags());
        if options.deny_warnings {
            cargo.args(["-D", "warnings"]);
        }

        let mut child = cargo
            .stdout(Stdio::piped())
            .spawn()
            .context("running cargo")?;
        let mut library = None;
        for line in BufReader::new(child.stdout.take().unwrap()).lines() {
            let line = line?;
            let Ok(msg) = serde_json::from_str::<serde_json::Value>(&line) else {
                continue;
            };
            if msg["reason"] != "compiler-artifact" {
                continue;
            }
            for f in msg["filenames"].as_array().into_iter().flatten() {
                let f = PathBuf::from(f.as_str().unwrap_or_default());
                if f.extension().and_then(|e| e.to_str()) == Some("so") {
                    library = Some(f);
                }
            }
        }
        if !child.wait()?.success() {
            bail!("building plugin {} failed", manifest.display());
        }
        library.ok_or_else(|| anyhow!("cargo built no library for {}", manifest.display()))
    }

    /// Run the unit tests of the plugin crate whose manifest is `manifest`,
    /// against this SDK, passing `args` to the test harness.
    pub fn test_plugin(&self, manifest: &Path, target_dir: &Path, args: &[String]) -> Result<()> {
        let mut cargo = Command::new(cargo_bin());
        self.prepare(&mut cargo);
        cargo
            .args(["test", "--lib"])
            .arg("--manifest-path")
            .arg(manifest)
            .arg("--target-dir")
            .arg(target_dir);
        if self.profile == Profile::Release {
            cargo.arg("--release");
        }
        cargo.arg("--").args(args);
        // `cargo test` passes no extra compiler flags on its command line; the
        // plugin has no dependencies, so these reach only its own crate.
        cargo.env("CARGO_ENCODED_RUSTFLAGS", self.rustc_flags().join("\u{1f}"));
        // The test program loads the shared library and the standard library.
        let mut path = vec![
            self.rt.parent().unwrap_or(Path::new(".")).to_path_buf(),
            sysroot_libs(&self.toolchain)?,
        ];
        path.extend(std::env::split_paths(
            &std::env::var_os("LD_LIBRARY_PATH").unwrap_or_default(),
        ));
        cargo.env("LD_LIBRARY_PATH", std::env::join_paths(path)?);
        if !cargo.status().context("running cargo")?.success() {
            bail!("testing plugin {} failed", manifest.display());
        }
        Ok(())
    }

    /// Compiler flags that make the shared library, and what it is built
    /// from, available to a plugin crate.
    fn rustc_flags(&self) -> Vec<String> {
        let mut flags = vec![
            "--extern".to_string(),
            format!("{RT_CRATE}={}", self.rt.display()),
            "-C".to_string(),
            "prefer-dynamic".to_string(),
        ];
        if let Some(meta) = &self.rt_metadata {
            flags.push("--extern".to_string());
            flags.push(format!("{RT_CRATE}={}", meta.display()));
        }
        for dir in &self.deps {
            flags.push("-L".to_string());
            flags.push(format!("dependency={}", dir.display()));
        }
        flags
    }

    /// The toolchain of the SDK, and nothing of an enclosing build.
    fn prepare(&self, cargo: &mut Command) {
        if !self.toolchain.is_empty() {
            cargo.env("RUSTUP_TOOLCHAIN", &self.toolchain);
        }
        for var in [
            "RUSTFLAGS",
            "CARGO_ENCODED_RUSTFLAGS",
            "CARGO_TARGET_DIR",
            "CARGO_BUILD_TARGET_DIR",
        ] {
            cargo.env_remove(var);
        }
    }
}

/// Where Cargo keeps compiled crates under `profile_dir`
/// (`<target>/debug` or `<target>/release`): `deps/`, or one `out/`
/// directory per crate in the newer build-directory layout.
fn target_dependency_dirs(profile_dir: &Path) -> Result<Vec<PathBuf>> {
    let mut dirs = Vec::new();
    let flat = profile_dir.join("deps");
    if flat.is_dir() {
        dirs.push(flat);
    }
    let build = profile_dir.join("build");
    if build.is_dir() {
        for package in fs::read_dir(&build)? {
            let package = package?.path();
            if !package.is_dir() {
                continue;
            }
            for unit in fs::read_dir(&package)? {
                let out = unit?.path().join("out");
                let has_crates = out.is_dir()
                    && fs::read_dir(&out)?.filter_map(|e| e.ok()).any(|e| {
                        matches!(
                            e.path().extension().and_then(|x| x.to_str()),
                            Some("rlib" | "rmeta" | "so")
                        )
                    });
                if has_crates {
                    dirs.push(out);
                }
            }
        }
    }
    if dirs.is_empty() {
        bail!("no compiled crates under {}", profile_dir.display());
    }
    dirs.sort();
    Ok(dirs)
}

/// Where the toolchain keeps the standard library.
fn sysroot_libs(toolchain: &str) -> Result<PathBuf> {
    let mut rustc = Command::new("rustc");
    rustc.args(["--print", "target-libdir"]);
    if !toolchain.is_empty() {
        rustc.env("RUSTUP_TOOLCHAIN", toolchain);
    }
    let out = rustc
        .output()
        .context("running rustc --print target-libdir")?;
    Ok(PathBuf::from(String::from_utf8_lossy(&out.stdout).trim()))
}

/// The `.rmeta` belonging to the shared library `rt`: next to it, or, when
/// `rt` is the copy Cargo places in the profile directory, next to the
/// original in the build directory (same file, same inode).
fn find_rt_metadata(rt: &Path, profile_dir: &Path) -> Option<PathBuf> {
    use std::os::unix::fs::MetadataExt;
    let sibling = rt.with_extension("rmeta");
    if sibling.is_file() {
        return Some(sibling);
    }
    let inode = fs::metadata(rt).ok()?.ino();
    let package = profile_dir.join("build").join("futuresdr-plugin-rt");
    fs::read_dir(package)
        .ok()?
        .filter_map(|e| e.ok())
        .find_map(|unit| {
            let out = unit.path().join("out");
            let library = out.join(rt.file_name()?);
            let meta = library.with_extension("rmeta");
            (fs::metadata(&library).ok()?.ino() == inode && meta.is_file()).then_some(meta)
        })
}

fn is_rt_library(path: &Path) -> bool {
    path.file_name()
        .and_then(|n| n.to_str())
        .is_some_and(|n| n.starts_with(&format!("lib{RT_CRATE}")) && n.ends_with(".so"))
}

fn cargo_bin() -> String {
    std::env::var("CARGO").unwrap_or_else(|_| "cargo".to_string())
}
