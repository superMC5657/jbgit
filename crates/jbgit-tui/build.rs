//! Embeds the Windows application icon into the `jbgit` binary.

use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

fn main() {
    println!("cargo:rerun-if-changed=build.rs");

    if env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }

    let manifest_dir = PathBuf::from(env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR"));
    let icon_path = manifest_dir.join("..").join("..").join("assets").join("icon.ico");
    println!("cargo:rerun-if-changed={}", icon_path.display());

    let Ok(abs_icon) = fs::canonicalize(&icon_path) else {
        println!(
            "cargo:warning=Windows icon not found at {}, skipping icon embedding",
            icon_path.display()
        );
        return;
    };

    // Normalize to forward slashes without UNC `\\?\` prefix so rc.exe / llvm-rc / windres all accept it.
    let rc_icon_path = abs_icon
        .to_string_lossy()
        .trim_start_matches(r"\\?\")
        .replace('\\', "/");

    let out_dir = PathBuf::from(env::var("OUT_DIR").expect("OUT_DIR"));
    let rc_file = out_dir.join("jbgit.rc");
    let rc_content = format!("1 ICON \"{rc_icon_path}\"\n");
    fs::write(&rc_file, rc_content).expect("failed to write jbgit.rc");

    let target_env = env::var("CARGO_CFG_TARGET_ENV").unwrap_or_default();
    let target_arch = env::var("CARGO_CFG_TARGET_ARCH").unwrap_or_default();

    let compiled_res = if target_env == "gnu" {
        compile_gnu_rc(&rc_file, &out_dir, &target_arch)
    } else {
        compile_msvc_rc(&rc_file, &out_dir)
    };

    match compiled_res {
        Some(res_path) => {
            println!("cargo:rustc-link-arg-bins={}", res_path.display());
        }
        None => {
            println!("cargo:warning=No Windows resource compiler (rc.exe / llvm-rc / windres) found; jbgit.exe built without icon");
        }
    }
}

fn compile_msvc_rc(rc_file: &Path, out_dir: &Path) -> Option<PathBuf> {
    let res_path = out_dir.join("jbgit.res");
    // Run the host-architecture rc.exe so cross-compiling (e.g. Windows ARM64 on x64 CI) works too.
    let sdk_arch = match env::consts::ARCH {
        "x86_64" => "x64",
        "x86" => "x86",
        "aarch64" => "arm64",
        _ => "x64",
    };

    if let Some(rc_exe) = find_msvc_rc(sdk_arch) {
        let status = Command::new(&rc_exe)
            .arg("/nologo")
            .arg("/fo")
            .arg(&res_path)
            .arg(rc_file)
            .status()
            .ok()?;
        if status.success() {
            return Some(res_path);
        }
    }

    if command_exists("llvm-rc") {
        let status = Command::new("llvm-rc")
            .arg("/fo")
            .arg(&res_path)
            .arg(rc_file)
            .status()
            .ok()?;
        if status.success() {
            return Some(res_path);
        }
    }

    None
}

fn compile_gnu_rc(rc_file: &Path, out_dir: &Path, target_arch: &str) -> Option<PathBuf> {
    let obj_path = out_dir.join("jbgit_res.o");
    let candidates: &[&str] = match target_arch {
        "x86_64" => &["x86_64-w64-mingw32-windres", "windres"],
        "x86" => &["i686-w64-mingw32-windres", "windres"],
        _ => &["windres"],
    };

    for tool in candidates {
        if command_exists(tool) {
            let status = Command::new(tool)
                .arg("-i")
                .arg(rc_file)
                .arg("-o")
                .arg(&obj_path)
                .arg("-O")
                .arg("coff")
                .status()
                .ok()?;
            if status.success() {
                return Some(obj_path);
            }
        }
    }
    None
}

fn find_msvc_rc(sdk_arch: &str) -> Option<PathBuf> {
    if command_exists("rc") {
        return Some(PathBuf::from("rc"));
    }

    let roots = [
        env::var_os("ProgramFiles(x86)").map(PathBuf::from),
        env::var_os("ProgramFiles").map(PathBuf::from),
    ];

    let mut candidates = Vec::new();
    for root in roots.into_iter().flatten() {
        let bin_dir = root.join("Windows Kits").join("10").join("bin");
        let Ok(entries) = fs::read_dir(&bin_dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let rc = entry.path().join(sdk_arch).join("rc.exe");
            if rc.is_file() {
                candidates.push(rc);
            }
        }
    }

    // Prefer the highest Windows SDK version directory.
    candidates.sort();
    candidates.pop()
}

fn command_exists(name: &str) -> bool {
    Command::new(name)
        .arg("/?")
        .output()
        .or_else(|_| Command::new(name).arg("--version").output())
        .is_ok()
}
