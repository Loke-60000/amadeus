#[cfg(target_os = "android")]
use std::{
    env, fs,
    path::{Path, PathBuf},
};

#[cfg(target_os = "android")]
const THIRD_PARTY_DIR_NAME: &str = "third_party";
#[cfg(target_os = "android")]
const CUBISM_FRAMEWORK_DIR_NAME: &str = "CubismNativeFramework";
#[cfg(target_os = "android")]
const CUBISM_FRAMEWORK_DIR_ENV: &str = "AMADEUS_CUBISM_FRAMEWORK_DIR";
#[cfg(target_os = "android")]
const CUBISM_CORE_DIR_ENV: &str = "AMADEUS_CUBISM_CORE_DIR";
#[cfg(target_os = "android")]
const SKIP_NATIVE_CUBISM_ENV: &str = "AMADEUS_SKIP_NATIVE_CUBISM";

fn main() {
    println!("cargo:rerun-if-changed=build.rs");

    #[cfg(target_os = "android")]
    build_android_cubism();
}

#[cfg(target_os = "android")]
fn build_android_cubism() {
    println!("cargo:rerun-if-env-changed={SKIP_NATIVE_CUBISM_ENV}");
    println!("cargo:rerun-if-env-changed={CUBISM_FRAMEWORK_DIR_ENV}");
    println!("cargo:rerun-if-env-changed={CUBISM_CORE_DIR_ENV}");

    if env_flag(SKIP_NATIVE_CUBISM_ENV) {
        return;
    }

    let manifest_dir = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap());
    let workspace_root = manifest_dir
        .join("..")
        .join("..")
        .canonicalize()
        .unwrap_or_else(|_| manifest_dir.join("..").join(".."));

    let client_cubism_src = workspace_root
        .join("crates")
        .join("amadeus-client")
        .join("src")
        .join("cubism");

    let android_cubism_src = manifest_dir.join("src").join("cubism");

    let framework_src = resolve_framework_src(&workspace_root, &manifest_dir);
    let core_root = resolve_core_root(&workspace_root, &manifest_dir);

    let abi = env::var("CARGO_CFG_TARGET_ARCH").unwrap_or_default();
    let android_abi = match abi.as_str() {
        "aarch64" => "arm64-v8a",
        "arm" => "armeabi-v7a",
        "x86_64" => "x86_64",
        "x86" => "x86",
        _ => panic!("unsupported Android ABI: {abi}"),
    };

    let core_include = core_root.join("include");
    let core_lib = core_root
        .join("lib")
        .join("android")
        .join(android_abi)
        .join("libLive2DCubismCore.a");

    if !core_lib.exists() {
        panic!(
            "Cubism Core for Android not found at {}. \
             Set {CUBISM_CORE_DIR_ENV} to the Core/ directory from the Android SDK download.",
            core_lib.display()
        );
    }

    println!(
        "cargo:rustc-link-search=native={}",
        core_lib.parent().unwrap().display()
    );
    println!("cargo:rustc-link-lib=static=Live2DCubismCore");
    println!("cargo:rustc-link-lib=EGL");
    println!("cargo:rustc-link-lib=GLESv3");
    println!("cargo:rustc-link-lib=android");
    println!("cargo:rustc-link-lib=log");
    println!("cargo:rustc-link-lib=aaudio");

    let mut build = cc::Build::new();
    build
        .cpp(true)
        .std("c++17")
        .warnings(false)
        .define("CSM_TARGET_ANDROID_ES2", None)
        .flag_if_supported("-Wno-deprecated-declarations")
        .flag_if_supported("-Wno-missing-field-initializers")
        .flag_if_supported("-Wno-unused-parameter")
        .include(&android_cubism_src)
        .include(&client_cubism_src)
        .include(&framework_src)
        .include(&core_include);

    for dir in framework_source_directories(&framework_src) {
        for file in collect_cpp_files(&dir) {
            println!("cargo:rerun-if-changed={}", file.display());
            build.file(file);
        }
    }

    let shared_sources = [
        "CubismSampleViewMatrix_Common.cpp",
        "LAppAllocator_Common.cpp",
        "LAppModel_Common.cpp",
        "LAppTextureManager_Common.cpp",
        "MouseActionManager_Common.cpp",
        "TouchManager_Common.cpp",
        "stb_image.h",
    ];

    for name in &shared_sources {
        if name.ends_with(".cpp") {
            let path = client_cubism_src.join(name);
            println!("cargo:rerun-if-changed={}", path.display());
            build.file(path);
        }
    }

    for name in &[
        "LAppPal.cpp",
        "cubism_bridge_android.cpp",
        "CubismModelAndroid.cpp",
        "LAppTextureManager_Android.cpp",
    ] {
        let path = android_cubism_src.join(name);
        println!("cargo:rerun-if-changed={}", path.display());
        build.file(path);
    }

    build.compile("amadeus_cubism_android");

    let aaudio_src = android_cubism_src.join("aaudio_player.c");
    println!("cargo:rerun-if-changed={}", aaudio_src.display());
    cc::Build::new().file(aaudio_src).compile("amadeus_aaudio");
}

#[cfg(target_os = "android")]
fn resolve_framework_src(workspace_root: &Path, manifest_dir: &Path) -> PathBuf {
    if let Some(dir) = env::var_os(CUBISM_FRAMEWORK_DIR_ENV) {
        let p = normalize_path(manifest_dir, PathBuf::from(dir));
        if p.exists() {
            return p;
        }
        panic!(
            "{CUBISM_FRAMEWORK_DIR_ENV} points to missing dir: {}",
            p.display()
        );
    }

    let tracked = workspace_root
        .join(THIRD_PARTY_DIR_NAME)
        .join(CUBISM_FRAMEWORK_DIR_NAME)
        .join("src");
    if tracked.exists() {
        return tracked;
    }

    panic!(
        "CubismNativeFramework not found at {}. \
         Run: git submodule update --init --recursive",
        tracked.display()
    );
}

#[cfg(target_os = "android")]
fn resolve_core_root(workspace_root: &Path, manifest_dir: &Path) -> PathBuf {
    if let Some(dir) = env::var_os(CUBISM_CORE_DIR_ENV) {
        let p = normalize_path(manifest_dir, PathBuf::from(dir));
        if p.exists() {
            return p;
        }
        panic!(
            "{CUBISM_CORE_DIR_ENV} points to missing dir: {}",
            p.display()
        );
    }

    let sdk_core = workspace_root
        .join("ressource")
        .join("CubismSdkForNative-5-r.4.1")
        .join("Core");
    if sdk_core.exists() {
        return sdk_core;
    }

    panic!(
        "Cubism Core for Android not found. Set {CUBISM_CORE_DIR_ENV} to the Core/ directory \
         from the Live2D Native SDK download."
    );
}

#[cfg(target_os = "android")]
fn normalize_path(base: &Path, candidate: PathBuf) -> PathBuf {
    if candidate.is_absolute() {
        candidate
    } else {
        base.join(candidate)
    }
}

#[cfg(target_os = "android")]
fn framework_source_directories(framework_src: &Path) -> Vec<PathBuf> {
    vec![
        framework_src.to_path_buf(),
        framework_src.join("Effect"),
        framework_src.join("Id"),
        framework_src.join("Math"),
        framework_src.join("Model"),
        framework_src.join("Motion"),
        framework_src.join("Physics"),
        framework_src.join("Rendering"),
        framework_src.join("Rendering").join("OpenGL"),
        framework_src.join("Type"),
        framework_src.join("Utils"),
    ]
}

#[cfg(target_os = "android")]
fn collect_cpp_files(directory: &Path) -> Vec<PathBuf> {
    let Ok(entries) = fs::read_dir(directory) else {
        return vec![];
    };
    let mut files: Vec<PathBuf> = entries
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.extension().and_then(|x| x.to_str()) == Some("cpp"))
        .collect();
    files.sort();
    files
}

#[cfg(target_os = "android")]
fn env_flag(name: &str) -> bool {
    env::var(name)
        .map(|v| {
            matches!(
                v.trim().to_ascii_lowercase().as_str(),
                "1" | "true" | "yes" | "on"
            )
        })
        .unwrap_or(false)
}
