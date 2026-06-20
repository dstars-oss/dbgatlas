use std::collections::BTreeSet;
use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

fn main() {
    let manifest_dir = PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").unwrap());
    let repo_dir = manifest_dir.join("..").join("..");
    let native_dir = repo_dir.join("native");
    let ida_sdk_include_dir = repo_dir
        .join("3rdpart")
        .join("cpp")
        .join("ida-sdk")
        .join("include");

    println!(
        "cargo:rerun-if-changed={}",
        native_dir.join("CMakeLists.txt").display()
    );
    println!(
        "cargo:rerun-if-changed={}",
        native_dir.join("include").join("dbgatlas_ida.h").display()
    );
    println!(
        "cargo:rerun-if-changed={}",
        native_dir
            .join("adapters")
            .join("ida")
            .join("dbgatlas_ida.cpp")
            .display()
    );
    println!(
        "cargo:rerun-if-changed={}",
        native_dir
            .join("adapters")
            .join("ida")
            .join("dbgatlas_ida_runtime.cpp")
            .display()
    );
    println!(
        "cargo:rerun-if-changed={}",
        native_dir
            .join("adapters")
            .join("ida")
            .join("dbgatlas_ida_runtime.h")
            .display()
    );
    emit_rerun_if_changed_dir(&ida_sdk_include_dir);

    if env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }
    ensure_msvc_target();

    let out_dir = PathBuf::from(env::var_os("OUT_DIR").unwrap());
    let generator = visual_studio_generator();
    let build_dir = out_dir.join(match generator {
        "Visual Studio 18 2026" => "cmake-build-msvc-vs18",
        "Visual Studio 17 2022" => "cmake-build-msvc-vs17",
        _ => "cmake-build-msvc",
    });
    let profile = env::var("PROFILE").unwrap_or_else(|_| "debug".to_string());
    let build_type = if profile == "release" {
        "Release"
    } else {
        "Debug"
    };
    fs::create_dir_all(&build_dir).expect("failed to create CMake build directory");

    let mut configure = Command::new("cmake");
    configure
        .arg("-S")
        .arg(&native_dir)
        .arg("-B")
        .arg(&build_dir)
        .arg("-G")
        .arg(generator)
        .arg("-A")
        .arg(msvc_arch());
    run(configure, "configure native CMake project");

    let mut build = Command::new("cmake");
    build
        .arg("--build")
        .arg(&build_dir)
        .arg("--config")
        .arg(build_type)
        .arg("--target")
        .arg("dbgatlas_ida");
    run(build, "build native IDA adapter");

    let mut outputs = Vec::new();
    collect_files(&build_dir, &mut outputs);

    let mut link_dirs = BTreeSet::new();
    let mut runtime_dlls = Vec::new();
    for path in outputs {
        let Some(file_name) = path.file_name().and_then(|value| value.to_str()) else {
            continue;
        };
        if !file_name.contains("dbgatlas_ida") {
            continue;
        }
        if file_name.ends_with(".dll") {
            runtime_dlls.push(path.clone());
        }
        if file_name.ends_with(".lib") || file_name.ends_with(".dll.a") || file_name.ends_with(".a")
        {
            if let Some(parent) = path.parent() {
                link_dirs.insert(parent.to_path_buf());
            }
        }
    }

    for dir in &link_dirs {
        println!("cargo:rustc-link-search=native={}", dir.display());
    }
    copy_runtime_dlls(&out_dir, &runtime_dlls);
}

fn emit_rerun_if_changed_dir(dir: &Path) {
    println!("cargo:rerun-if-changed={}", dir.display());
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            emit_rerun_if_changed_dir(&path);
        } else {
            println!("cargo:rerun-if-changed={}", path.display());
        }
    }
}

fn ensure_msvc_target() {
    let target = env::var("TARGET").unwrap_or_else(|_| "unknown-target".to_string());
    let target_env = env::var("CARGO_CFG_TARGET_ENV").unwrap_or_default();
    if target_env != "msvc" && !target.ends_with("-msvc") {
        panic!("dbgatlas native adapters must be built for a Windows MSVC target; got `{target}`");
    }
}

fn run(mut command: Command, description: &str) {
    let status = command
        .status()
        .unwrap_or_else(|error| panic!("failed to {description}: {error}"));
    if !status.success() {
        panic!("{description} failed with status {status}");
    }
}

fn msvc_arch() -> &'static str {
    match env::var("CARGO_CFG_TARGET_ARCH").as_deref() {
        Ok("x86") => "Win32",
        Ok("aarch64") => "ARM64",
        _ => "x64",
    }
}

fn visual_studio_generator() -> &'static str {
    let help = Command::new("cmake")
        .arg("--help")
        .output()
        .ok()
        .and_then(|output| String::from_utf8(output.stdout).ok())
        .unwrap_or_default();
    if help.contains("Visual Studio 18 2026") {
        "Visual Studio 18 2026"
    } else {
        "Visual Studio 17 2022"
    }
}

fn collect_files(dir: &Path, files: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_files(&path, files);
        } else {
            files.push(path);
        }
    }
}

fn copy_runtime_dlls(out_dir: &Path, dlls: &[PathBuf]) {
    let Some(profile_dir) = out_dir.ancestors().nth(3).map(Path::to_path_buf) else {
        return;
    };
    let deps_dir = profile_dir.join("deps");
    fs::create_dir_all(&deps_dir).expect("failed to create target deps directory");

    for dll in dlls {
        let Some(file_name) = dll.file_name() else {
            continue;
        };
        let target = profile_dir.join(file_name);
        fs::copy(dll, &target).expect("failed to copy native DLL to target profile directory");
        let deps_target = deps_dir.join(file_name);
        fs::copy(dll, deps_target).expect("failed to copy native DLL to target deps directory");
    }
}
