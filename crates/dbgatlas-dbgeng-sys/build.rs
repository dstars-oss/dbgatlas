use std::collections::BTreeSet;
use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

fn main() {
    println!("cargo:rerun-if-changed=../../native/CMakeLists.txt");
    println!("cargo:rerun-if-changed=../../native/include/dbgatlas_native.h");
    println!("cargo:rerun-if-changed=../../native/adapters/dbgeng/dbgatlas_dbgeng.cpp");

    if env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }

    let manifest_dir = PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").unwrap());
    let native_dir = manifest_dir.join("..").join("..").join("native");
    let out_dir = PathBuf::from(env::var_os("OUT_DIR").unwrap());
    let build_dir = out_dir.join("cmake-build");
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
        .arg(format!("-DCMAKE_BUILD_TYPE={build_type}"));
    if env::var_os("CMAKE_GENERATOR").is_none() && command_exists("ninja") {
        configure.arg("-G").arg("Ninja");
    }
    run(configure, "configure native CMake project");

    let mut build = Command::new("cmake");
    build
        .arg("--build")
        .arg(&build_dir)
        .arg("--config")
        .arg(build_type)
        .arg("--target")
        .arg("dbgatlas_dbgeng");
    run(build, "build native CMake project");

    let mut outputs = Vec::new();
    collect_files(&build_dir, &mut outputs);

    let mut link_dirs = BTreeSet::new();
    let mut runtime_dlls = Vec::new();
    for path in outputs {
        let Some(file_name) = path.file_name().and_then(|value| value.to_str()) else {
            continue;
        };
        if !file_name.contains("dbgatlas_dbgeng") {
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
    println!("cargo:rustc-link-lib=dylib=dbgatlas_dbgeng");

    copy_runtime_dlls(&out_dir, &runtime_dlls);
}

fn run(mut command: Command, description: &str) {
    let status = command
        .status()
        .unwrap_or_else(|error| panic!("failed to {description}: {error}"));
    if !status.success() {
        panic!("{description} failed with status {status}");
    }
}

fn command_exists(command: &str) -> bool {
    Command::new(command)
        .arg("--version")
        .output()
        .map(|output| output.status.success())
        .unwrap_or(false)
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
