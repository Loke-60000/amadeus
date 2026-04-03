#[cfg(target_os = "linux")]
use std::{
    env, fs,
    path::{Path, PathBuf},
};

#[cfg(target_os = "linux")]
const LOCAL_RESOURCE_DIR_NAME: &str = "ressource";
#[cfg(target_os = "linux")]
const CUBISM_SDK_DIR_NAME: &str = "CubismSdkForNative-5-r.4.1";
#[cfg(target_os = "linux")]
const CUBISM_SDK_DIR_ENV: &str = "AMADEUS_CUBISM_SDK_DIR";
#[cfg(target_os = "linux")]
const SKIP_NATIVE_CUBISM_ENV: &str = "AMADEUS_SKIP_NATIVE_CUBISM";

fn main() {
    println!("cargo:rerun-if-changed=build.rs");

    #[cfg(target_os = "linux")]
    build_linux_native_cubism();
}

#[cfg(target_os = "linux")]
fn build_linux_native_cubism() {
    println!("cargo:rerun-if-env-changed={SKIP_NATIVE_CUBISM_ENV}");
    if env_flag(SKIP_NATIVE_CUBISM_ENV) {
        println!(
            "cargo:warning=Skipping native Cubism build because {SKIP_NATIVE_CUBISM_ENV} is enabled"
        );
        return;
    }

    let manifest_dir = PathBuf::from(
        env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR should be available"),
    );
    let native_root = manifest_dir.join("src").join("core").join("native");
    let native_cpp_src = native_root.join("cpp");
    println!("cargo:rerun-if-env-changed={CUBISM_SDK_DIR_ENV}");
    println!(
        "cargo:rerun-if-changed={}",
        manifest_dir.join(LOCAL_RESOURCE_DIR_NAME).display()
    );
    println!(
        "cargo:rerun-if-changed={}",
        manifest_dir.join(CUBISM_SDK_DIR_NAME).display()
    );

    let sdk_root = resolve_cubism_sdk_root(&manifest_dir);
    let framework_src = sdk_root.join("Framework").join("src");
    let common_src = sdk_root.join("Samples").join("Common");
    let sample_linux_src = sdk_root
        .join("Samples")
        .join("OpenGL")
        .join("Demo")
        .join("proj.linux.cmake")
        .join("src");
    let stb_src = sdk_root
        .join("Samples")
        .join("OpenGL")
        .join("thirdParty")
        .join("stb");
    let bridge_src = native_cpp_src.join("cubism_bridge.cpp");
    let overlay_src = native_cpp_src.join("amadeus_overlay.cpp");
    let overlay_header = native_cpp_src.join("amadeus_overlay.hpp");
    let text_renderer_src = native_cpp_src.join("amadeus_text_renderer.cpp");
    let text_renderer_header = native_cpp_src.join("amadeus_text_renderer.hpp");
    let core_lib = sdk_root
        .join("Core")
        .join("lib")
        .join("linux")
        .join("x86_64")
        .join("libLive2DCubismCore.a");
    let core_include = sdk_root.join("Core").join("include");

    for path in [
        &sdk_root,
        &framework_src,
        &common_src,
        &sample_linux_src,
        &stb_src,
        &native_root,
        &native_cpp_src,
        &bridge_src,
        &overlay_src,
        &overlay_header,
        &text_renderer_src,
        &text_renderer_header,
        &core_lib,
        &core_include,
    ] {
        if !path.exists() {
            panic!("required native Cubism path is missing: {}", path.display());
        }
        println!("cargo:rerun-if-changed={}", path.display());
    }

    let glfw = pkg_config::Config::new()
        .probe("glfw3")
        .expect("glfw3 development files are required for the native Cubism viewer");
    let glew = pkg_config::Config::new()
        .probe("glew")
        .expect("glew development files are required for the native Cubism viewer");
    let freetype = pkg_config::Config::new()
        .probe("freetype2")
        .expect("freetype2 development files are required for the native overlay text renderer");
    let fontconfig = pkg_config::Config::new()
        .probe("fontconfig")
        .expect("fontconfig development files are required for the native overlay text renderer");

    println!(
        "cargo:rustc-link-search=native={}",
        core_lib
            .parent()
            .expect("Cubism Core library should have a parent directory")
            .display()
    );
    println!("cargo:rustc-link-lib=static=Live2DCubismCore");

    let mut build = cc::Build::new();
    build
        .cpp(true)
        .std("c++17")
        .warnings(false)
        .define("CSM_TARGET_LINUX_GL", None)
        .flag_if_supported("-Wno-deprecated-declarations")
        .flag_if_supported("-Wno-missing-field-initializers")
        .flag_if_supported("-Wno-unused-parameter")
        .include(&native_cpp_src)
        .include(&framework_src)
        .include(&core_include)
        .include(&common_src)
        .include(&sample_linux_src)
        .include(&stb_src);

    for include_path in glfw
        .include_paths
        .iter()
        .chain(glew.include_paths.iter())
        .chain(freetype.include_paths.iter())
        .chain(fontconfig.include_paths.iter())
    {
        build.include(include_path);
    }

    for directory in framework_source_directories(&framework_src) {
        for source_file in collect_cpp_files(&directory) {
            println!("cargo:rerun-if-changed={}", source_file.display());
            build.file(source_file);
        }
    }

    for source_file in [
        common_src.join("CubismSampleViewMatrix_Common.cpp"),
        common_src.join("LAppAllocator_Common.cpp"),
        common_src.join("LAppModel_Common.cpp"),
        common_src.join("LAppTextureManager_Common.cpp"),
        common_src.join("MouseActionManager_Common.cpp"),
        common_src.join("TouchManager_Common.cpp"),
        sample_linux_src.join("LAppDefine.cpp"),
        sample_linux_src.join("LAppPal.cpp"),
        sample_linux_src.join("LAppTextureManager.cpp"),
        sample_linux_src.join("CubismUserModelExtend.cpp"),
        sample_linux_src.join("MouseActionManager.cpp"),
        overlay_src,
        text_renderer_src,
        bridge_src,
    ] {
        println!("cargo:rerun-if-changed={}", source_file.display());
        build.file(source_file);
    }

    build.compile("amadeus_cubism_native");
}

#[cfg(target_os = "linux")]
fn resolve_cubism_sdk_root(manifest_dir: &Path) -> PathBuf {
    if let Some(override_dir) = env::var_os(CUBISM_SDK_DIR_ENV) {
        let override_dir = normalize_resource_path(manifest_dir, PathBuf::from(override_dir));
        if override_dir.exists() {
            return override_dir;
        }

        panic!(
            "{CUBISM_SDK_DIR_ENV} points to a missing Cubism SDK: {}",
            override_dir.display()
        );
    }

    let preferred_dir = manifest_dir
        .join(LOCAL_RESOURCE_DIR_NAME)
        .join(CUBISM_SDK_DIR_NAME);
    if preferred_dir.exists() {
        return preferred_dir;
    }

    let legacy_dir = manifest_dir.join(CUBISM_SDK_DIR_NAME);
    if legacy_dir.exists() {
        return legacy_dir;
    }

    panic!(
        "Cubism SDK not found. Expected {} or {}",
        preferred_dir.display(),
        legacy_dir.display()
    );
}

#[cfg(target_os = "linux")]
fn normalize_resource_path(manifest_dir: &Path, candidate: PathBuf) -> PathBuf {
    if candidate.is_absolute() {
        candidate
    } else {
        manifest_dir.join(candidate)
    }
}

#[cfg(target_os = "linux")]
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

#[cfg(target_os = "linux")]
fn collect_cpp_files(directory: &Path) -> Vec<PathBuf> {
    let mut files = fs::read_dir(directory)
        .unwrap_or_else(|error| panic!("failed to read {}: {error}", directory.display()))
        .filter_map(|entry| {
            let entry = entry.ok()?;
            let path = entry.path();
            (path.extension().and_then(|extension| extension.to_str()) == Some("cpp"))
                .then_some(path)
        })
        .collect::<Vec<_>>();
    files.sort();
    files
}

#[cfg(target_os = "linux")]
fn env_flag(name: &str) -> bool {
    env::var(name)
        .map(|value| {
            matches!(
                value.trim().to_ascii_lowercase().as_str(),
                "1" | "true" | "yes" | "on"
            )
        })
        .unwrap_or(false)
}
