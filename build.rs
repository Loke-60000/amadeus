#[cfg(target_os = "linux")]
use std::{
    env, fs,
    path::{Path, PathBuf},
};

#[cfg(target_os = "linux")]
const THIRD_PARTY_DIR_NAME: &str = "third_party";
#[cfg(target_os = "linux")]
const CUBISM_FRAMEWORK_DIR_NAME: &str = "CubismNativeFramework";
#[cfg(target_os = "linux")]
const LOCAL_RESOURCE_DIR_NAME: &str = "ressource";
#[cfg(target_os = "linux")]
const CUBISM_SDK_DIR_NAME: &str = "CubismSdkForNative-5-r.4.1";
#[cfg(target_os = "linux")]
const CUBISM_FRAMEWORK_DIR_ENV: &str = "AMADEUS_CUBISM_FRAMEWORK_DIR";
#[cfg(target_os = "linux")]
const CUBISM_CORE_DIR_ENV: &str = "AMADEUS_CUBISM_CORE_DIR";
#[cfg(target_os = "linux")]
const CUBISM_SDK_DIR_ENV: &str = "AMADEUS_CUBISM_SDK_DIR";
#[cfg(target_os = "linux")]
const SKIP_NATIVE_CUBISM_ENV: &str = "AMADEUS_SKIP_NATIVE_CUBISM";

#[cfg(target_os = "linux")]
struct CubismPaths {
    framework_src: PathBuf,
    core_root: PathBuf,
}

fn main() {
    println!("cargo:rerun-if-changed=build.rs");

    #[cfg(target_os = "linux")]
    build_linux_native_cubism();

    #[cfg(target_os = "linux")]
    build_linux_local_llm_if_enabled();
}

#[cfg(target_os = "linux")]
fn build_linux_native_cubism() {
    println!("cargo:rerun-if-env-changed={SKIP_NATIVE_CUBISM_ENV}");
    println!("cargo:rerun-if-env-changed={CUBISM_FRAMEWORK_DIR_ENV}");
    println!("cargo:rerun-if-env-changed={CUBISM_CORE_DIR_ENV}");
    println!("cargo:rerun-if-env-changed={CUBISM_SDK_DIR_ENV}");
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
    println!(
        "cargo:rerun-if-changed={}",
        manifest_dir.join(THIRD_PARTY_DIR_NAME).display()
    );
    println!(
        "cargo:rerun-if-changed={}",
        manifest_dir.join(LOCAL_RESOURCE_DIR_NAME).display()
    );
    println!(
        "cargo:rerun-if-changed={}",
        manifest_dir.join(CUBISM_SDK_DIR_NAME).display()
    );

    let cubism_paths = resolve_cubism_paths(&manifest_dir);
    let framework_src = cubism_paths.framework_src.clone();
    let bridge_src = native_cpp_src.join("cubism_bridge.cpp");
    let overlay_src = native_cpp_src.join("overlay.cpp");
    let overlay_draw_src = native_cpp_src.join("overlay_draw.cpp");
    let overlay_header = native_cpp_src.join("overlay.hpp");
    let text_renderer_src = native_cpp_src.join("font_renderer.cpp");
    let text_renderer_header = native_cpp_src.join("font_renderer.hpp");
    let core_lib = cubism_paths
        .core_root
        .join("lib")
        .join("linux")
        .join("x86_64")
        .join("libLive2DCubismCore.a");
    let core_include = cubism_paths.core_root.join("include");

    for path in [
        &framework_src,
        &cubism_paths.core_root,
        &native_root,
        &native_cpp_src,
        &bridge_src,
        &overlay_src,
        &overlay_draw_src,
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
        .include(&core_include);

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
        native_cpp_src.join("boot_sequence.cpp"),
        native_cpp_src.join("CubismSampleViewMatrix_Common.cpp"),
        native_cpp_src.join("LAppAllocator_Common.cpp"),
        native_cpp_src.join("LAppModel_Common.cpp"),
        native_cpp_src.join("LAppTextureManager_Common.cpp"),
        native_cpp_src.join("MouseActionManager_Common.cpp"),
        native_cpp_src.join("TouchManager_Common.cpp"),
        native_cpp_src.join("LAppDefine.cpp"),
        native_cpp_src.join("LAppPal.cpp"),
        native_cpp_src.join("LAppTextureManager.cpp"),
        native_cpp_src.join("CubismUserModelExtend.cpp"),
        native_cpp_src.join("MouseActionManager.cpp"),
        overlay_src,
        overlay_draw_src,
        text_renderer_src,
        bridge_src,
    ] {
        println!("cargo:rerun-if-changed={}", source_file.display());
        build.file(source_file);
    }

    build.compile("amadeus_cubism_native");
}

#[cfg(target_os = "linux")]
fn resolve_cubism_paths(manifest_dir: &Path) -> CubismPaths {
    if let Some(override_dir) = env::var_os(CUBISM_SDK_DIR_ENV) {
        let sdk_root = normalize_resource_path(manifest_dir, PathBuf::from(override_dir));
        if sdk_root.exists() {
            return CubismPaths {
                framework_src: sdk_root.join("Framework").join("src"),
                core_root: sdk_root.join("Core"),
            };
        }

        panic!(
            "{CUBISM_SDK_DIR_ENV} points to a missing Cubism SDK: {}",
            sdk_root.display()
        );
    }

    let framework_src = resolve_cubism_framework_src(manifest_dir);
    let core_root = resolve_cubism_core_root(manifest_dir, &framework_src);
    CubismPaths {
        framework_src,
        core_root,
    }
}

#[cfg(target_os = "linux")]
fn resolve_cubism_framework_src(manifest_dir: &Path) -> PathBuf {
    if let Some(override_dir) = env::var_os(CUBISM_FRAMEWORK_DIR_ENV) {
        let override_dir = normalize_resource_path(manifest_dir, PathBuf::from(override_dir));
        if override_dir.exists() {
            return override_dir;
        }

        panic!(
            "{CUBISM_FRAMEWORK_DIR_ENV} points to a missing Cubism Framework directory: {}",
            override_dir.display()
        );
    }

    let tracked_dir = manifest_dir
        .join(THIRD_PARTY_DIR_NAME)
        .join(CUBISM_FRAMEWORK_DIR_NAME)
        .join("src");
    if tracked_dir.exists() {
        return tracked_dir;
    }

    let preferred_dir = manifest_dir
        .join(LOCAL_RESOURCE_DIR_NAME)
        .join(CUBISM_SDK_DIR_NAME)
        .join("Framework")
        .join("src");
    if preferred_dir.exists() {
        return preferred_dir;
    }

    let legacy_dir = manifest_dir
        .join(CUBISM_SDK_DIR_NAME)
        .join("Framework")
        .join("src");
    if legacy_dir.exists() {
        return legacy_dir;
    }

    panic!(
        "Cubism Framework not found. Expected {tracked}, {preferred}, or {legacy}",
        tracked = tracked_dir.display(),
        preferred = preferred_dir.display(),
        legacy = legacy_dir.display()
    );
}

#[cfg(target_os = "linux")]
fn resolve_cubism_core_root(manifest_dir: &Path, _adjacent_hint: &Path) -> PathBuf {
    if let Some(override_dir) = env::var_os(CUBISM_CORE_DIR_ENV) {
        let override_dir = normalize_resource_path(manifest_dir, PathBuf::from(override_dir));
        if cubism_core_is_available(&override_dir) {
            return override_dir;
        }

        panic!(
            "{CUBISM_CORE_DIR_ENV} points to a Cubism Core directory missing include/ or lib/: {}",
            override_dir.display()
        );
    }

    let local_core_dir = manifest_dir.join("Core");
    if cubism_core_is_available(&local_core_dir) {
        return local_core_dir;
    }

    let preferred_dir = manifest_dir
        .join(LOCAL_RESOURCE_DIR_NAME)
        .join(CUBISM_SDK_DIR_NAME)
        .join("Core");
    if cubism_core_is_available(&preferred_dir) {
        return preferred_dir;
    }

    let legacy_dir = manifest_dir.join(CUBISM_SDK_DIR_NAME).join("Core");
    if cubism_core_is_available(&legacy_dir) {
        return legacy_dir;
    }

    panic!(
        "Cubism Core not found. Set {CUBISM_CORE_DIR_ENV} to the private Core directory (the `Core/` directory from the Cubism SDK download)."
    );
}

#[cfg(target_os = "linux")]
fn cubism_core_is_available(core_root: &Path) -> bool {
    core_root.join("include").is_dir()
        && core_root
            .join("lib")
            .join("linux")
            .join("x86_64")
            .join("libLive2DCubismCore.a")
            .is_file()
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

#[cfg(target_os = "linux")]
fn build_linux_local_llm_if_enabled() {
    println!("cargo:rerun-if-env-changed=CARGO_FEATURE_LOCAL_LLM");
    if env::var("CARGO_FEATURE_LOCAL_LLM").is_err() {
        return;
    }

    let manifest_dir = PathBuf::from(
        env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR should be available"),
    );
    let llama_dir = manifest_dir.join(THIRD_PARTY_DIR_NAME).join("llama.cpp");
    let bridge_dir = manifest_dir
        .join("src")
        .join("agent")
        .join("llm")
        .join("cpp");

    println!("cargo:rerun-if-changed={}", llama_dir.display());
    println!("cargo:rerun-if-changed={}", bridge_dir.display());

    if !llama_dir.exists() {
        panic!(
            "llama.cpp submodule not found at {}. Run: git submodule update --init third_party/llama.cpp",
            llama_dir.display()
        );
    }

    let llm_vulkan = env::var("CARGO_FEATURE_LLM_VULKAN").is_ok();
    let llm_cuda   = env::var("CARGO_FEATURE_LLM_CUDA").is_ok();

    println!("cargo:rerun-if-env-changed=CARGO_FEATURE_LLM_VULKAN");
    println!("cargo:rerun-if-env-changed=CARGO_FEATURE_LLM_CUDA");

    if llm_vulkan {
        println!("cargo:warning=llama.cpp: building with Vulkan backend");
    } else if llm_cuda {
        println!("cargo:warning=llama.cpp: building with CUDA backend");
    } else {
        println!("cargo:warning=llama.cpp: building CPU-only (use --features llm-vulkan or llm-cuda for GPU)");
    }

    // Build llama.cpp via cmake (static; GPU backend selected by feature flag).
    let mut cmake_cfg = cmake::Config::new(&llama_dir);
    cmake_cfg
        .define("BUILD_SHARED_LIBS", "OFF")
        .define("LLAMA_BUILD_TESTS", "OFF")
        .define("LLAMA_BUILD_TOOLS", "OFF")
        .define("LLAMA_BUILD_EXAMPLES", "OFF")
        .define("LLAMA_BUILD_SERVER", "OFF")
        .define("LLAMA_BUILD_COMMON", "OFF")
        .define("GGML_CUDA",    if llm_cuda { "ON" } else { "OFF" })
        .define("GGML_VULKAN",  if llm_vulkan { "ON" } else { "OFF" })
        .define("GGML_METAL",   "OFF")
        .define("GGML_OPENSSL", "OFF");

    // Point cmake at the CUDA toolkit so it can find nvcc and the libraries.
    if llm_cuda {
        if let Ok(cuda_path) = env::var("CUDA_PATH") {
            cmake_cfg.define("CUDAToolkit_ROOT", &cuda_path);
            cmake_cfg.define("CMAKE_CUDA_COMPILER", format!("{cuda_path}/bin/nvcc"));
        }
    }

    let dst = cmake_cfg.build();

    // cmake install doesn't place static .a files in the install prefix's lib/
    // directory for LIBRARY targets, so we search the build directory recursively.
    let build_dir = dst.join("build");
    for lib_path in collect_static_libs(&build_dir) {
        let dir = lib_path
            .parent()
            .expect("static lib must have a parent directory");
        let stem = lib_path
            .file_stem()
            .and_then(|s| s.to_str())
            .expect("static lib must have a valid filename");
        // Strip "lib" prefix that the linker expects us to omit.
        let name = stem.strip_prefix("lib").unwrap_or(stem);
        println!("cargo:rustc-link-search=native={}", dir.display());
        println!("cargo:rustc-link-lib=static={name}");
    }

    // C++ runtime required for linking the bridge and llama.cpp internals.
    println!("cargo:rustc-link-lib=stdc++");
    // llama.cpp's ggml-cpu uses OpenMP; link the GNU OpenMP runtime.
    println!("cargo:rustc-link-lib=gomp");

    // When CUDA is enabled, the ggml-cuda static backend needs the CUDA runtime,
    // driver stub, and cuBLAS (used for matrix ops in llama.cpp).
    if llm_cuda {
        // CUDA toolkit libs (cudart, cublas, cublasLt).
        let cuda_lib_dir = env::var("CUDA_PATH")
            .map(|p| format!("{p}/lib64"))
            .unwrap_or_else(|_| "/usr/local/cuda/lib64".to_string());
        println!("cargo:rustc-link-search=native={cuda_lib_dir}");
        println!("cargo:rustc-link-lib=cudart");
        println!("cargo:rustc-link-lib=cublas");
        println!("cargo:rustc-link-lib=cublasLt");

        // CUDA driver stub (libcuda.so) — lives in the system lib, not the toolkit.
        for driver_dir in &["/usr/lib64", "/usr/lib/x86_64-linux-gnu", "/usr/local/cuda/lib64/stubs"] {
            if std::path::Path::new(driver_dir).join("libcuda.so").exists()
                || std::path::Path::new(driver_dir).join("libcuda.so.1").exists()
            {
                println!("cargo:rustc-link-search=native={driver_dir}");
                break;
            }
        }
        println!("cargo:rustc-link-lib=cuda");

        // cuBLAS device-side library (needed for the cuBLAS backend in ggml).
        println!("cargo:rustc-link-lib=dl");
    }

    // Compile the thin C bridge that wraps the llama.cpp C++ API.
    cc::Build::new()
        .cpp(true)
        .std("c++17")
        .warnings(false)
        .flag_if_supported("-Wno-deprecated-declarations")
        .flag_if_supported("-Wno-unused-parameter")
        .include(&bridge_dir)
        .include(llama_dir.join("include"))
        .include(llama_dir.join("ggml").join("include"))
        .file(bridge_dir.join("llama_bridge.cpp"))
        .compile("amadeus_llm_bridge");
}

/// Recursively collect all `.a` files under `dir`.
#[cfg(target_os = "linux")]
fn collect_static_libs(dir: &Path) -> Vec<PathBuf> {
    let mut libs = Vec::new();
    let entries = match fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return libs,
    };
    for entry in entries.filter_map(|e| e.ok()) {
        let path = entry.path();
        if path.is_dir() {
            libs.extend(collect_static_libs(&path));
        } else if path.extension().and_then(|e| e.to_str()) == Some("a") {
            libs.push(path);
        }
    }
    libs
}
