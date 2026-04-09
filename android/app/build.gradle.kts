plugins {
    alias(libs.plugins.android.application)
}

android {
    namespace = "dev.amadeus.app"
    compileSdk = 35
    ndkVersion = "26.3.11579264"

    defaultConfig {
        applicationId = "dev.amadeus.app"
        minSdk = 26
        targetSdk = 35
        versionCode = 1
        versionName = "1.0"

        ndk {
            abiFilters += listOf("arm64-v8a", "x86_64")
        }

        externalNativeBuild {
            cmake {
                cppFlags += "-std=c++17"
                arguments += listOf(
                    "-DANDROID_STL=c++_shared",
                    "-DANDROID_ARM_NEON=TRUE"
                )
            }
        }
    }

    buildTypes {
        release {
            isMinifyEnabled = false
        }
    }

    externalNativeBuild {
        cmake {
            path = file("src/main/cpp/CMakeLists.txt")
            version = "3.22.1"
        }
    }

    sourceSets {
        getByName("main") {
            assets.srcDirs("src/main/assets")
        }
    }

    compileOptions {
        sourceCompatibility = JavaVersion.VERSION_11
        targetCompatibility = JavaVersion.VERSION_11
    }
}

val syncModelAssets by tasks.registering(Copy::class) {
    val workspaceRoot = rootProject.projectDir.parentFile
    from(workspaceRoot.resolve("assets/model"))
    into(projectDir.resolve("src/main/assets/model"))
}

tasks.named("preBuild") {
    dependsOn(syncModelAssets)
}

dependencies {
    implementation("androidx.core:core:1.13.1")
}
