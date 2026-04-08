#[cfg(target_os = "linux")]
use std::{
    env, fs,
    path::{Path, PathBuf},
};

#[cfg(target_os = "linux")]
const THIRD_PARTY_DIR_NAME: &str = "third_party";

fn main() {
    println!("cargo:rerun-if-changed=build.rs");

    #[cfg(target_os = "linux")]
    build_linux_local_llm_if_enabled();
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
    // amadeus-backend lives at crates/amadeus-backend — workspace root is two levels up.
    let workspace_root = manifest_dir.join("..").join("..").canonicalize()
        .unwrap_or_else(|_| manifest_dir.join("..").join(".."));

    let llama_dir = workspace_root.join(THIRD_PARTY_DIR_NAME).join("llama.cpp");
    let bridge_dir = manifest_dir.join("src").join("llm").join("cpp");

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

    if llm_cuda {
        if let Ok(cuda_path) = env::var("CUDA_PATH") {
            cmake_cfg.define("CUDAToolkit_ROOT", &cuda_path);
            cmake_cfg.define("CMAKE_CUDA_COMPILER", format!("{cuda_path}/bin/nvcc"));
        }
    }

    let dst = cmake_cfg.build();

    let build_dir = dst.join("build");
    for lib_path in collect_static_libs(&build_dir) {
        let dir = lib_path
            .parent()
            .expect("static lib must have a parent directory");
        let stem = lib_path
            .file_stem()
            .and_then(|s| s.to_str())
            .expect("static lib must have a valid filename");
        let name = stem.strip_prefix("lib").unwrap_or(stem);
        println!("cargo:rustc-link-search=native={}", dir.display());
        println!("cargo:rustc-link-lib=static={name}");
    }

    println!("cargo:rustc-link-lib=stdc++");
    println!("cargo:rustc-link-lib=gomp");

    if llm_cuda {
        let cuda_lib_dir = env::var("CUDA_PATH")
            .map(|p| format!("{p}/lib64"))
            .unwrap_or_else(|_| "/usr/local/cuda/lib64".to_string());
        println!("cargo:rustc-link-search=native={cuda_lib_dir}");
        println!("cargo:rustc-link-lib=cudart");
        println!("cargo:rustc-link-lib=cublas");
        println!("cargo:rustc-link-lib=cublasLt");

        for driver_dir in &["/usr/lib64", "/usr/lib/x86_64-linux-gnu", "/usr/local/cuda/lib64/stubs"] {
            if std::path::Path::new(driver_dir).join("libcuda.so").exists()
                || std::path::Path::new(driver_dir).join("libcuda.so.1").exists()
            {
                println!("cargo:rustc-link-search=native={driver_dir}");
                break;
            }
        }
        println!("cargo:rustc-link-lib=cuda");
        println!("cargo:rustc-link-lib=dl");
    }

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
