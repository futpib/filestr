plugins {
    id("com.android.application")
    // The Flutter Gradle Plugin must be applied after the Android and Kotlin Gradle plugins.
    id("dev.flutter.flutter-gradle-plugin")
}

android {
    namespace = "com.filestr.filestr_app"
    compileSdk = flutter.compileSdkVersion
    ndkVersion = flutter.ndkVersion

    compileOptions {
        sourceCompatibility = JavaVersion.VERSION_17
        targetCompatibility = JavaVersion.VERSION_17
    }

    defaultConfig {
        // TODO: Specify your own unique Application ID (https://developer.android.com/studio/build/application-id.html).
        applicationId = "com.filestr.filestr_app"
        // You can update the following values to match your application needs.
        // For more information, see: https://flutter.dev/to/review-gradle-config.
        minSdk = maxOf(flutter.minSdkVersion, 24)
        targetSdk = flutter.targetSdkVersion
        versionCode = flutter.versionCode
        versionName = flutter.versionName

        // We bundle the filestrd daemon as libfilestrd.so only for these ABIs
        // (see scripts/build-android.sh). Restrict to them so every shipped
        // ABI actually has a daemon.
        ndk {
            abiFilters += listOf("arm64-v8a", "x86_64")
        }
    }

    // The daemon is an executable shipped as libfilestrd.so; legacy packaging
    // unpacks it into nativeLibraryDir with exec permission so we can run it.
    packaging {
        jniLibs {
            useLegacyPackaging = true
        }
    }

    // A release keystore is supplied by CI via the KEYSTORE_PATH env var (decoded
    // from the KEYSTORE_BASE64 repo secret — see .github/workflows/build-apk.yml).
    // Convention (same as iroh-ssh-android): store/key password "android", key
    // alias "release". When absent (local builds, or CI without the secret) the
    // release build falls back to the debug keys so it still installs.
    signingConfigs {
        val keystorePath = System.getenv("KEYSTORE_PATH")
        if (!keystorePath.isNullOrEmpty()) {
            create("release") {
                storeFile = file(keystorePath)
                storePassword = "android"
                keyAlias = "release"
                keyPassword = "android"
            }
        }
    }

    buildTypes {
        release {
            signingConfig = if (signingConfigs.names.contains("release")) {
                signingConfigs.getByName("release")
            } else {
                signingConfigs.getByName("debug")
            }
        }
    }
}

kotlin {
    compilerOptions {
        jvmTarget = org.jetbrains.kotlin.gradle.dsl.JvmTarget.JVM_17
    }
}

flutter {
    source = "../.."
}
